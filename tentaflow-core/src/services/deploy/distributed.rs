// =============================================================================
// Plik: services/deploy/distributed.rs
// Opis: Per-node distributed (multi-node tensor-parallel) deploy — buduje
//       komendy `ray start ... && vllm serve ...` (z pelnym shell-quotingiem),
//       NCCL/RoCE env i `user_config` sterujacy istniejacym potokiem `deploy()`
//       (docker host-network). Dostarcza tez preflight portow/stale-Ray oraz
//       realny pomiar gotowosci (GCS Ray + czlonkostwo + endpoint OpenAI).
//       Teardown po `deployment_cluster_id` propaguje bledy (brak osieroconych
//       kontenerow).
// =============================================================================

use serde_json::{json, Map, Value};
use tentaflow_protocol::mesh::DistributedDeploySpec;

/// Deterministyczna nazwa kontenera distributed-czlonka — identyczna konwencja
/// co single-node (`tentaflow-<engine>-<port>`), zeby `deploy::stop()` mogl ja
/// odtworzyc z `engine_id` + `runtime_port` przy teardownie/shutdownie.
pub fn container_name(engine_id: &str, port: u16) -> String {
    format!("tentaflow-{}-{}", engine_id, port)
}

/// Czy wiersz serwisu jest czlonkiem distributed-deploymentu (config_json niesie
/// blok `_distributed`). Takich wierszy NIE wolno kasowac pojedynczo z listy
/// serwisow (lokalnie ani przez mesh `ServiceDeleteRemote`) — usuniecie workera
/// zabija rank calego klastra TP, ktory serwuje na INNYM nodzie. Jedyna legalna
/// sciezka usuniecia to stop deploymentu klastra (`stop_distributed`), ktora
/// kasuje wiersze bezposrednio w repo, z pominieciem tego guardu.
pub fn service_is_distributed_member(config_json: &str) -> bool {
    serde_json::from_str::<Value>(config_json)
        .ok()
        .and_then(|v| v.get("_distributed").cloned())
        .is_some()
}

/// `deployment_cluster_id` z bloku `_distributed` wiersza-czlonka (None gdy
/// wiersz nie jest czlonkiem albo blok nie niesie id).
fn member_deployment_id(config_json: &str) -> Option<String> {
    serde_json::from_str::<Value>(config_json)
        .ok()?
        .get("_distributed")?
        .get("deployment_cluster_id")?
        .as_str()
        .map(String::from)
}

/// Czy deployment wiersza-czlonka jest AKTYWNY z perspektywy TEGO noda: lokalny
/// kontener z etykieta deploymentu nadal istnieje LUB (na koordynatorze) rekord
/// `cluster_deployments` jest deploying/running. Osierocony wiersz po deployu,
/// ktorego nikt juz nie zna (rekord skasowany, kontener zniknal), zwraca false —
/// wtedy delete-guard przepuszcza sprzatanie z listy serwisow. Brak id w config
/// = fail-closed (true), zeby uszkodzony wiersz zywego klastra nie byl kasowalny.
pub async fn distributed_member_deployment_active(
    db: &crate::db::DbPool,
    config_json: &str,
) -> bool {
    let Some(did) = member_deployment_id(config_json) else {
        return true;
    };
    #[cfg(feature = "docker")]
    {
        if let Ok(out) = tokio::process::Command::new("docker")
            .args([
                "ps",
                "-q",
                "--filter",
                &format!("label={}={}", super::docker::DISTRIBUTED_LABEL, did),
            ])
            .output()
            .await
        {
            if !String::from_utf8_lossy(&out.stdout).trim().is_empty() {
                return true;
            }
        }
    }
    crate::db::repository::get_cluster_deployment(db, &did)
        .ok()
        .flatten()
        .map(|d| matches!(d.status.as_str(), "deploying" | "running"))
        .unwrap_or(false)
}

/// Informacyjny endpoint head-a (`http://<rdma_ip>:<port>/v1`). Workery headless
/// → None. Routing realny idzie przez rejestr serwisow mesh (head rejestruje sie
/// lokalnie z `127.0.0.1`), to tylko podglad dla GUI.
pub fn endpoint_url_for(spec: &DistributedDeploySpec) -> Option<String> {
    if spec.role == "head" {
        Some(format!("http://{}:{}/v1", spec.rdma_ip, spec.port))
    } else {
        None
    }
}

/// POSIX single-quote escaping. Zawija `s` w `'...'`, a kazdy `'` zamienia na
/// `'\''`. Neutralizuje WSZYSTKIE metaznaki powloki (`;`, `$()`, backtick, `&&`,
/// spacje), wiec wartosc nie moze wyrwac sie z `sh -c` (OWASP A03 — komenda leci
/// do uprzywilejowanego kontenera host-net/RDMA).
fn sh_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Czy silnik deklaruje multi-node przez natywny vLLM mp (`cluster_launch =
/// "vllm-mp"` w manifescie) zamiast Raya. Brak manifestu = false (Ray).
pub fn engine_is_vllm_mp(engine_id: &str) -> bool {
    crate::services::manifest::registry()
        .by_id(engine_id)
        .map(|m| m.engine.cluster_launch.as_deref() == Some("vllm-mp"))
        .unwrap_or(false)
}

/// Rank czlonka w trybie vllm-mp — koordynator wstrzykuje `_mp_node_rank` do
/// `config_json` spec-a (head=0, workery 1..). Brak klucza = blad (spec zbudowany
/// przez koordynatora bez rankow nie moze odpalic `--node-rank`).
fn mp_node_rank(spec: &DistributedDeploySpec) -> Result<u32, String> {
    serde_json::from_str::<Value>(&spec.config_json)
        .ok()
        .and_then(|v| v.get("_mp_node_rank").and_then(|x| x.as_u64()))
        .map(|r| r as u32)
        .ok_or_else(|| "vllm-mp: brak _mp_node_rank w config_json spec-a".to_string())
}

/// Stale argumenty `vllm serve` dla silnika w trybie vllm-mp. `vllm-dspark` =
/// zweryfikowany profil DeepSeek V4 Flash DSpark.
///
/// `num_speculative_tokens` MUSI rownac sie `dspark_block_size` z config.json
/// checkpointu, wiec przychodzi z PRESETU, nie z tej funkcji: preview ma 5, a
/// 0731 deklaruje 7. Blok krotszy niz zadeklarowany daje BLEDNE wyjscie, wiec
/// wczesniejsze zaszyte 3 leczylo objaw garblingu jego wlasna przyczyna —
/// wlasciwym lekarstwem jest probabilistyczny draft przy PELNYM bloku.
/// `--max-num-seqs`/`--max-cudagraph-capture-size` mozna nadpisac przez
/// `vllm_args` (argparse: ostatnie wystapienie wygrywa).
fn vllm_mp_engine_args(engine_id: &str, spec_num_tokens: Option<u32>) -> Vec<String> {
    if engine_id != "vllm-dspark" {
        return Vec::new();
    }
    let k = spec_num_tokens.unwrap_or(5).max(1);
    let speculative_config = format!(
        r#"{{"method":"dspark","num_speculative_tokens":{k},"draft_sample_method":"probabilistic"}}"#
    );
    [
        "--trust-remote-code",
        "--kv-cache-dtype",
        "nvfp4_ds_mla",
        "--block-size",
        "256",
        "--max-num-seqs",
        "12",
        "--max-num-batched-tokens",
        "8192",
        "--max-cudagraph-capture-size",
        "12",
        "--enable-prefix-caching",
        "--async-scheduling",
        "--enable-chunked-prefill",
        "--speculative-config",
        speculative_config.as_str(),
        "--tokenizer-mode",
        "deepseek_v4",
        "--tool-call-parser",
        "deepseek_v4",
        "--enable-auto-tool-choice",
        "--reasoning-parser",
        "deepseek_v4",
        "--reasoning-config",
        r#"{"reasoning_parser":"deepseek_v4","reasoning_start_str":"<think>","reasoning_end_str":"</think>"}"#,
        "--default-chat-template-kwargs",
        r#"{"thinking":false}"#,
        "--generation-config",
        "vllm",
        "--enable-flashinfer-autotune",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// Pelna komenda `vllm serve` w trybie vllm-mp — IDENTYCZNA na kazdym czlonku
/// poza `--node-rank` i `--headless` (kontrakt natywnego multi-node vLLM).
/// Master TCPStore = `ray_head_ip:dist_port` (ta sama para co tryb Ray, tylko
/// konsumowana flagami zamiast przez GCS). KAZDY token jest shell-quote'owany
/// (argi silnika niosa JSON-y ze spacjami/nawiasami).
fn build_mp_serve_command(
    spec: &DistributedDeploySpec,
    rank: u32,
    headless: bool,
) -> Result<String, String> {
    let nnodes = (spec.tp_size / spec.num_gpus.max(1)).max(1);
    let mut serve = format!(
        "vllm serve {model} \
         --host 0.0.0.0 \
         --port {port} \
         --served-model-name {served} \
         --tensor-parallel-size {tp} \
         --pipeline-parallel-size 1 \
         --gpu-memory-utilization {util:.2} \
         --max-model-len {maxlen} \
         --distributed-executor-backend mp \
         --nnodes {nnodes} \
         --node-rank {rank} \
         --master-addr {master} \
         --master-port {master_port}",
        model = sh_quote(&spec.model),
        port = spec.port,
        served = sh_quote(&spec.served_model_name),
        tp = spec.tp_size,
        util = spec.gpu_memory_utilization,
        maxlen = spec.max_model_len,
        nnodes = nnodes,
        rank = rank,
        master = sh_quote(&spec.ray_head_ip),
        master_port = spec.dist_port,
    );
    if headless {
        serve.push_str(" --headless");
    }
    for tok in vllm_mp_engine_args(&spec.engine_id, spec.speculative_num_tokens) {
        serve.push(' ');
        serve.push_str(&sh_quote(&tok));
    }
    for tok in user_vllm_arg_tokens(spec)? {
        serve.push(' ');
        serve.push_str(&sh_quote(&tok));
    }
    Ok(serve)
}

/// Komenda STARTU KONTENERA dla danej roli (idzie jako entrypoint `bash -c`).
/// KAZDA interpolowana wartosc jest shell-quote'owana.
///
/// WAZNE (ordering): head startuje TYLKO GCS Ray, po czym `sleep infinity` trzyma
/// kontener przy zyciu. `vllm serve` NIE jest tu w lancuchu `&&` — gdyby byl,
/// ruszylby od razu i ZABLOKOWAL czekajac na N GPU w klastrze Ray, ale worker
/// dolacza dopiero pozniej → vLLM timeoutuje i head pada. Zamiast tego koordynator
/// odpala `vllm serve` przez `docker exec` DOPIERO gdy `ray status` pokaze pelny
/// klaster (patrz `build_serve_command`). Worker = ray join + `--block`.
///
/// Tryb vllm-mp (natywny multi-node vLLM, bez Raya): worker startuje OD RAZU
/// pelne `vllm serve --headless --node-rank N` (worker-first — czeka na mastera
/// TCPStore), a head trzyma kontener na `sleep infinity`; serve head-a (rank 0,
/// binduje master port) odpala koordynator przez `docker exec` w P5 — czyli
/// dokladnie ostatni, jak wymaga tego mp init.
fn build_launch_command(spec: &DistributedDeploySpec) -> Result<String, String> {
    if engine_is_vllm_mp(&spec.engine_id) {
        if spec.role == "worker" {
            return build_mp_serve_command(spec, mp_node_rank(spec)?, true);
        }
        return Ok("sleep infinity".to_string());
    }
    let ray_addr = format!("{}:{}", spec.ray_head_ip, spec.ray_port);
    if spec.role == "worker" {
        return Ok(format!(
            "cd /root && ray start --address={addr} --node-ip-address={ip} --num-gpus={gpus} --block",
            addr = sh_quote(&ray_addr),
            ip = sh_quote(&spec.rdma_ip),
            gpus = spec.num_gpus,
        ));
    }

    // Head: tylko GCS Ray, potem `sleep infinity` (kontener zyje, `docker exec`
    // pozniej odpali vllm serve gdy klaster bedzie kompletny).
    Ok(format!(
        "cd /root && ray start --head --node-ip-address={ip} --port={ray_port} --num-gpus={gpus} --disable-usage-stats && sleep infinity",
        ip = sh_quote(&spec.ray_head_ip),
        ray_port = spec.ray_port,
        gpus = spec.num_gpus,
    ))
}

/// Komenda `vllm serve` (TP=N, backend ray) odpalana NA HEADZIE przez
/// `docker exec` DOPIERO gdy klaster Ray ma juz wszystkie GPU. Env (NCCL/RoCE,
/// VLLM_HOST_IP, HF_HUB_CACHE/OFFLINE) dziedziczone z konfiguracji kontenera.
/// Shell-quoting jak w `build_launch_command`. Tylko dla roli head.
pub fn build_serve_command(spec: &DistributedDeploySpec) -> Result<String, String> {
    if engine_is_vllm_mp(&spec.engine_id) {
        // Head = rank 0, nie-headless (serwuje API); workery juz czekaja na
        // mastera TCPStore, wiec ten exec domyka init caly klaster.
        return build_mp_serve_command(spec, 0, false);
    }
    let mut serve = format!(
        "cd /root && vllm serve {model} \
         --tensor-parallel-size {tp} \
         --distributed-executor-backend ray \
         --host 0.0.0.0 \
         --port {port} \
         --served-model-name {served} \
         --gpu-memory-utilization {util:.2} \
         --max-model-len {maxlen}",
        model = sh_quote(&spec.model),
        tp = spec.tp_size,
        port = spec.port,
        served = sh_quote(&spec.served_model_name),
        util = spec.gpu_memory_utilization,
        maxlen = spec.max_model_len,
    );
    // DGX Spark: eager + no-flashinfer-autotune (sm_121). Jedno zrodlo prawdy z
    // single-node (super::spark_engine_args) — cluster nie-Spark nic nie dostaje.
    for a in super::spark_engine_args(&spec.engine_id) {
        serve.push(' ');
        serve.push_str(&a);
    }
    for tok in user_vllm_arg_tokens(spec)? {
        serve.push(' ');
        serve.push_str(&sh_quote(&tok));
    }
    Ok(serve)
}

/// Tokeny `vllm_args` z `config_json` usera (puste gdy brak). Niezbalansowane
/// cudzyslowy → blad (zamiast cichego, niepoprawnego splitu).
fn user_vllm_arg_tokens(spec: &DistributedDeploySpec) -> Result<Vec<String>, String> {
    if spec.config_json.trim().is_empty() {
        return Ok(Vec::new());
    }
    let v: Value = serde_json::from_str(&spec.config_json)
        .map_err(|e| format!("invalid distributed config_json: {e}"))?;
    let raw = v
        .get("vllm_args")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    shlex::split(&raw).ok_or_else(|| "vllm_args: niezbalansowane cudzyslowy".to_string())
}

/// NCCL/RoCE env (przepuszczone przez `apply_engine_env` → kontener). OBA twins
/// w `NCCL_IB_HCA` daje pelne ~200G RDMA na DGX Spark; `gid_index` per-czlonek
/// (D1, nie hardkod); `VLLM_HOST_IP` przypina vLLM do interconnectu RDMA.
fn nccl_env(spec: &DistributedDeploySpec) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("NCCL_IB_HCA".into(), json!(spec.rdma_devices));
    m.insert("NCCL_SOCKET_IFNAME".into(), json!(spec.socket_ifname));
    m.insert("GLOO_SOCKET_IFNAME".into(), json!(spec.socket_ifname));
    m.insert("NCCL_IB_DISABLE".into(), json!("0"));
    m.insert(
        "NCCL_IB_GID_INDEX".into(),
        json!(spec.gid_index.to_string()),
    );
    m.insert("VLLM_HOST_IP".into(), json!(spec.rdma_ip));
    // Native PyTorch sampler zamiast FlashInfer: FlashInfer robi wolny kernel JIT (ninja)
    // na GB10 (sm_121a, brak prebuiltu), co blokuje faze profiling w TP init.
    // Tryb vllm-mp NIE dostaje tego override'u: obraz (np. vllm-dspark) wypala
    // wlasna, zweryfikowana wartosc (FlashInfer sampler=1 jest czescia garble-fixa
    // DSpark) i ma prebuiltowe kernele dla sm_121.
    if !engine_is_vllm_mp(&spec.engine_id) {
        m.insert("VLLM_USE_FLASHINFER_SAMPLER".into(), json!("0"));
    }
    // Persist fp4_gemm autotune (leci wielokrotnie przy starcie) w bind-montowanym cache.
    let autotune_cache = format!("{}/fi-at", crate::paths::CONTAINER_VLLM_CACHE_PATH);
    m.insert(
        "VLLM_FLASHINFER_AUTOTUNE_CACHE_DIR".into(),
        json!(autotune_cache),
    );
    m.insert(
        "HF_HUB_CACHE".into(),
        json!(crate::paths::CONTAINER_MODELS_PATH),
    );
    m.insert("HF_HUB_OFFLINE".into(), json!("1"));
    m
}

// =============================================================================
// P0 "ensure model" — head pobiera model do cache HF (jesli brak), potem
// koordynator przesyla snapshot na workery przez mesh (patrz `mesh_artifact`).
// Layout cache = ten sam, ktory serwuje `vllm serve`: `HF_HUB_CACHE=/data/models`
// (CONTAINER_MODELS_PATH) montowany z `models_root()` → snapshot lezy pod
// `models_root()/models--ORG--NAME/snapshots/HASH/` (BEZ podkatalogu `hub/`).
// =============================================================================

use std::path::{Path, PathBuf};

/// Nazwa katalogu cache HF dla repo (`org/name` → `models--org--name`).
pub fn model_dir_name(model_repo: &str) -> String {
    format!("models--{}", model_repo.replace('/', "--"))
}

/// Czy segment repo (`org` lub `name`) jest bezpieczny: niepusty, bez `..`, tylko
/// `[A-Za-z0-9._-]`.
fn repo_segment_is_safe(seg: &str) -> bool {
    !seg.is_empty()
        && !seg.contains("..")
        && seg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Czy `model_repo` ma bezpieczna postac HF repo id — DOKLADNIE `org/name` (jeden
/// slash, oba segmenty niepuste i tylko `[A-Za-z0-9._-]`). Chroni komende pobierania
/// (interpolacja do `python -c`) i sciezki cache przed path-traversal/injection —
/// zaden leading/trailing/multiple slash nie przechodzi.
fn model_repo_is_safe(model_repo: &str) -> bool {
    let mut parts = model_repo.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(org), Some(name), None) => repo_segment_is_safe(org) && repo_segment_is_safe(name),
        _ => false,
    }
}

/// Czy `engine_id` jest bezpieczny do interpolacji w prefiks obrazu
/// `tentaflow/{engine_id}` (peer steruje nim przy `EnsureModelLocal`). Allowlist
/// `[a-z0-9-]`, niepusty — zaden separator sciezki/powloki nie przejdzie.
fn engine_id_is_safe(engine_id: &str) -> bool {
    !engine_id.is_empty()
        && engine_id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Czy snapshot w `dir` jest kompletny do serwowania: ma `config.json` i choc
/// jeden plik `*.safetensors` (symlinki do blobow sa podazane przez `is_file`).
fn snapshot_complete(dir: &Path) -> bool {
    if !dir.join("config.json").is_file() {
        return false;
    }
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok()).any(|e| {
                e.path()
                    .extension()
                    .map(|x| x == "safetensors")
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

/// Sciezka KOMPLETNEGO snapshotu `model_repo` w lokalnym cache HF, albo `None`
/// gdy brak/niekompletny. Rozwiazuje rewizje przez `refs/main`, a jak brak — bierze
/// najnowszy katalog `snapshots/*`.
pub fn model_snapshot_dir(model_repo: &str) -> Option<PathBuf> {
    if !model_repo_is_safe(model_repo) {
        return None;
    }
    let base = crate::paths::models_root().join(model_dir_name(model_repo));
    let snapshots = base.join("snapshots");
    if !snapshots.is_dir() {
        return None;
    }
    // Preferuj rewizje wskazana przez `refs/main`; inaczej najnowszy snapshot.
    let by_ref = std::fs::read_to_string(base.join("refs").join("main"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(|h| snapshots.join(h))
        .filter(|p| p.is_dir());
    let candidate = match by_ref {
        Some(p) => p,
        None => std::fs::read_dir(&snapshots)
            .ok()?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_dir())
            .max_by_key(|p| {
                std::fs::metadata(p)
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH)
            })?,
    };
    snapshot_complete(&candidate).then_some(candidate)
}

/// Upewnia sie, ze `model_repo` jest w lokalnym cache HF; jesli nie — pobiera go
/// kontenerem silnika (`snapshot_download`) i zwraca sciezke snapshotu. `hf_token`
/// (gated repo) rozwiazany LOKALNIE u wolajacego (nie leci przez mesh). Docelowy
/// layout jest identyczny z tym, ktory serwuje `vllm serve` (HF_HUB_CACHE).
pub async fn ensure_model_downloaded_local(
    model_repo: &str,
    engine_id: &str,
    hf_token: Option<&str>,
    deployment_cluster_id: &str,
) -> Result<PathBuf, String> {
    if !model_repo_is_safe(model_repo) {
        return Err(format!("niepoprawne repo modelu: {model_repo}"));
    }
    if !engine_id_is_safe(engine_id) {
        return Err(format!("niepoprawny engine_id: {engine_id}"));
    }
    if let Some(dir) = model_snapshot_dir(model_repo) {
        tracing::info!(model = %model_repo, dir = %dir.display(), "P0: model juz w cache — pomijam pobieranie");
        return Ok(dir);
    }
    #[cfg(feature = "docker")]
    {
        download_model_via_container(model_repo, engine_id, hf_token, deployment_cluster_id)
            .await?;
        model_snapshot_dir(model_repo).ok_or_else(|| {
            format!("model {model_repo} pobrany, ale snapshot niekompletny (brak config.json/*.safetensors)")
        })
    }
    #[cfg(not(feature = "docker"))]
    {
        let _ = (engine_id, hf_token, deployment_cluster_id);
        Err("tentaflow-core zbudowany bez feature `docker` — nie moge pobrac modelu".to_string())
    }
}

/// Deterministyczna nazwa kontenera pobierania modelu dla danego deploymentu —
/// pozwala ubic go po stall-timeout (`docker rm -f`) i posprzatac przy teardown.
/// Sanityzacja do dozwolonych znakow nazwy kontenera Dockera (`[A-Za-z0-9_.-]`).
pub fn download_container_name(deployment_cluster_id: &str) -> String {
    let prefix: String = deployment_cluster_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .take(24)
        .collect();
    let prefix = if prefix.is_empty() {
        "unknown".to_string()
    } else {
        prefix
    };
    format!("tf-model-dl-{prefix}")
}

/// Best-effort usuniecie kontenera pobierania modelu (stall-kill / teardown).
/// Ignoruje bledy (kontener moze juz nie istniec — `--rm` sam go czysci po
/// normalnym zakonczeniu).
pub async fn remove_download_container(deployment_cluster_id: &str) {
    #[cfg(feature = "docker")]
    {
        let name = download_container_name(deployment_cluster_id);
        let _ = tokio::process::Command::new("docker")
            .args(["rm", "-f", &name])
            .output()
            .await;
    }
    #[cfg(not(feature = "docker"))]
    {
        let _ = deployment_cluster_id;
    }
}

/// Sumaryczny rozmiar plikow (bez podazania za symlinkami) w katalogu — mierzy
/// POSTEP pobierania: bloby HF rosna w `models--org--name/blobs/*`.
#[cfg(feature = "docker")]
fn dir_size_bytes(path: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(p) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&p) else {
            continue;
        };
        for entry in rd.flatten() {
            let ep = entry.path();
            let Ok(md) = std::fs::symlink_metadata(&ep) else {
                continue;
            };
            if md.is_dir() {
                stack.push(ep);
            } else if md.is_file() {
                total += md.len();
            }
        }
    }
    total
}

/// Pierwszy lokalny obraz Docker silnika (`tentaflow/<engine_id>...`), zdatny do
/// uruchomienia `python -c snapshot_download`. Cluster deploy vllm-spark buduje
/// `tentaflow/vllm-spark:<ver>` i cienki `tentaflow/vllm-spark-ray:<ver>` — oba
/// maja `huggingface_hub`, wiec dowolny pasujacy tag wystarczy.
#[cfg(feature = "docker")]
async fn resolve_download_image(engine_id: &str) -> Option<String> {
    let out = tokio::process::Command::new("docker")
        .args(["images", "--format", "{{.Repository}}:{{.Tag}}"])
        .output()
        .await
        .ok()?;
    let prefix = format!("tentaflow/{engine_id}");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with(&prefix) && !l.ends_with(":<none>"))
        .map(String::from)
}

/// Pobiera `model_repo` do zamontowanego cache HF przez `python -c snapshot_download`
/// w kontenerze silnika. `HF_HUB_CACHE`/`HF_HOME` = CONTAINER_MODELS_PATH (ten sam
/// mount i layout co serwowanie), `offline=0`, opcjonalny `HF_TOKEN`. Postep + tail
/// stderr logowane; niezerowy exit → blad z ogonem stderr.
#[cfg(feature = "docker")]
async fn download_model_via_container(
    model_repo: &str,
    engine_id: &str,
    hf_token: Option<&str>,
    deployment_cluster_id: &str,
) -> Result<(), String> {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio::io::AsyncRead;
    use tokio::io::{AsyncBufReadExt, BufReader};

    // Brak POSTEPU (rozmiar cache nie rosnie i output nie przybywa) przez ten czas
    // => pobieranie utknelo (np. gated model bez hf_token) — ubijamy kontener.
    // Duzy model przez wolne lacze pobiera sie legalnie dlugo, wiec progiem jest
    // BRAK POSTEPU, nie calkowity czas.
    const STALL_SECS: u64 = 600;
    // Bezpiecznik gornego limitu calkowitego czasu (postep to glowny mechanizm).
    const MAX_SECS: u64 = 4 * 3600;

    let image = resolve_download_image(engine_id).await.ok_or_else(|| {
        format!(
            "brak lokalnego obrazu 'tentaflow/{engine_id}...' do pobrania modelu — \
             zbuduj najpierw obraz silnika (deploy single-node)"
        )
    })?;
    let models_host = crate::paths::models_root();
    std::fs::create_dir_all(&models_host).map_err(|e| format!("mkdir models_root: {e}"))?;
    let cache_dir = models_host.join(model_dir_name(model_repo));
    let container_name = download_container_name(deployment_cluster_id);
    // Usun ewentualny nieszczelny kontener po poprzednim nieudanym pobieraniu,
    // inaczej `docker run --name` odmowi (konflikt nazwy).
    remove_download_container(deployment_cluster_id).await;
    let mount = format!(
        "{}:{}",
        models_host.display(),
        crate::paths::CONTAINER_MODELS_PATH
    );
    // `model_repo` jest ograniczony do [A-Za-z0-9._/-] przez `model_repo_is_safe`,
    // wiec nie moze wyrwac sie z literalu Pythona; caly `-c` idzie jako jeden argv
    // (bez powloki).
    let py =
        format!("from huggingface_hub import snapshot_download; snapshot_download('{model_repo}')");

    let mut args: Vec<String> = vec![
        "run".into(),
        "--rm".into(),
        "--name".into(),
        container_name.clone(),
        "-v".into(),
        mount,
        "-e".into(),
        format!("HF_HOME={}", crate::paths::CONTAINER_MODELS_PATH),
        "-e".into(),
        format!("HF_HUB_CACHE={}", crate::paths::CONTAINER_MODELS_PATH),
        "-e".into(),
        "HF_HUB_OFFLINE=0".into(),
    ];
    if let Some(tok) = hf_token.map(str::trim).filter(|s| !s.is_empty()) {
        args.push("-e".into());
        args.push(format!("HF_TOKEN={tok}"));
    }
    args.push("--entrypoint".into());
    args.push("python".into());
    args.push(image.clone());
    args.push("-c".into());
    args.push(py);

    tracing::info!(model = %model_repo, image = %image, "P0: pobieram model do cache HF (snapshot_download)");
    let mut child = tokio::process::Command::new("docker")
        .args(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("docker run (pobieranie modelu) nieudany: {e}"))?;

    let tail: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
    // Licznik linii outputu — rosnie => kontener zyje i cos robi (drugi sygnal
    // postepu obok rozmiaru cache).
    let line_counter = Arc::new(AtomicU64::new(0));
    let mut drains = Vec::new();
    // stdout i stderr maja rozne typy (ChildStdout / ChildStderr), wiec ujednolicamy
    // je do wspolnego trait-objectu AsyncRead, by trzymac w jednej tablicy.
    let pipes: Vec<(Option<Box<dyn AsyncRead + Unpin + Send>>, bool)> = vec![
        (
            child
                .stdout
                .take()
                .map(|s| Box::new(s) as Box<dyn AsyncRead + Unpin + Send>),
            false,
        ),
        (
            child
                .stderr
                .take()
                .map(|s| Box::new(s) as Box<dyn AsyncRead + Unpin + Send>),
            true,
        ),
    ];
    for (pipe, is_err) in pipes {
        let Some(pipe) = pipe else { continue };
        let tail = tail.clone();
        let line_counter = line_counter.clone();
        let repo = model_repo.to_string();
        drains.push(tokio::spawn(async move {
            let mut lines = BufReader::new(pipe).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                let l = l.trim().to_string();
                if l.is_empty() {
                    continue;
                }
                line_counter.fetch_add(1, Ordering::Relaxed);
                tracing::info!(model = %repo, "hf download: {l}");
                if is_err {
                    if let Ok(mut t) = tail.lock() {
                        t.push_back(l);
                        while t.len() > 40 {
                            t.pop_front();
                        }
                    }
                }
            }
        }));
    }

    // Heartbeat + stall-watchdog: duze wagi ida dlugo (tqdm pisze `\r` bez nowej
    // linii), wiec log milczalby minutami. Co 60s sprawdzamy POSTEP (rozmiar cache
    // rosnie LUB output przybywa); brak postepu przez STALL_SECS => ubijamy kontener.
    let started = std::time::Instant::now();
    let mut last_progress = std::time::Instant::now();
    let mut prev_lines = 0u64;
    let mut prev_size = 0u64;
    let status = loop {
        tokio::select! {
            s = child.wait() => break s.map_err(|e| format!("docker run (pobieranie modelu): {e}"))?,
            _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => {
                let lines = line_counter.load(Ordering::Relaxed);
                let size = dir_size_bytes(&cache_dir);
                if lines > prev_lines || size > prev_size {
                    last_progress = std::time::Instant::now();
                    prev_lines = lines;
                    prev_size = size;
                    tracing::info!(model = %model_repo, cache_mb = size / 1_048_576, "hf download: postep...");
                } else {
                    let stalled = last_progress.elapsed().as_secs();
                    tracing::warn!(model = %model_repo, stalled_s = stalled, "hf download: brak postepu");
                    if stalled >= STALL_SECS {
                        remove_download_container(deployment_cluster_id).await;
                        let _ = child.start_kill();
                        let _ = child.wait().await;
                        for d in drains {
                            let _ = d.await;
                        }
                        return Err(format!(
                            "pobieranie modelu {model_repo} utknelo (brak postepu przez {stalled}s) — \
                             sprawdz hf_token / dostepnosc modelu"
                        ));
                    }
                }
                if started.elapsed().as_secs() >= MAX_SECS {
                    remove_download_container(deployment_cluster_id).await;
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                    for d in drains {
                        let _ = d.await;
                    }
                    return Err(format!(
                        "pobieranie modelu {model_repo} przekroczylo gorny limit czasu ({MAX_SECS}s)"
                    ));
                }
            }
        }
    };
    for d in drains {
        let _ = d.await;
    }
    if status.success() {
        tracing::info!(model = %model_repo, "P0: pobieranie modelu zakonczone");
        Ok(())
    } else {
        let tail_txt = tail
            .lock()
            .ok()
            .map(|t| t.iter().cloned().collect::<Vec<_>>().join("\n"))
            .unwrap_or_default();
        Err(format!(
            "pobieranie modelu {model_repo} nieudane (exit {:?})\n--- stderr ---\n{tail_txt}",
            status.code()
        ))
    }
}

/// Buduje `user_config` (JSON) sterujacy istniejacym potokiem `deploy()` na
/// docelowym czlonku. Klucze: `_distributed` (host-net + RDMA flagi),
/// `launch_command_override` (ray + vllm verbatim), `engine_env` (NCCL/RoCE),
/// `model_repo`/`served_model_name`/`transport_explicit`/`gpu_select_mode`.
pub fn build_member_config_json(spec: &DistributedDeploySpec) -> Result<String, String> {
    let mut cfg: Map<String, Value> = if spec.config_json.trim().is_empty() {
        Map::new()
    } else {
        serde_json::from_str::<Value>(&spec.config_json)
            .map_err(|e| format!("invalid distributed config_json: {e}"))?
            .as_object()
            .cloned()
            .ok_or_else(|| "distributed config_json must be a JSON object".to_string())?
    };

    cfg.insert("model_repo".into(), json!(spec.model));
    cfg.insert("served_model_name".into(), json!(spec.served_model_name));
    cfg.insert("transport_explicit".into(), json!("direct_http"));
    cfg.entry("gpu_select_mode".to_string())
        .or_insert_with(|| json!("all"));
    cfg.insert(
        "launch_command_override".into(),
        json!(build_launch_command(spec)?),
    );

    // Merge NCCL env z ewentualnym engine_env usera (NCCL wygrywa — to kontrakt
    // RDMA klastra, nie da sie go nadpisac z formularza).
    let mut env = cfg
        .get("engine_env")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    for (k, v) in nccl_env(spec) {
        env.insert(k, v);
    }
    // DGX Spark: Marlin NVFP4 GEMM na kazdym czlonku klastra (head + workery).
    // No-op dla nie-fp4; cluster nie-Spark (engine != vllm-spark) nic nie dostaje.
    for (k, v) in super::spark_engine_env(&spec.engine_id) {
        env.insert(k, Value::String(v));
    }
    cfg.insert("engine_env".into(), Value::Object(env));

    cfg.insert(
        "_distributed".into(),
        json!({
            "role": spec.role,
            "port": spec.port,
            "dist_port": spec.dist_port,
            "deployment_cluster_id": spec.deployment_cluster_id,
            "cluster_id": spec.cluster_id,
            "num_gpus": spec.num_gpus,
            "tp_size": spec.tp_size,
        }),
    );

    serde_json::to_string(&Value::Object(cfg))
        .map_err(|e| format!("serialize distributed config: {e}"))
}

// =============================================================================
// Preflight (P1-4) — czysci stale kontenery Ray z poprzednich prob i sprawdza,
// ze porty head-a sa wolne PRZED startem (host networking nie chroni portow).
// =============================================================================

/// Usuwa WSZYSTKIE kontenery z etykieta distributed na TYM nodzie — kazdego starego
/// deploymentu, BIEZACY id wlacznie. Z `--network host` ich procesy trzymaja porty
/// (torch.distributed master + serve), wiec idempotentny retry tego samego id tez
/// musi je najpierw usunac, inaczej nowy serve dostanie `EADDRINUSE`. `rm -f`
/// zwalnia porty (TCP_LISTEN→CLOSED). Best-effort.
///
/// DLACZEGO remove-all (nie tylko biezacy id): node ma jeden zestaw GPU, wiec
/// fizycznie tylko JEDEN distributed deploy moze na nim dzialac naraz. Kazdy
/// istniejacy kontener distributed MUSI wiec zostac usuniety przed nowym deployem.
/// Usuniecie kontenera nalezacego do INNEGO deployment_cluster_id nie jest ciche —
/// logujemy `warn!`, bo to zatrzymanie obcego, rownoleglego deploymentu (ktorego
/// rekord w DB pozostaje wtedy `running`/stale; sprzatniecie stanu DB tego obcego
/// deploymentu wymagaloby przekazania `db` do preflight — follow-up).
#[cfg(feature = "docker")]
async fn remove_all_distributed_containers(current_deployment_cluster_id: &str) {
    let Ok(out) = tokio::process::Command::new("docker")
        .args([
            "ps",
            "-aq",
            "--filter",
            &format!("label={}", super::docker::DISTRIBUTED_LABEL),
        ])
        .output()
        .await
    else {
        return;
    };
    for id in String::from_utf8_lossy(&out.stdout).split_whitespace() {
        let other_id = tokio::process::Command::new("docker")
            .args([
                "inspect",
                id,
                "--format",
                &format!(
                    "{{{{index .Config.Labels \"{}\"}}}}",
                    super::docker::DISTRIBUTED_LABEL
                ),
            ])
            .output()
            .await
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        if !other_id.is_empty() && other_id != current_deployment_cluster_id {
            tracing::warn!(
                removed_deployment = %other_id,
                current = %current_deployment_cluster_id,
                "preflight: removing container from a DIFFERENT distributed deployment — only one distributed deploy can run per node (single GPU set)"
            );
        }
        let _ = tokio::process::Command::new("docker")
            .args(["rm", "-f", id])
            .output()
            .await;
    }
}

/// Czy port TCP jest wolny na hoscie (probny bind 0.0.0.0). Krotki retry na
/// zwolnienie po `docker rm` (TCP_LISTEN→CLOSED w jadrze).
#[cfg(feature = "docker")]
async fn host_port_free(port: u16) -> bool {
    use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};
    for _ in 0..10 {
        if TcpListener::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port)).is_ok() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    false
}

/// Preflight czlonka PRZED deployem: czysci stary Ray (inny deployment), a dla
/// head sprawdza wolnosc portu serve + GCS Ray. Zwraca czytelny blad gdy port
/// trzyma obcy proces (nie nasz stale Ray). Idempotentny.
#[cfg(feature = "docker")]
pub async fn preflight_member(spec: &DistributedDeploySpec) -> Result<(), String> {
    // Hard cleanup: drop EVERY distributed-label container on this node (any prior
    // deployment, this id included) so host-network ports are released BEFORE the
    // free-checks below — a leftover process is exactly what makes serve EADDRINUSE.
    remove_all_distributed_containers(&spec.deployment_cluster_id).await;

    // torch.distributed master port (allocated `spec.dist_port`) + serve port must
    // be free on every member — a leftover process on either silently breaks
    // `vllm serve` (EADDRINUSE on the TCPStore master, no visible error in detached
    // exec). `host_port_free` retries.
    if !host_port_free(spec.dist_port).await {
        return Err(format!(
            "port {} zajety przed deployem (leftover proces torch.distributed)",
            spec.dist_port
        ));
    }
    if !host_port_free(spec.port).await {
        return Err(format!(
            "port serve {} zajety przed deployem (leftover proces)",
            spec.port
        ));
    }
    if spec.role == "head" && !host_port_free(spec.ray_port).await {
        return Err(format!(
            "port GCS Ray {} zajety na hoscie (obcy proces?) — zwolnij go",
            spec.ray_port
        ));
    }
    Ok(())
}

#[cfg(not(feature = "docker"))]
pub async fn preflight_member(_spec: &DistributedDeploySpec) -> Result<(), String> {
    Err("tentaflow-core compiled without `docker` feature".to_string())
}

// =============================================================================
// Readiness (P1-1 / P2-1) — realny pomiar gotowosci head-a.
// =============================================================================

/// Stan gotowosci czlonka distributed-deploymentu NA TYM nodzie.
#[derive(Debug, Clone)]
pub struct ReadinessStatus {
    /// Kontener deploymentu na tym nodzie dziala (obraz zbudowany + kontener
    /// wstal) — gate fazy BUILDU przed odliczaniem GCS/serve.
    pub container_running: bool,
    /// GCS Ray nasluchuje (workery moga dolaczyc).
    pub ray_gcs_up: bool,
    /// Ilu nodow widzi klaster Ray (head + workery; weryfikacja dolaczenia).
    pub ray_nodes: u32,
    /// Endpoint OpenAI `/v1/models` zwraca 200 (caly TP-cluster serwuje).
    pub serve_ready: bool,
    pub error: Option<String>,
}

/// Sonduje gotowosc czlonka NA TYM nodzie: czy kontener dziala (faza build),
/// GCS Ray (TCP), czlonkostwo (`ray status` w kontenerze head-a) i endpoint
/// OpenAI. Bezstanowa. GCS/serve maja sens tylko dla head-a; container_running
/// dla kazdego noda.
#[cfg(feature = "docker")]
pub async fn probe_readiness(
    deployment_cluster_id: &str,
    ray_port: u16,
    serve_port: u16,
) -> ReadinessStatus {
    let container_running = distributed_container_running(deployment_cluster_id).await;
    let ray_gcs_up = tcp_reachable("127.0.0.1", ray_port).await;
    let serve_ready = http_models_ok(serve_port).await;
    let ray_nodes = ray_active_node_count(deployment_cluster_id).await;
    ReadinessStatus {
        container_running,
        ray_gcs_up,
        ray_nodes,
        serve_ready,
        error: None,
    }
}

#[cfg(not(feature = "docker"))]
pub async fn probe_readiness(
    _deployment_cluster_id: &str,
    _ray_port: u16,
    _serve_port: u16,
) -> ReadinessStatus {
    ReadinessStatus {
        container_running: false,
        ray_gcs_up: false,
        ray_nodes: 0,
        serve_ready: false,
        error: Some("docker feature disabled".to_string()),
    }
}

/// Id kontenera HEAD-a (po etykiecie deploymentu + roli) NA TYM nodzie. None gdy
/// brak (np. ten nod nie jest headem albo kontener nie wstal).
#[cfg(feature = "docker")]
async fn head_container_id(deployment_cluster_id: &str) -> Option<String> {
    let out = tokio::process::Command::new("docker")
        .args([
            "ps",
            "--filter",
            &format!(
                "label={}={}",
                super::docker::DISTRIBUTED_LABEL,
                deployment_cluster_id
            ),
            "--filter",
            "label=tentaflow.distributed_role=head",
            "-q",
        ])
        .output()
        .await
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .map(String::from)
}

/// Sciezka pliku logu `vllm serve` W KONTENERZE head-a. Detached `docker exec -d`
/// nie ma stdout do odebrania, wiec serve loguje do pliku, ktory pozniej czytamy
/// przez `serve_log_tail` — bez tego ciche padniecie serve jest niewidoczne.
#[cfg(feature = "docker")]
const SERVE_LOG_PATH: &str = "/tmp/vllm-serve.log";

/// Odpala `vllm serve` NA HEADZIE przez `docker exec -d` (detached) DOPIERO gdy
/// klaster Ray jest kompletny. Env dziedziczone z kontenera (NCCL/RoCE/HF). Blad
/// gdy kontener head-a nie istnieje albo exec sie nie powiodl. stdout+stderr serve
/// idzie do `SERVE_LOG_PATH` w kontenerze (detached exec gubi strumienie) — dzieki
/// temu `serve_log_tail` pokaze REALNY powod, gdy serve cicho padnie.
#[cfg(feature = "docker")]
pub async fn exec_serve_on_head(
    deployment_cluster_id: &str,
    serve_cmd: &str,
) -> Result<(), String> {
    let cid = head_container_id(deployment_cluster_id)
        .await
        .ok_or_else(|| "kontener head-a nie istnieje (vllm serve)".to_string())?;
    // `serve_cmd` is already a shell command string; wrap it in a group with the
    // log redirect so the detached process captures stdout+stderr to a file.
    let logged = format!("{{ {serve_cmd} ; }} > {SERVE_LOG_PATH} 2>&1");
    let out = tokio::process::Command::new("docker")
        .args(["exec", "-d", &cid, "bash", "-c", &logged])
        .output()
        .await
        .map_err(|e| format!("docker exec (vllm serve) nieudany: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "docker exec (vllm serve) blad: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

#[cfg(not(feature = "docker"))]
pub async fn exec_serve_on_head(
    _deployment_cluster_id: &str,
    _serve_cmd: &str,
) -> Result<(), String> {
    Err("docker feature disabled".to_string())
}

/// Ostatnie `lines` linii logu `vllm serve` z kontenera head-a (po etykiecie
/// deploymentu). None gdy nie ma head-a NA TYM nodzie albo nie da sie odczytac
/// logu. Uzywane do wciagniecia realnego bledu serve do komunikatu o timeoucie.
#[cfg(feature = "docker")]
pub async fn serve_log_tail(deployment_cluster_id: &str, lines: usize) -> Option<String> {
    let cid = head_container_id(deployment_cluster_id).await?;
    let out = tokio::process::Command::new("docker")
        .args([
            "exec",
            &cid,
            "tail",
            "-n",
            &lines.to_string(),
            SERVE_LOG_PATH,
        ])
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

#[cfg(not(feature = "docker"))]
pub async fn serve_log_tail(_deployment_cluster_id: &str, _lines: usize) -> Option<String> {
    None
}

/// Czy JAKIKOLWIEK kontener deploymentu (po etykiecie) dziala na tym nodzie.
/// Dla head = kontener head-a, dla worker-noda = kontener workera. Gate buildu.
#[cfg(feature = "docker")]
async fn distributed_container_running(deployment_cluster_id: &str) -> bool {
    let Ok(out) = tokio::process::Command::new("docker")
        .args([
            "ps",
            "--filter",
            &format!(
                "label={}={}",
                super::docker::DISTRIBUTED_LABEL,
                deployment_cluster_id
            ),
            "--filter",
            "status=running",
            "-q",
        ])
        .output()
        .await
    else {
        return false;
    };
    !String::from_utf8_lossy(&out.stdout).trim().is_empty()
}

#[cfg(feature = "docker")]
async fn tcp_reachable(host: &str, port: u16) -> bool {
    use tokio::net::TcpStream;
    matches!(
        tokio::time::timeout(
            std::time::Duration::from_millis(1500),
            TcpStream::connect((host, port)),
        )
        .await,
        Ok(Ok(_))
    )
}

#[cfg(feature = "docker")]
async fn http_models_ok(serve_port: u16) -> bool {
    let Ok(client) = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
    else {
        return false;
    };
    let url = format!("http://127.0.0.1:{}/v1/models", serve_port);
    matches!(client.get(&url).send().await, Ok(r) if r.status().is_success())
}

/// Liczba aktywnych nodow Ray wg `ray status` uruchomionego W kontenerze head-a
/// (znalezionym po etykiecie deploymentu + roli). Best-effort (0 gdy nie da sie
/// odpytac) — twardy gate gotowosci to `serve_ready` (vLLM nie wstanie bez
/// wszystkich workerow), to liczy sie do diagnostyki i wczesnego sygnalu.
#[cfg(feature = "docker")]
async fn ray_active_node_count(deployment_cluster_id: &str) -> u32 {
    let Ok(ps) = tokio::process::Command::new("docker")
        .args([
            "ps",
            "--filter",
            &format!(
                "label={}={}",
                super::docker::DISTRIBUTED_LABEL,
                deployment_cluster_id
            ),
            "--filter",
            "label=tentaflow.distributed_role=head",
            "-q",
        ])
        .output()
        .await
    else {
        return 0;
    };
    let Some(cid) = String::from_utf8_lossy(&ps.stdout)
        .split_whitespace()
        .next()
        .map(String::from)
    else {
        return 0;
    };
    let Ok(out) = tokio::process::Command::new("docker")
        .args(["exec", &cid, "ray", "status"])
        .output()
        .await
    else {
        return 0;
    };
    if !out.status.success() {
        return 0;
    }
    parse_ray_active_nodes(&String::from_utf8_lossy(&out.stdout))
}

/// Parsuje sekcje `Active:` z wyjscia `ray status` i liczy linie nodow
/// (`<n> node_<hex>`). Tylko Active (pomija Pending/Recent failures).
#[cfg(feature = "docker")]
fn parse_ray_active_nodes(text: &str) -> u32 {
    let mut in_active = false;
    let mut count = 0u32;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("Active:") {
            in_active = true;
            continue;
        }
        if t.ends_with(':') && !t.starts_with("Active") {
            in_active = false;
        }
        if in_active && t.contains("node_") {
            count += 1;
        }
    }
    count
}

// =============================================================================
// Teardown (P1-2) — propaguje bledy (brak osieroconych kontenerow Ray).
// =============================================================================

/// Tear down kontenerow + wierszy serwisow `deployment_cluster_id` NA TYM nodzie.
/// Zwraca `(usuniete_service_id, bledy)`. NIEpusta lista bledow = teardown
/// NIEKOMPLETNY (kontener moze nadal zyc) — caller MUSI zachowac rekord
/// deploymentu do retry. Idempotentny.
pub async fn stop_distributed(
    db: &crate::db::DbPool,
    ports: std::sync::Arc<crate::services::ports::PortAllocator>,
    deployment_cluster_id: &str,
) -> (Vec<i64>, Vec<String>) {
    let mut errors: Vec<String> = Vec::new();

    // 1. Kontenery po etykiecie grupujacej.
    #[cfg(feature = "docker")]
    {
        match tokio::process::Command::new("docker")
            .args([
                "ps",
                "-aq",
                "--filter",
                &format!(
                    "label={}={}",
                    super::docker::DISTRIBUTED_LABEL,
                    deployment_cluster_id
                ),
            ])
            .output()
            .await
        {
            Ok(out) => {
                for id in String::from_utf8_lossy(&out.stdout).split_whitespace() {
                    let rm = tokio::process::Command::new("docker")
                        .args(["rm", "-f", id])
                        .output()
                        .await;
                    match rm {
                        Ok(o) if o.status.success() => {}
                        // Idempotent: a container already gone between `ps` and
                        // `rm` (or removed by a prior teardown) is NOT a failure.
                        Ok(o)
                            if String::from_utf8_lossy(&o.stderr)
                                .to_lowercase()
                                .contains("no such container") => {}
                        Ok(o) => errors.push(format!(
                            "docker rm {} nieudany: {}",
                            id,
                            String::from_utf8_lossy(&o.stderr).trim()
                        )),
                        Err(e) => errors.push(format!("docker rm {} nieudany: {}", id, e)),
                    }
                }
            }
            Err(e) => errors.push(format!("docker ps (teardown) nieudany: {}", e)),
        }
    }

    // 2. Wiersze serwisow niosace ten deployment_cluster_id (head + workery).
    let rows = match db.read() {
        Ok(conn) => crate::services_repo::services::list_all(&conn)
            .unwrap_or_default()
            .into_iter()
            .filter(|s| {
                s.config_json.contains(&format!(
                    "\"deployment_cluster_id\":\"{}\"",
                    deployment_cluster_id
                ))
            })
            .collect::<Vec<_>>(),
        Err(e) => {
            errors.push(format!("db read (teardown) nieudany: {}", e));
            Vec::new()
        }
    };

    let mut removed_ids = Vec::new();
    for svc in rows {
        if let Err(e) = crate::services::deploy::stop(&svc, ports.clone()).await {
            errors.push(format!("stop service {} nieudany: {}", svc.id, e));
        }
        match db.write() {
            Ok(conn) => match crate::services_repo::services::delete(&conn, svc.id) {
                Ok(()) => removed_ids.push(svc.id),
                Err(e) => errors.push(format!("delete service {} nieudany: {}", svc.id, e)),
            },
            Err(e) => errors.push(format!("db write (teardown) nieudany: {}", e)),
        }
    }
    (removed_ids, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(role: &str) -> DistributedDeploySpec {
        DistributedDeploySpec {
            deployment_cluster_id: "dep-123".into(),
            cluster_id: "clus-1".into(),
            engine_id: "vllm-spark".into(),
            role: role.into(),
            model: "LilaRest/gemma-4-31B-it-NVFP4-turbo".into(),
            served_model_name: "gemma-4-31b".into(),
            tp_size: 2,
            num_gpus: 1,
            port: 8100,
            dist_port: 8101,
            gpu_memory_utilization: 0.9,
            max_model_len: 8192,
            ray_head_ip: "10.10.10.24".into(),
            ray_port: 6379,
            rdma_ip: "10.10.10.25".into(),
            rdma_devices: "roceP2p1s0f0,rocep1s0f0".into(),
            socket_ifname: "enP2p1s0f0np0".into(),
            gid_index: 3,
            speculative_num_tokens: None,
            config_json: String::new(),
        }
    }

    #[test]
    fn head_launch_is_ray_head_only_then_sleep() {
        // Head container command = ray head + sleep (NO vllm serve — that runs
        // later via docker exec once the cluster is complete).
        let cmd = build_launch_command(&spec("head")).unwrap();
        assert!(cmd.contains("ray start --head --node-ip-address='10.10.10.24' --port=6379"));
        assert!(cmd.trim_end().ends_with("sleep infinity"));
        assert!(!cmd.contains("vllm serve"));
    }

    #[test]
    fn head_serve_command_has_tp_and_ray_backend() {
        let cmd = build_serve_command(&spec("head")).unwrap();
        assert!(cmd.starts_with("cd /root && vllm serve 'LilaRest/gemma-4-31B-it-NVFP4-turbo'"));
        assert!(cmd.contains("--tensor-parallel-size 2"));
        assert!(cmd.contains("--distributed-executor-backend ray"));
        assert!(cmd.contains("--enforce-eager"));
        assert!(!cmd.contains("ray start"));
    }

    #[test]
    fn worker_command_joins_head_and_blocks() {
        let cmd = build_launch_command(&spec("worker")).unwrap();
        assert!(cmd.contains("ray start --address='10.10.10.24:6379'"));
        assert!(cmd.contains("--node-ip-address='10.10.10.25'"));
        assert!(cmd.trim_end().ends_with("--block"));
        assert!(!cmd.contains("vllm serve"));
    }

    #[test]
    fn shell_metachars_in_model_are_neutralized() {
        let mut s = spec("head");
        s.model = "evil'; rm -rf / #".into();
        let cmd = build_serve_command(&s).unwrap();
        // Single-quote escaping: the embedded quote becomes `'\''`, so the
        // dangerous payload cannot break out into a new shell token.
        assert!(cmd.contains("vllm serve 'evil'\\''; rm -rf / #'"));
        assert!(!cmd.contains("&& rm -rf"));
    }

    #[test]
    fn vllm_args_tokens_are_quoted() {
        let mut s = spec("head");
        s.config_json = r#"{"vllm_args":"--swap-space 8 --foo $(touch /pwned)"}"#.into();
        let cmd = build_serve_command(&s).unwrap();
        assert!(cmd.contains("'--swap-space'"));
        assert!(cmd.contains("'$(touch /pwned)'")); // quoted → inert
        assert!(!cmd.contains("&& touch"));
    }

    #[test]
    fn unbalanced_vllm_args_rejected() {
        let mut s = spec("head");
        s.config_json = r#"{"vllm_args":"--foo 'unbalanced"}"#.into();
        assert!(build_serve_command(&s).is_err());
    }

    #[test]
    fn config_carries_distributed_block_and_nccl_env() {
        let json = build_member_config_json(&spec("head")).unwrap();
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["_distributed"]["role"], "head");
        assert_eq!(v["engine_env"]["NCCL_IB_HCA"], "roceP2p1s0f0,rocep1s0f0");
        assert_eq!(v["engine_env"]["NCCL_IB_GID_INDEX"], "3");
        assert_eq!(v["transport_explicit"], "direct_http");
    }

    #[test]
    fn gid_index_flows_from_spec() {
        let mut s = spec("head");
        s.gid_index = 1;
        let json = build_member_config_json(&s).unwrap();
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["engine_env"]["NCCL_IB_GID_INDEX"], "1");
    }

    #[test]
    fn worker_config_has_no_endpoint() {
        assert!(endpoint_url_for(&spec("worker")).is_none());
        assert!(endpoint_url_for(&spec("head")).is_some());
    }

    #[cfg(feature = "docker")]
    #[test]
    fn ray_status_active_node_parse() {
        let txt = "Node status\n--------\nActive:\n 1 node_aaa\n 1 node_bbb\nPending:\n (no pending nodes)\nRecent failures:\n node_ccc\n";
        assert_eq!(parse_ray_active_nodes(txt), 2);
    }
}

// =============================================================================
// Plik: mesh.rs
// Opis: Typy wiadomosci mesh dla komunikacji gossip, membership
//       i service discovery miedzy nodami TentaFlow.AI przez QUIC.
// =============================================================================

use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

use crate::profiling::{
    ProfilingActiveInfoRequest, ProfilingActiveInfoResponse, ProfilingDeleteRequest,
    ProfilingDeleteResponse, ProfilingDownloadRequest, ProfilingDownloadResponse,
    ProfilingReportRequest, ProfilingReportResponse, ProfilingSessionsRequest,
    ProfilingSessionsResponse, ProfilingStartRequest, ProfilingStartResponse, ProfilingStopRequest,
    ProfilingStopResponse,
};

// =============================================================================
// Glowny enum wiadomosci mesh
// =============================================================================

/// Wiadomosc protokolu mesh - gossip, membership, service discovery
/// i forwarding requestow miedzy nodami.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub enum MeshMessage {
    // -- Gossip --
    /// Ping do sprawdzenia czy nod zyje
    Ping { from: String, incarnation: u64 },

    /// Odpowiedz na ping
    PingAck { from: String, incarnation: u64 },

    /// Posredni ping przez inny nod (protocol SWIM)
    IndirectPing {
        from: String,
        target: String,
        incarnation: u64,
    },

    // -- Membership --
    /// Dolaczenie noda do mesh
    Join {
        node_id: String,
        addr: String,
        role: String,
        capabilities: Vec<String>,
    },

    /// Opuszczenie mesh przez nod
    Leave { node_id: String },

    // -- Forwarding --
    /// Przekazanie requestu do innego noda
    ForwardRequest {
        request_id: String,
        target_node: String,
        payload: Vec<u8>,
    },

    /// Odpowiedz na przekazany request
    ForwardResponse {
        request_id: String,
        payload: Vec<u8>,
    },

    // -- Stale polaczenia QUIC --
    /// Heartbeat wysylany co 500ms na stalym polaczeniu
    Heartbeat(MeshHeartbeat),

    /// Pelna wymiana stanu po nawiazaniu polaczenia QUIC
    FullStateExchange(MeshFullState),

    /// Aktualizacja listy modeli na nodzie
    ModelListUpdate {
        node_id: String,
        models: Vec<MeshModelInfo>,
    },

    /// Aktualizacja listy kontenerow na nodzie
    ContainerListUpdate {
        node_id: String,
        containers: Vec<MeshContainerInfo>,
    },

    // -- Parowanie mesh (bezpieczenstwo) --
    /// Zadanie parowania — wysylane do noda po mDNS discovery
    PairingRequest { from_node_id: String, pin: String },

    /// Potwierdzenie parowania — wymiana kluczy publicznych
    PairingConfirm {
        from_node_id: String,
        public_key: Vec<u8>,
    },

    /// Odrzucenie parowania
    PairingReject { from_node_id: String },

    /// Cofniecie zaufania — node nie jest juz zaufany
    TrustRevoked { node_id: String },

    /// Synchronizacja kluczy zaufanych nodow po zatwierdzeniu parowania
    TrustedKeysSync { keys: Vec<(String, Vec<u8>)> },

    /// Rotacja klucza szyfrowania — wymiana ephemeral X25519 public key
    KeyRotation {
        from_node_id: String,
        ephemeral_public_key: String,
    },

    /// Odpowiedz na rotacje klucza — zawiera ephemeral public key drugiej strony
    KeyRotationResponse {
        from_node_id: String,
        ephemeral_public_key: String,
    },

    /// Graceful leave — node opuszcza mesh (nie revoke, chwilowe odlaczenie)
    NodeLeaving { node_id: String },

    // -- Komendy zarzadzania --
    /// Komenda zarzadzania wyslana do sparowanego noda
    MeshCommand {
        command_id: String,
        from_node_id: String,
        command: MeshCommandType,
    },

    /// Odpowiedz na komende zarzadzania — typed payload zamiast goluego stringa.
    MeshCommandResponse {
        command_id: String,
        from_node_id: String,
        ok: bool,
        payload: MeshCommandResponsePayload,
        error: Option<String>,
    },

    /// Streaming postepu deploy (od noda wykonujacego)
    MeshDeployProgress {
        command_id: String,
        from_node_id: String,
        phase: String,
        message: String,
        percent: u8,
        is_done: bool,
    },

    /// Fragment logow kontenera (streaming)
    MeshLogChunk {
        command_id: String,
        from_node_id: String,
        container_id: String,
        data: String,
        is_stderr: bool,
        is_done: bool,
    },

    // -- Cluster --
    /// Informacja o clusterze nodow
    ClusterInfo {
        cluster_id: String,
        name: String,
        node_ids: Vec<String>,
        strategy: String,
    },
}

// =============================================================================
// Metryki GPU w heartbeat
// =============================================================================

/// Metryki pojedynczego GPU przesylane w heartbeat.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshGpuMetric {
    /// Indeks GPU na nodzie
    pub index: u32,
    /// Uzycie GPU w procentach (0-100)
    pub usage_percent: f32,
    /// Zuzycie VRAM w MB
    pub vram_used_mb: u64,
    /// Calkowita VRAM w MB
    pub vram_total_mb: u64,
    /// Temperatura GPU w stopniach Celsjusza
    pub temperature_c: f32,
}

// =============================================================================
// Heartbeat stalego polaczenia QUIC
// =============================================================================

/// Heartbeat wysylany co 500ms na stalym polaczeniu QUIC.
/// Zawiera metryki zasobow noda do load balancingu.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshHeartbeat {
    /// Identyfikator noda
    pub node_id: String,
    /// Hostname noda
    pub hostname: String,
    /// Adresy IP noda (np. ["192.168.1.10", "10.0.0.5"])
    pub ip_addresses: Vec<String>,
    /// Timestamp w milisekundach (unix epoch)
    pub timestamp_ms: u64,
    /// Uzycie CPU w procentach (0-100)
    pub cpu_usage_percent: f32,
    /// Zuzycie RAM w MB
    pub ram_used_mb: u64,
    /// Calkowita pamiec RAM w MB
    pub ram_total_mb: u64,
    /// Metryki poszczegolnych GPU
    pub gpu_metrics: Vec<MeshGpuMetric>,
    /// Srednie obciazenie systemu (1 minuta)
    pub load_avg_1m: f32,
    /// Liczba aktywnych requestow
    pub active_requests: u32,
    /// Platforma noda: "linux", "macos", "windows", "android", "ios"
    pub platform: String,
    /// Liczba serwisow uruchomionych na nodzie
    pub services_count: u32,
    /// Czy Docker jest uruchomiony
    pub docker_running: bool,
}

// =============================================================================
// Informacja o modelu AI
// =============================================================================

/// Opis modelu AI zaladowanego na nodzie mesh.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshModelInfo {
    /// Nazwa modelu
    pub name: String,
    /// Rozmiar modelu w bajtach
    pub size_bytes: u64,
    /// Backend inferencyjny (np. "llama.cpp", "vllm")
    pub backend: String,
    /// Maksymalny rozmiar kontekstu w tokenach
    pub max_context: u32,
    /// Kwantyzacja modelu (np. "Q4_K_M", "FP16")
    pub quantization: String,
}

// =============================================================================
// Informacja o kontenerze Docker
// =============================================================================

/// Opis kontenera Docker dzialajacego na nodzie mesh.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshContainerInfo {
    /// Identyfikator kontenera
    pub id: String,
    /// Nazwa kontenera
    pub name: String,
    /// Obraz Docker
    pub image: String,
    /// Status kontenera (np. "running", "exited")
    pub status: String,
    /// Lista mapowanych portow (np. "8080:80")
    pub ports: Vec<String>,
    /// Uzycie CPU w procentach
    pub cpu_percent: f32,
    /// Uzycie pamieci w MB
    pub memory_mb: u64,
}

// =============================================================================
// Pelny stan noda po nawiazaniu polaczenia QUIC
// =============================================================================

/// Pelna wymiana stanu po nawiazaniu polaczenia QUIC.
/// Wysylana jednokrotnie przy handshake, potem aktualizacje przyrostowe.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshFullState {
    /// Identyfikator noda
    pub node_id: String,
    /// Hostname noda
    pub hostname: String,
    /// Adresy IP noda
    pub ip_addresses: Vec<String>,
    /// Rola noda w mesh (np. "router", "desktop", "mobile")
    pub role: String,
    /// Lista zdolnosci noda
    pub capabilities: Vec<String>,
    /// Zaladowane modele AI
    pub models: Vec<MeshModelInfo>,
    /// Dzialajace kontenery Docker
    pub containers: Vec<MeshContainerInfo>,
    /// Wektor wersji: pary (hash_noda, czas_logiczny)
    pub version_vector: Vec<(u64, u64)>,
    /// Platforma noda: "linux", "macos", "windows", "android", "ios"
    pub platform: String,
    /// Liczba rdzeni CPU
    pub cpu_count: u32,
    /// Czy Docker jest dostepny na nodzie
    pub docker_available: bool,
    /// Wersja Docker (pusty string jesli niedostepny)
    pub docker_version: String,
    /// Identyfikator clustera (jesli nod nalezy do clustera)
    pub cluster_id: Option<String>,
}

// =============================================================================
// Typ komendy zarzadzania
// =============================================================================

/// Rodzaj komendy zarzadzania wysylanej przez mesh do sparowanego noda.
/// Obejmuje operacje Docker, certyfikaty i serwisy.
#[derive(Clone, SerdeSerialize, SerdeDeserialize)]
pub enum MeshCommandType {
    /// Uruchomienie kontenera
    ContainerStart {
        container_id: String,
    },
    /// Zatrzymanie kontenera
    ContainerStop {
        container_id: String,
    },
    /// Restart kontenera
    ContainerRestart {
        container_id: String,
    },
    /// Lista kontenerow
    ListContainers,
    /// Lista obrazow Docker
    ListImages,
    /// Czyszczenie Docker (prune)
    SystemPrune {
        volumes: bool,
    },
    /// Wgranie certyfikatow TLS
    ProvisionCerts {
        cert_pem: String,
        key_pem: String,
        target_dir: String,
    },
    /// Dodanie serwisu na nodzie
    AddService {
        service_config: String,
    },
    /// Zmiana konfiguracji sieciowej na zdalnym nodzie
    NetworkConfig {
        interface: String,
        ipv4: Option<String>,
        netmask: Option<String>,
        gateway: Option<String>,
        dhcp: bool,
        sudo_password: String,
        /// Docelowe MTU karty (np. 9000 dla RoCE/jumbo frames). `None` zostawia
        /// MTU bez zmian. `#[serde(default)]` utrzymuje wire-compat ze starszymi
        /// nodami, ktore nie znaja tego pola.
        #[serde(default)]
        mtu: Option<u32>,
    },
    /// Probe przepustowosci sieci miedzy nodami (TCP multi-stream lub RDMA)
    BandwidthProbe {
        target_ip: String,
        target_port: u16,
        rdma_port: u16,
        bind_interface: String,
        duration_ms: u32,
        mode: String,
        nonce: Vec<u8>,
        num_streams: u8,
    },
    /// Anulowanie probing sesji
    BandwidthProbeCancel,

    /// Multi-source profiling: start sesji.
    ProfilingStart(ProfilingStartRequest),
    /// Multi-source profiling: stop sesji + zwrot pelnego raportu.
    ProfilingStop(ProfilingStopRequest),
    /// Multi-source profiling: lista sesji widocznych na nodzie.
    ProfilingSessions(ProfilingSessionsRequest),
    /// Multi-source profiling: pobranie raportu.
    ProfilingReport(ProfilingReportRequest),
    /// Multi-source profiling: usuniecie sesji.
    ProfilingDelete(ProfilingDeleteRequest),
    /// Multi-source profiling: pobranie tar.gz z calym katalogiem sesji.
    ProfilingDownload(ProfilingDownloadRequest),
    /// Multi-source profiling: snapshot aktywnej sesji (Some) albo None.
    ProfilingActiveInfo(ProfilingActiveInfoRequest),

    // -- Cross-node service action forwarding (krok N3b). `service_id` is
    //    interpreted in the receiver's local SQLite namespace; the receiver
    //    runs the action against its own DB and returns the result.
    ServiceStartRemote {
        service_id: i64,
    },
    ServiceDeleteRemote {
        service_id: i64,
    },
    ServicePinRemote {
        service_id: i64,
        pinned: bool,
    },
    ServicePauseRemote {
        service_id: i64,
        paused: bool,
    },
    /// Forwarded `ServiceManifestDeployRequest`. The receiver re-runs the same
    /// validation + tokio::spawn deploy that a local request would, and
    /// returns the synchronously generated `deploy_id` (slug). Logs continue
    /// to flow on the receiver's local websocket bus — cross-node log
    /// streaming is intentionally not part of N3b.
    ServiceDeployRemote {
        engine_id: String,
        deploy_method: String,
        config_json: String,
    },
    /// Forwarded `ServiceUpdateRequest`. Receiver wykonuje pełną logikę
    /// edycji serwisu (merge config_json, opcjonalny stop+respawn), tak
    /// samo jak handler na lokalnym nodzie. Zwraca status przez
    /// `ServiceActionResult` (success/failure z message).
    ServiceUpdateRemote {
        service_id: i64,
        model_repo: Option<String>,
        model_preset_id: Option<String>,
        gpu_memory_utilization: Option<f32>,
        max_model_len: Option<u32>,
        max_num_seqs: Option<u32>,
        max_num_batched_tokens: Option<u32>,
        kv_cache_dtype: Option<String>,
        chunked_prefill: Option<bool>,
        vllm_args_override: Option<String>,
        pinned: Option<bool>,
        paused: Option<bool>,
        restart_after_save: bool,
    },
    /// Forwarded web research request. Receiver executes it against its local
    /// SearXNG service and returns serialized WebResearchResponse JSON.
    WebResearch {
        request_json: String,
    },
    /// Forwarded vector operation. The receiver owns a Milvus service; it opens
    /// that local service by `service_id` (carried inside the CBOR) and runs the
    /// op against its own loopback Milvus, returning a `VectorOpResult`. The
    /// payload is an opaque minicbor `VectorOpRequest` (keeps this crate free of
    /// vector-type deps). New variant appended at END (ciborium index rule).
    VectorOp {
        request_cbor: Vec<u8>,
    },
    /// Forwarded subscription OAuth login start. The receiver owns the service
    /// and tokens, so it runs the device-code flow locally and returns an
    /// `OauthStartResult`. Appended at END (ciborium index rule).
    OauthStart {
        provider: String,
    },
    /// Forwarded subscription OAuth poll — receiver checks its local flow store
    /// and returns an `OauthPollResult`. Appended at END.
    OauthPoll {
        flow_id: String,
    },
    /// ML Studio: uruchom trening NA TYM nodzie (odbiorca). `spec_json` niesie
    /// pełną specyfikację (kind, dataset_dir, class_names, variant, hyperparams,
    /// output_dir). Odbiorca startuje swój lokalny serwis treningowy i śledzi
    /// job pod `run_id`. Odpowiedź: Empty (ok/error). Appended at END.
    MlTrainStart {
        run_id: String,
        spec_json: String,
    },
    /// ML Studio: status zdalnego treningu o `run_id` (odbiorca odpytuje swój
    /// lokalny serwis). Odpowiedź: `MlTrainStatusResult { status_json }`.
    MlTrainStatus {
        run_id: String,
    },
    /// ML Studio: transfer datasetu COCO przez mesh (chunk zip-a, content-addr
    /// po `dataset_hash`). Odbiorca składa zip i rozpakowuje do cache pod hashem.
    /// Dedup: gdy odbiorca MA już ten hash, zwraca `have_already=true` (nadawca
    /// przerywa). Odpowiedź: `MlDatasetChunkResult { have_already }`.
    MlDatasetChunk {
        dataset_hash: String,
        seq: u32,
        total: u32,
        data_b64: String,
    },
    /// ML Studio: detekcja NA TYM nodzie wytrenowanym tu modelem (checkpoint
    /// lokalny na odbiorcy). Inicjator (Node A) wysyła checkpoint_path + klasy +
    /// wariant + próg + obraz (base64); odbiorca woła swój lokalny serwis i
    /// zwraca `MlDetectResult`. Pozwala testować z A model żyjący na B.
    MlDetect {
        checkpoint_path: String,
        class_names_json: String,
        variant: String,
        threshold: f64,
        image_b64: String,
    },
    /// ML Studio: eksport GGUF NA TYM nodzie modelu, którego adapter żyje tutaj
    /// (wytrenowany lokalnie/przez mesh). `spec_json` niesie adapter_path/base_model/
    /// outtype/export_id. Odbiorca startuje eksport na swoim ml-training; inicjator
    /// odpytuje przez `MlExportStatus`. Odpowiedź: Empty (ok/error).
    MlExport {
        export_id: String,
        spec_json: String,
    },
    /// ML Studio: status zdalnego eksportu GGUF o `export_id`. Odpowiedź:
    /// `MlExportStatusResult { status_json }`.
    MlExportStatus {
        export_id: String,
    },
    /// ML Studio: zapytanie do modelu FT wdrożonego na odbiorcy (alias `model_name`
    /// w jego lokalnym routingu). Odbiorca odpala inferencję lokalnie i zwraca
    /// `MlChatResult`. Pozwala UŻYĆ z Node A modelu żyjącego na Node B. Appended at END.
    MlChat {
        model_name: String,
        message: String,
        max_tokens: u32,
    },
    /// ML Studio: zlecenie węzłowi-źródłu spakowania katalogu artefaktu `src_path`
    /// i wypchnięcia go (komendą `MlArtifactChunk`) do `target_node_id`. Odpowiedź:
    /// `MlArtifactPushResult { target_path }`. Appended at END.
    MlArtifactPushTo {
        src_path: String,
        target_node_id: String,
    },
    /// Cross-node robot control. The receiver owns the robot addon; it decodes
    /// the opaque `RobotControlRequest` (CBOR), re-checks trust + timing +
    /// permission, sanitizes the action and dispatches it to the local robot
    /// addon, returning a `RobotControlResult`. Opaque CBOR keeps this crate free
    /// of robot types (like `VectorOp`). Appended at END (ciborium index rule).
    RobotControl {
        request_cbor: Vec<u8>,
    },
    /// Enumeracja RoCE/RDMA interfejsow na nodzie (cluster-create network
    /// auto-config). Odbiorca czyta swoje `/sys/class/net/*/device/infiniband`
    /// i zwraca `RoceInterfaceList` z mapowaniem netdev->roce-device, IP, MTU,
    /// stanem linku i slotem PCI. Bezstanowa. Appended at END (ciborium index rule).
    RoceProbe,
    /// Distributed (multi-node tensor-parallel) deploy of ONE slice of a model on
    /// THIS node. The coordinator computes head/worker roles from cluster_members
    /// + ich D1 RoCE config i wysyla jeden `spec` na czlonka (kazdy z WLASNYM
    /// rdma_ip/devices/socket). Head serwuje endpoint OpenAI, workery dolaczaja do
    /// klastra Ray jako headless. Odbiorca uruchamia kontener z komenda
    /// `ray start ... && vllm serve ...` + NCCL env + flagami RDMA i zwraca
    /// `ServiceDeployDistributedResult`. Appended at END (ciborium index rule).
    ServiceDeployDistributed { spec: DistributedDeploySpec },
    /// Teardown jednego distributed-deploymentu na TYM nodzie (head lub worker).
    /// Odbiorca zatrzymuje+usuwa kontener i wiersz serwisu nalezacy do
    /// `deployment_cluster_id`. Czysci sesje Ray (kazdy nieudany vllm brudzi
    /// sesje), wiec redeploy startuje na czysto. Appended at END.
    ServiceStopDistributed { deployment_cluster_id: String },
    /// Sonduje REALNA gotowosc head-a distributed-deploymentu NA TYM nodzie:
    /// czy GCS Ray nasluchuje (`ray_port`), ilu nodow widzi klaster Ray
    /// (`ray status` w kontenerze head-a — weryfikacja dolaczenia workerow,
    /// P2-1) i czy endpoint OpenAI (`serve_port` `/v1/models`) zwraca 200.
    /// Koordynator odpytuje to z bounded timeoutem zamiast ufac samemu
    /// zaplanowaniu deployu (P1-1). Appended at END.
    DistributedReadiness {
        deployment_cluster_id: String,
        ray_port: u16,
        serve_port: u16,
        /// Oczekiwana liczba nodow Ray (= liczba czlonkow klastra).
        expected_nodes: u32,
    },
    /// Odpala `vllm serve` NA HEADZIE (przez `docker exec` w kontenerze head-a)
    /// DOPIERO gdy klaster Ray jest kompletny (wszystkie GPU dolaczyly). Rozdziela
    /// start GCS Ray od `vllm serve`, zeby vLLM nie czekal na nieobecne jeszcze
    /// GPU i nie timeoutowal. Odpowiedz: Empty. Appended at END.
    DistributedStartServe {
        deployment_cluster_id: String,
        serve_cmd: String,
    },
    /// P0 cluster deploy: upewnij sie, ze model `model_repo` jest kompletny w cache
    /// HF NA TYM nodzie (zwykle head). Jesli brak — odbiorca pobiera go kontenerem
    /// silnika `engine_id` (`snapshot_download`) z WLASNYM `HF_TOKEN` z secure
    /// setting (token NIGDY nie leci przez mesh). Odpowiedz: `EnsureModelResult`.
    /// `deployment_cluster_id` dowiazuje komende do aktywnego cluster-deployu —
    /// odbiorca autoryzuje ja wzgledem wspoldzielonego klastra (patrz executor).
    /// Appended at END (ciborium index rule).
    EnsureModelLocal {
        deployment_cluster_id: String,
        model_repo: String,
        engine_id: String,
    },
    /// P0 cluster deploy: czy model `model_repo` jest juz kompletny w cache HF NA
    /// TYM nodzie (worker sprawdza przed transferem z head-a). Odpowiedz:
    /// `ModelPresentResult { present }`. Appended at END.
    ModelPresentLocal {
        deployment_cluster_id: String,
        model_repo: String,
    },
    /// P0 cluster deploy: spakuj snapshot modelu `model_repo` z LOKALNEGO cache HF
    /// tego noda (source/head) i wypchnij go strumieniem ALPN_ARTIFACT do
    /// `target_node_id`, ktory zapisze go do swojego cache HF. Odpowiedz:
    /// `MlArtifactPushResult`. Appended at END.
    PushModelToPeer {
        deployment_cluster_id: String,
        model_repo: String,
        target_node_id: String,
    },
    /// Cross-node camera recordings listing. Node A asks paired node B for its
    /// recordings list. B resolves its own org context, applies the filters
    /// (serialized JSON — keeps this crate free of the recordings filter types)
    /// and returns `CameraRecordingsListResult { recordings_json }`. Appended
    /// at END (ciborium index rule).
    CameraRecordingsList {
        filters_json: String,
    },
    /// Cross-node camera recordings pull. Node A asks paired node B to stream the
    /// selected recording files back to `target_node_id` over ALPN_ARTIFACT. B
    /// validates each recording path (canonicalize + containment under its own
    /// recordings dir, reject symlinks), caps count + per-file size, streams each
    /// file with an artifact name key `recording|<ref>|<ext>` and returns
    /// `CameraRecordingPullResult { pulled_refs }`. The files themselves travel
    /// over ALPN_ARTIFACT, not in the CBOR response. Appended at END.
    CameraRecordingPull {
        recording_refs: Vec<String>,
        target_node_id: String,
    },
    /// ML Studio: anuluj zdalny trening o `run_id` (odbiorca woła `POST /cancel`
    /// swojego lokalnego serwisu treningowego). Odpowiedź: Empty (ok/error).
    /// Appended at END (ciborium index rule).
    MlTrainCancel {
        run_id: String,
    },
}

/// Per-node spec distributed-deployu policzony przez koordynatora z
/// `cluster_members` + per-node konfiguracji RoCE z D1 (`rdma_devices` = OBA
/// twins, `rdma_ip`, `rdma_socket_ifname`). Kazdy czlonek dostaje wlasny z jego
/// rola i adresacja. To jest tez kontrakt, ktorego potrzebuje frontend D4 do
/// zbudowania `ClusterDeployRequest` (koordynator wyprowadza z niego te spec-y).
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct DistributedDeploySpec {
    /// UUID grupujacy head + workery jednego deploymentu (teardown po nim).
    pub deployment_cluster_id: String,
    /// Cluster, z ktorego policzono czlonkow/role.
    pub cluster_id: String,
    /// Silnik (vllm/vllm-spark) — wybiera obraz i dialekt komendy.
    pub engine_id: String,
    /// "head" | "worker".
    pub role: String,
    /// Repo/preset modelu serwowanego przez head (workery laduja ten sam plik
    /// lokalnie — model jest juz obecny na kazdym nodzie w tym chunku).
    pub model: String,
    /// Nazwa modelu w routingu (`--served-model-name`).
    pub served_model_name: String,
    /// Tensor-parallel = laczna liczba GPU we wszystkich czlonkach.
    pub tp_size: u32,
    /// Liczba GPU na TYM nodzie (`ray start --num-gpus`).
    pub num_gpus: u32,
    /// Port endpointu OpenAI head-a (`vllm serve --port`). Ignorowany dla workera.
    pub port: u16,
    /// Port mastera torch.distributed (TCPStore) dla tensor-parallel — `VLLM_PORT`
    /// na KAZDYM czlonku. MUSI byc rozny od `port` (serve API) i przydzielony z tej
    /// samej puli `PortAllocator` co serve, zeby nie kolidowac z domyslnym 8000.
    #[serde(default)]
    pub dist_port: u16,
    /// `--gpu-memory-utilization`.
    pub gpu_memory_utilization: f32,
    /// `--max-model-len`.
    pub max_model_len: u32,
    /// IP RDMA head-a (`ray start --head --node-ip-address` na head;
    /// `ray start --address=<ray_head_ip>:<ray_port>` na workerze).
    pub ray_head_ip: String,
    /// Port GCS Ray (zwykle 6379).
    pub ray_port: u16,
    /// IP RDMA TEGO noda (`--node-ip-address`, `VLLM_HOST_IP`).
    pub rdma_ip: String,
    /// Lista urzadzen RoCE TEGO noda (OBA twins) → `NCCL_IB_HCA`.
    pub rdma_devices: String,
    /// Netdev QSFP TEGO noda → `NCCL_SOCKET_IFNAME`/`GLOO_SOCKET_IFNAME`.
    pub socket_ifname: String,
    /// Index GID RoCEv2 IPv4 tego noda → `NCCL_IB_GID_INDEX`. Persystowany
    /// per-czlonek (D1); nie hardkodowany. Domyslnie 3 (zweryfikowana wartosc
    /// RoCEv2 IPv4 na ConnectX-7 DGX Spark).
    pub gid_index: u32,
    /// `num_speculative_tokens` z presetu modelu. Wartosc jest wlasnoscia
    /// KONKRETNEGO checkpointu (dla DSpark musi rownac sie jego
    /// `dspark_block_size`, inaczej wyjscie jest bledne), wiec nie wolno jej
    /// zaszywac per silnik. `None` = zostaw domyslna silnika.
    #[serde(default)]
    pub speculative_num_tokens: Option<u32>,
    /// Domyslny sampling presetu jako JSON do `--override-generation-config`.
    /// `None` = manifest nie deklaruje, silnik zostaje przy swoim.
    #[serde(default)]
    pub generation_config_json: Option<String>,
    /// Dodatkowy config usera (vllm_args, gpu_select_mode, gpu_ids) jako JSON.
    pub config_json: String,
}

/// Pojedynczy RoCE/RDMA interfejs noda — wynik `RoceProbe`. Niesie zarowno
/// nazwe netdev (do `ip`/netplan), jak i nazwe urzadzenia RoCE (do `NCCL_IB_HCA`),
/// bo na DGX Spark jeden port QSFP = dwa netdevy + dwa urzadzenia RoCE ("twins")
/// dzielace jeden link PCIe.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct RoceInterfaceInfo {
    /// Nazwa netdev (np. `enP2p1s0f0np0`).
    pub netdev: String,
    /// Nazwa urzadzenia RoCE/IB (np. `roceP2p1s0f0`) — do `NCCL_IB_HCA`.
    pub roce_device: String,
    /// Pierwszy adres IPv4 netdev (None gdy karta UP bez adresu — "twin").
    pub ipv4: Option<String>,
    /// Maska sieciowa w notacji dotted, gdy IP jest ustawiony.
    pub netmask: Option<String>,
    /// Pozostale adresy IPv4 (sekundarne) tego netdev. Interconnect klastra moze
    /// byc adresem sekundarnym — bez tego dopasowanie primary by go pominelo.
    #[serde(default)]
    pub ipv4_aliases: Vec<String>,
    /// Aktualne MTU karty.
    pub mtu: u32,
    /// Czy link jest UP (carrier).
    pub link_up: bool,
    /// Predkosc linku w Mbps (0 gdy nieznana).
    pub speed_mbps: u64,
    /// Sciezka slotu PCI (realpath device symlink) — referencyjna.
    pub pci_slot: String,
    /// Klucz grupowania "twins" jednego fizycznego portu QSFP, wyliczony NA
    /// nodzie z najpewniejszego sygnalu: `phys_switch_id` (te same porty ASIC
    /// ConnectX), a gdy go brak — rodzic sciezki PCI. Pusty gdy nieznany.
    #[serde(default)]
    pub group_key: String,
    /// Indeks GID RoCE v2 / IPv4 tej karty, odczytany NA nodzie z
    /// `/sys/class/infiniband/<dev>/ports/1/gid_attrs`. Trafia do
    /// `NCCL_IB_GID_INDEX` — zalezy od sprzetu, wiec nie wolno go zakladac.
    /// `None` gdy nie da sie ustalic (starszy peer albo brak wpisu RoCE v2).
    #[serde(default)]
    pub gid_index: Option<u32>,
}

// =============================================================================
// Typed payload odpowiedzi na komende mesh
// =============================================================================

/// Typed payload odpowiedzi na `MeshCommandType`. Zastepuje `output: String`,
/// zeby kazda komenda miala scisle zdefiniowany typ wyniku — bez parsowania
/// JSON-a w warstwie aplikacyjnej.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub enum MeshCommandResponsePayload {
    /// Komendy void — zwracaja sam status (start/stop/restart/remove kontenera,
    /// add service, deploy stack, pull image, provision certs, bandwidth-cancel).
    Empty,
    /// Lista kontenerow zwracana przez `ListContainers`.
    ContainerList(Vec<MeshContainerInfo>),
    /// Lista obrazow zwracana przez `ListImages`.
    ImageList(Vec<String>),
    /// Wynik probing przepustowosci (server side: porty otwarte do polaczenia).
    BandwidthProbeServerStarted { tcp_port: u16, rdma_port: u16 },
    /// Wynik probing przepustowosci (client side: zmierzone metryki).
    BandwidthProbeClientResult {
        bandwidth_mbps: f64,
        bytes_transferred: u64,
        duration_ms: u64,
        latency_us: u64,
        streams_completed: u8,
        rdma: bool,
    },
    /// Nieforemny tekst — uzywany tylko dla `SystemPrune` (human-readable summary
    /// zwracane przez Docker daemon) i `NetworkConfig` (diagnostyczny output).
    Text(String),

    /// Multi-source profiling: potwierdzenie startu sesji.
    ProfilingStart(ProfilingStartResponse),
    /// Multi-source profiling: zatrzymanie + raport ProfileReportV2.
    ProfilingStop(ProfilingStopResponse),
    /// Multi-source profiling: lista sesji.
    ProfilingSessions(ProfilingSessionsResponse),
    /// Multi-source profiling: raport sesji.
    ProfilingReport(ProfilingReportResponse),
    /// Multi-source profiling: potwierdzenie usuniecia.
    ProfilingDelete(ProfilingDeleteResponse),
    /// Multi-source profiling: tar.gz katalogu sesji.
    ProfilingDownload(ProfilingDownloadResponse),
    /// Multi-source profiling: snapshot aktywnej sesji.
    ProfilingActiveInfo(ProfilingActiveInfoResponse),

    /// Cross-node service action result (stop/delete/pin/pause/rename) — the
    /// generic ok/error already lives in the outer `MeshCommandResponse`, so
    /// the payload is `Empty` for all five.
    ServiceActionResult,
    /// Cross-node deploy result — carries the slug allocated by the receiver
    /// so the initiator can wire the deploy log websocket back to that node.
    ServiceDeployResult {
        deploy_id: String,
        engine_id: String,
        deploy_method: String,
    },
    /// Serialized WebResearchResponse JSON produced by the receiver.
    WebResearchResult { response_json: String },
    /// ML Studio: status zdalnego treningu (JSON: status/epoch/total_epochs/
    /// train_loss/map50/map50_95/artifact_path/error) produkowany przez odbiorcę.
    MlTrainStatusResult { status_json: String },
    /// ML Studio: ack chunku datasetu. `have_already` = odbiorca ma już ten
    /// dataset (hash) → nadawca może przerwać transfer.
    MlDatasetChunkResult { have_already: bool },
    /// ML Studio: wynik detekcji na odbiorcy (detekcje JSON + wymiary obrazu;
    /// `error` gdy serwis zawiódł).
    MlDetectResult {
        detections_json: String,
        width: u32,
        height: u32,
        error: Option<String>,
    },
    /// Opaque minicbor `VectorOpResponse` produced by the receiver running a
    /// forwarded `VectorOp` against its local Milvus. Appended at END.
    VectorOpResult { result_cbor: Vec<u8> },
    /// Subscription OAuth login-start result from the receiver. Appended at END.
    OauthStartResult {
        flow_id: String,
        authorize_url: String,
        user_code: String,
        error: Option<String>,
    },
    /// Subscription OAuth poll result from the receiver. Appended at END.
    OauthPollResult {
        status: String,
        account_label: Option<String>,
        error: Option<String>,
    },
    /// ML Studio: status zdalnego eksportu GGUF (JSON: status/gguf_path/error)
    /// produkowany przez odbiorcę. Appended at END (kolejność = wire compat).
    MlExportStatusResult { status_json: String },
    /// ML Studio: odpowiedź modelu FT z odbiorcy (wygenerowany tekst lub `error`).
    /// Appended at END (kolejność = wire compat).
    MlChatResult {
        answer: String,
        error: Option<String>,
    },
    /// ML Studio: wynik wypchnięcia artefaktu do węzła docelowego — ścieżka
    /// katalogu artefaktu NA węźle docelowym. Appended at END.
    MlArtifactPushResult {
        target_path: String,
        error: Option<String>,
    },
    /// Opaque CBOR `RobotControlResponse` produced by the receiver running a
    /// forwarded `RobotControl` against its local robot addon. A rejection
    /// (timing/permission/unknown-robot) is carried as a successful response with
    /// the encoded `RobotControlResponse::rejected(...)`. Appended at END.
    RobotControlResult {
        result_cbor: Vec<u8>,
    },
    /// Wynik `RoceProbe` — lista RoCE/RDMA interfejsow noda. Appended at END.
    RoceInterfaceList(Vec<RoceInterfaceInfo>),
    /// Wynik `ServiceDeployDistributed` — slug deployu (do streamu logow na
    /// odbiorcy), nazwa kontenera i endpoint (Some tylko dla head). Appended at END.
    ServiceDeployDistributedResult {
        deploy_id: String,
        container_name: String,
        endpoint_url: Option<String>,
    },
    /// Wynik `DistributedReadiness` — realny stan gotowosci head-a. Appended at END.
    DistributedReadinessResult {
        /// Czy kontener deploymentu NA TYM nodzie dziala (obraz zbudowany +
        /// kontener wstal) — gate fazy buildu PRZED odliczaniem GCS/serve.
        container_running: bool,
        ray_gcs_up: bool,
        ray_nodes: u32,
        serve_ready: bool,
        error: Option<String>,
    },
    /// P0 cluster deploy: wynik `EnsureModelLocal` — sciezka snapshotu modelu w
    /// cache HF NA odbiorcy (pusta gdy `error`). Appended at END.
    EnsureModelResult {
        snapshot_dir: String,
        error: Option<String>,
    },
    /// P0 cluster deploy: wynik `ModelPresentLocal`. Appended at END.
    ModelPresentResult {
        present: bool,
    },
    /// Serialized recordings list JSON (`Vec<RemoteRecordingItem>`) produced by
    /// the receiver for `CameraRecordingsList`. Appended at END.
    CameraRecordingsListResult {
        recordings_json: String,
    },
    /// Recording refs the receiver successfully streamed over ALPN_ARTIFACT for
    /// `CameraRecordingPull`. Confirms which files travelled; the bytes are not
    /// in this CBOR. Appended at END.
    CameraRecordingPullResult {
        pulled_refs: Vec<String>,
    },
}

impl std::fmt::Debug for MeshCommandType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ContainerStart { container_id } => f
                .debug_struct("ContainerStart")
                .field("container_id", container_id)
                .finish(),
            Self::ContainerStop { container_id } => f
                .debug_struct("ContainerStop")
                .field("container_id", container_id)
                .finish(),
            Self::ContainerRestart { container_id } => f
                .debug_struct("ContainerRestart")
                .field("container_id", container_id)
                .finish(),
            Self::ListContainers => write!(f, "ListContainers"),
            Self::ListImages => write!(f, "ListImages"),
            Self::SystemPrune { volumes } => f
                .debug_struct("SystemPrune")
                .field("volumes", volumes)
                .finish(),
            Self::ProvisionCerts {
                cert_pem: _,
                key_pem: _,
                target_dir,
            } => f
                .debug_struct("ProvisionCerts")
                .field("cert_pem", &"[CERT]")
                .field("key_pem", &"***")
                .field("target_dir", target_dir)
                .finish(),
            Self::AddService { service_config } => f
                .debug_struct("AddService")
                .field("service_config", service_config)
                .finish(),
            Self::NetworkConfig {
                interface,
                ipv4,
                netmask,
                gateway,
                dhcp,
                sudo_password: _,
                mtu,
            } => f
                .debug_struct("NetworkConfig")
                .field("interface", interface)
                .field("ipv4", ipv4)
                .field("netmask", netmask)
                .field("gateway", gateway)
                .field("dhcp", dhcp)
                .field("sudo_password", &"***")
                .field("mtu", mtu)
                .finish(),
            Self::BandwidthProbe {
                target_ip, mode, ..
            } => f
                .debug_struct("BandwidthProbe")
                .field("target_ip", target_ip)
                .field("mode", mode)
                .finish(),
            Self::BandwidthProbeCancel => write!(f, "BandwidthProbeCancel"),
            Self::ProfilingStart(req) => f
                .debug_struct("ProfilingStart")
                .field("node_id", &req.node_id)
                .field("label", &req.label)
                .field("elevation_password", &"***")
                .finish(),
            Self::ProfilingStop(req) => f
                .debug_struct("ProfilingStop")
                .field("node_id", &req.node_id)
                .field("session_id", &req.session_id)
                .finish(),
            Self::ProfilingSessions(req) => f
                .debug_struct("ProfilingSessions")
                .field("node_id", &req.node_id)
                .finish(),
            Self::ProfilingReport(req) => f
                .debug_struct("ProfilingReport")
                .field("node_id", &req.node_id)
                .field("session_id", &req.session_id)
                .finish(),
            Self::ProfilingDelete(req) => f
                .debug_struct("ProfilingDelete")
                .field("node_id", &req.node_id)
                .field("session_id", &req.session_id)
                .finish(),
            Self::ProfilingDownload(req) => f
                .debug_struct("ProfilingDownload")
                .field("node_id", &req.node_id)
                .field("session_id", &req.session_id)
                .finish(),
            Self::ProfilingActiveInfo(req) => f
                .debug_struct("ProfilingActiveInfo")
                .field("node_id", &req.node_id)
                .finish(),
            Self::ServiceStartRemote { service_id } => f
                .debug_struct("ServiceStartRemote")
                .field("service_id", service_id)
                .finish(),
            Self::ServiceDeleteRemote { service_id } => f
                .debug_struct("ServiceDeleteRemote")
                .field("service_id", service_id)
                .finish(),
            Self::ServicePinRemote { service_id, pinned } => f
                .debug_struct("ServicePinRemote")
                .field("service_id", service_id)
                .field("pinned", pinned)
                .finish(),
            Self::ServicePauseRemote { service_id, paused } => f
                .debug_struct("ServicePauseRemote")
                .field("service_id", service_id)
                .field("paused", paused)
                .finish(),
            Self::ServiceDeployRemote {
                engine_id,
                deploy_method,
                ..
            } => f
                .debug_struct("ServiceDeployRemote")
                .field("engine_id", engine_id)
                .field("deploy_method", deploy_method)
                .finish(),
            Self::ServiceUpdateRemote {
                service_id,
                restart_after_save,
                ..
            } => f
                .debug_struct("ServiceUpdateRemote")
                .field("service_id", service_id)
                .field("restart_after_save", restart_after_save)
                .finish(),
            Self::WebResearch { request_json } => f
                .debug_struct("WebResearch")
                .field("request_len", &request_json.len())
                .finish(),
            Self::VectorOp { request_cbor } => f
                .debug_struct("VectorOp")
                .field("request_len", &request_cbor.len())
                .finish(),
            Self::OauthStart { provider } => f
                .debug_struct("OauthStart")
                .field("provider", provider)
                .finish(),
            Self::OauthPoll { .. } => write!(f, "OauthPoll"),
            Self::MlTrainStart { run_id, spec_json } => f
                .debug_struct("MlTrainStart")
                .field("run_id", run_id)
                .field("spec_len", &spec_json.len())
                .finish(),
            Self::MlTrainStatus { run_id } => f
                .debug_struct("MlTrainStatus")
                .field("run_id", run_id)
                .finish(),
            Self::MlTrainCancel { run_id } => f
                .debug_struct("MlTrainCancel")
                .field("run_id", run_id)
                .finish(),
            Self::MlDatasetChunk { dataset_hash, seq, total, data_b64 } => f
                .debug_struct("MlDatasetChunk")
                .field("hash", &&dataset_hash[..dataset_hash.len().min(12)])
                .field("seq", seq)
                .field("total", total)
                .field("chunk_len", &data_b64.len())
                .finish(),
            Self::MlDetect { checkpoint_path, variant, threshold, image_b64, .. } => f
                .debug_struct("MlDetect")
                .field("checkpoint", checkpoint_path)
                .field("variant", variant)
                .field("threshold", threshold)
                .field("image_len", &image_b64.len())
                .finish(),
            Self::MlExport { export_id, spec_json } => f
                .debug_struct("MlExport")
                .field("export_id", export_id)
                .field("spec_len", &spec_json.len())
                .finish(),
            Self::MlExportStatus { export_id } => f
                .debug_struct("MlExportStatus")
                .field("export_id", export_id)
                .finish(),
            Self::MlChat { model_name, max_tokens, message } => f
                .debug_struct("MlChat")
                .field("model_name", model_name)
                .field("max_tokens", max_tokens)
                .field("message_len", &message.len())
                .finish(),
            Self::MlArtifactPushTo { src_path, target_node_id } => f
                .debug_struct("MlArtifactPushTo")
                .field("src_path", src_path)
                .field("target_node_id", target_node_id)
                .finish(),
            Self::RobotControl { request_cbor } => f
                .debug_struct("RobotControl")
                .field("request_len", &request_cbor.len())
                .finish(),
            Self::RoceProbe => write!(f, "RoceProbe"),
            Self::ServiceDeployDistributed { spec } => f
                .debug_struct("ServiceDeployDistributed")
                .field("cluster", &spec.deployment_cluster_id)
                .field("engine", &spec.engine_id)
                .field("role", &spec.role)
                .field("tp_size", &spec.tp_size)
                .finish(),
            Self::ServiceStopDistributed {
                deployment_cluster_id,
            } => f
                .debug_struct("ServiceStopDistributed")
                .field("deployment_cluster_id", deployment_cluster_id)
                .finish(),
            Self::DistributedReadiness {
                deployment_cluster_id,
                ray_port,
                serve_port,
                expected_nodes,
            } => f
                .debug_struct("DistributedReadiness")
                .field("deployment_cluster_id", deployment_cluster_id)
                .field("ray_port", ray_port)
                .field("serve_port", serve_port)
                .field("expected_nodes", expected_nodes)
                .finish(),
            Self::DistributedStartServe {
                deployment_cluster_id,
                serve_cmd: _,
            } => f
                .debug_struct("DistributedStartServe")
                .field("deployment_cluster_id", deployment_cluster_id)
                .finish(),
            Self::EnsureModelLocal {
                deployment_cluster_id,
                model_repo,
                engine_id,
            } => f
                .debug_struct("EnsureModelLocal")
                .field("deployment_cluster_id", deployment_cluster_id)
                .field("model_repo", model_repo)
                .field("engine_id", engine_id)
                .finish(),
            Self::ModelPresentLocal {
                deployment_cluster_id,
                model_repo,
            } => f
                .debug_struct("ModelPresentLocal")
                .field("deployment_cluster_id", deployment_cluster_id)
                .field("model_repo", model_repo)
                .finish(),
            Self::PushModelToPeer {
                deployment_cluster_id,
                model_repo,
                target_node_id,
            } => f
                .debug_struct("PushModelToPeer")
                .field("deployment_cluster_id", deployment_cluster_id)
                .field("model_repo", model_repo)
                .field("target_node_id", target_node_id)
                .finish(),
            Self::CameraRecordingsList { filters_json } => f
                .debug_struct("CameraRecordingsList")
                .field("filters_json", filters_json)
                .finish(),
            Self::CameraRecordingPull {
                recording_refs,
                target_node_id,
            } => f
                .debug_struct("CameraRecordingPull")
                .field("recording_refs", recording_refs)
                .field("target_node_id", target_node_id)
                .finish(),
        }
    }
}

// =============================================================================
// Discriminant bytes dla identyfikacji wiadomosci na streamach QUIC
// =============================================================================

pub const MESH_MSG_HEARTBEAT: u8 = 0x10;
pub const MESH_MSG_FORWARD_REQ: u8 = 0x13;
pub const MESH_MSG_MODEL_LIST: u8 = 0x15;
pub const MESH_MSG_NODE_INFO: u8 = 0x18;
/// Minimal hello — hostname + platform. Wysylany przy kazdym PeerConnected
/// (trusted LUB discovered), zeby GUI mogl pokazac ludzka nazwe (spark-002)
/// zamiast skrotu hex przed zakonczeniem pairingu.
pub const MESH_MSG_HELLO: u8 = 0x19;
/// Gossip topologii mesh — floodowany z dedupem (origin, epoch) i TTL.
/// Kazdy zaufany peer broadcastuje swoj wpis co 30s; kazdy odbiorca rebroadcastuje
/// do swoich bezposrednich sasiadow (oprocz nadawcy). Dzieki temu mainpc dowiaduje sie
/// o spark-002 przez spark-001 — z nazwa, platforma i lista uslug.
pub const MESH_MSG_TOPOLOGY_ANNOUNCE: u8 = 0x1A;
/// Lekki anons 'oto kogo znam' — wysylany do nowo podlaczonego peera bez
/// wymagania zaufania. Rozwiazuje scenariusz 3 nodow na VLAN gdzie mDNS
/// multicast jest blokowany miedzy czescia klientow (typowe na enterprise
/// switches z IGMP snooping / client isolation). Tylko node_id + hostname +
/// adresy — bez uslug/modeli (pre-pairing = pre-trust).
pub const MESH_MSG_KNOWN_PEERS: u8 = 0x1B;
pub const MESH_MSG_PAIRING_REQUEST: u8 = 0x20;
pub const MESH_MSG_PAIRING_CONFIRM: u8 = 0x21;
pub const MESH_MSG_PAIRING_REJECT: u8 = 0x22;
pub const MESH_MSG_TRUST_REVOKED: u8 = 0x23;
pub const MESH_MSG_TRUSTED_KEYS_SYNC: u8 = 0x24;
pub const MESH_MSG_COMMAND: u8 = 0x30;
pub const MESH_MSG_COMMAND_RESPONSE: u8 = 0x31;
pub const MESH_MSG_DEPLOY_PROGRESS: u8 = 0x32;
pub const MESH_MSG_LOG_CHUNK: u8 = 0x33;
/// Online authority storage request for central-only addon data.
pub const MESH_MSG_STORAGE_PROXY_REQUEST: u8 = 0x34;
/// Online authority storage response for central-only addon data.
pub const MESH_MSG_STORAGE_PROXY_RESPONSE: u8 = 0x35;
pub const MESH_MSG_NODE_LEAVING: u8 = 0x27;
pub const MESH_MSG_FORWARD_STREAM_REQ: u8 = 0x38;
pub const MESH_MSG_ALIAS_SYNC: u8 = 0x39;
/// Pull request: nowo polaczony peer prosi o pelny snapshot serwisow.
pub const MESH_MSG_SERVICES_GET: u8 = 0x40;
/// Odpowiedz na `MESH_MSG_SERVICES_GET` — pelen snapshot lokalnego nodu.
pub const MESH_MSG_SERVICES_GET_RESPONSE: u8 = 0x41;
/// Periodyczny anti-drift broadcast pelnego stanu serwisow (co ~5min).
pub const MESH_MSG_SERVICES_ANNOUNCE: u8 = 0x42;
/// Push delta — pojedyncza zmiana (deploy/stop/pin/pause/rename/delete).
pub const MESH_MSG_SERVICES_UPDATE: u8 = 0x43;
/// Multi-node HMAC issuer key sync (F1b P3.B). Carries this peer's
/// pickup_token / frame_url / recording_url 32-byte HMAC keys (current +
/// optional previous-window key) so the receiver can verify tokens issued
/// by this peer. Sent only between trust-paired peers; verifier-only, never
/// used by the receiver for signing.
pub const MESH_MSG_HMAC_KEYS_SYNC: u8 = 0x44;

/// F1b P3.C — frame proxy request. Sent by a peer that received a signed
/// `frame_url` for a frame whose `raw_ref` is not held locally. The receiver
/// (the frame's owning node) looks up the frame in its local store and
/// replies with `MESH_MSG_FRAME_PROXY_RESPONSE`. Trusted-peer only; the
/// pre-trust whitelist intentionally excludes this discriminant.
pub const MESH_MSG_FRAME_PROXY_REQUEST: u8 = 0x45;

/// F1b P3.C — frame proxy response. Returns the encoded frame bytes plus the
/// wire-stable metadata mirror, or a typed miss (NotFound / Unavailable).
/// Trusted-peer only.
pub const MESH_MSG_FRAME_PROXY_RESPONSE: u8 = 0x46;
/// Sync Ledger push — paczka podpisanych operacji dla zaufanego peera.
pub const MESH_MSG_SYNC_PUSH: u8 = 0x47;
/// Sync Ledger ACK — potwierdzenie operacji zapisanych przez odbiorce.
pub const MESH_MSG_SYNC_ACK: u8 = 0x48;
/// Sync Ledger pull — prosba o zakres operacji z partycji.
pub const MESH_MSG_SYNC_PULL: u8 = 0x49;
/// Sync Ledger pull response — zakres operacji z partycji.
pub const MESH_MSG_SYNC_PULL_RESPONSE: u8 = 0x4A;
/// Sync Ledger snapshot pull — prosba o pakiet snapshotu partycji.
pub const MESH_MSG_SYNC_SNAPSHOT_PULL: u8 = 0x4B;
/// Sync Ledger snapshot response — metadane snapshotu, blob i tail operacji.
pub const MESH_MSG_SYNC_SNAPSHOT_RESPONSE: u8 = 0x4C;
/// Broadcast pelnego snapshotu trwalej konfiguracji routingu (klastry +
/// czlonkowie) po mutacji create/update/delete. Odbiorca tylko zapisuje
/// snapshot lokalnie — nigdy nie re-broadcastuje (anty-petla).
pub const MESH_MSG_ROUTING_SYNC: u8 = 0x4D;
/// Periodyczny anti-drift broadcast pelnego zestawu robotow nalezacych do
/// tego noda (`RobotsAnnouncePayload`). Mirror `MESH_MSG_SERVICES_ANNOUNCE` —
/// odbiorca robi `replace_node` w in-memory rejestrze robotow, dzieki czemu
/// resolver `resolve_robot_owner` widzi, ktory node posiada dany robot.
/// Trusted-peer only (nie ma go na liscie pre-trust).
pub const MESH_MSG_ROBOTS_ANNOUNCE: u8 = 0x4E;
/// Pull-on-connect: nowo polaczony peer prosi o pelny snapshot robotow tego
/// noda (`RobotsGetPayload`). Mirror `MESH_MSG_SERVICES_GET` — odbiorca
/// odpowiada `MESH_MSG_ROBOTS_GET_RESPONSE`. Trusted-peer only.
pub const MESH_MSG_ROBOTS_GET: u8 = 0x4F;
/// Odpowiedz na `MESH_MSG_ROBOTS_GET` — pelen snapshot robotow lokalnego noda
/// (`RobotsGetResponsePayload`). Mirror `MESH_MSG_SERVICES_GET_RESPONSE`.
/// Trusted-peer only.
pub const MESH_MSG_ROBOTS_GET_RESPONSE: u8 = 0x50;
/// Push delta — wysylane natychmiast po lokalnej zmianie zestawu robotow
/// (added/updated/removed). Mirror `MESH_MSG_SERVICES_UPDATE` — odbiorca
/// aplikuje `change` na swoim widoku noda przez `apply_change`. Trusted-peer
/// only.
pub const MESH_MSG_ROBOTS_UPDATE: u8 = 0x51;
/// Live camera relay subscribe — raw bi-stream discriminator (like
/// `MESH_MSG_FORWARD_STREAM_REQ`), trusted-peer only. The observer (B) opens a
/// QUIC bi-stream to the owner (A), writes
/// `[0x52][u32 id_len][req_id][CBOR CameraStreamSubscribePayload]`, then reads a
/// `[u32 len][CBOR CameraStreamFrame]` loop until the stream closes. This is
/// NOT a UFP/2 channel kind — it never travels as a UFP/2 unicast envelope, so
/// the channel-kind range is untouched (see `ufp2/discriminators.rs`).
pub const MESH_MSG_CAMERA_STREAM_SUBSCRIBE: u8 = 0x52;
/// Live LiDAR relay subscribe — raw bi-stream discriminator, mirror of
/// `MESH_MSG_CAMERA_STREAM_SUBSCRIBE`. The observer (B) opens a QUIC bi-stream to
/// the owner (A), writes `[0x53][u32 id_len][robot_id][CBOR LidarStreamSubscribePayload]`,
/// then reads a `[u32 len][CBOR LidarStreamFrame]` loop until the stream closes.
/// Like the camera relay this is NOT a UFP/2 channel kind — it never travels as a
/// UFP/2 unicast envelope, so the channel-kind range is untouched.
pub const MESH_MSG_LIDAR_STREAM_SUBSCRIBE: u8 = 0x53;

// =============================================================================
// Struktury wire format dla nowych wiadomosci mesh (CBOR zero-copy)
// =============================================================================

/// Observer→owner subscribe request body for the live camera relay bi-stream.
/// `camera_id` is the owner-local camera (e.g. `cam_xxx`, without the
/// `camera:` StreamHub prefix); `org_id` is the caller's tenant. Both ends
/// enforce org scope: the observer resolves the owner via `remote_camera_owner`
/// (org match) and the owner re-verifies the robot is advertised by itself in
/// this org before serving.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct CameraStreamSubscribePayload {
    pub camera_id: String,
    pub org_id: String,
}

/// One frame on the camera relay bi-stream. `is_init=true` carries the fMP4
/// init segment (ftyp+moov) delivered once at the start; `is_init=false` frames
/// are rolling media segments (moof+mdat) ready for `SourceBuffer.appendBuffer`.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct CameraStreamFrame {
    pub is_init: bool,
    /// `serde_bytes` → CBOR byte string (bulk copy), not an array-of-integers
    /// (per-element, ~100ns/byte). Same fix as `StreamFramePayload.data`: cross-node
    /// relay of ~hundreds-of-KB frames would otherwise serialize byte-by-byte.
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
}

/// Observer→owner subscribe request body for the live LiDAR relay bi-stream.
/// `robot_id` is the globally-unique addon-install id (== `addon_id`, single owner
/// org); `org_id` is the caller's tenant. Both ends enforce org scope: the
/// observer resolves the owner via `remote_lidar_owner` (org match) and the owner
/// re-verifies the robot is advertised by itself in this org before serving.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct LidarStreamSubscribePayload {
    pub robot_id: String,
    pub org_id: String,
}

/// One frame on the LiDAR relay bi-stream. Carries the raw canonical L1 frame
/// bytes (36-byte little-endian `LidarFrameHeader` + packed f32). Unlike the
/// camera relay there is NO `is_init` flag: LiDAR frames are self-describing, so
/// every frame is a complete, independently renderable point cloud and the
/// observer simply treats the latest received frame as its dynamic init segment.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct LidarStreamFrame {
    /// `serde_bytes` → CBOR byte string (bulk copy), not an array-of-integers.
    /// Same root-cause fix as `StreamFramePayload.data` / `CameraStreamFrame.data`:
    /// a ~300KB canonical cloud relayed cross-node must not serialize byte-by-byte.
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct TrustRevokedPayload {
    pub revoked_node_id: String,
    pub from_node_id: String,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct KeyRotationPayload {
    pub from_node_id: String,
    pub ephemeral_public_key: String,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct KeyRotationResponsePayload {
    pub from_node_id: String,
    pub ephemeral_public_key: String,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct TrustedKeyEntry {
    pub node_id: String,
    pub public_key_hex: String,
    /// Originating `approved_at` (the time the key was FIRST locally paired on the
    /// origin node), carried so a mirror re-add does not reset the trust-expiry TTL
    /// clock to "now". Empty when received from an un-upgraded peer (serde default),
    /// in which case the receiver falls back to its own current time.
    #[serde(default)]
    pub approved_at: String,
}

/// Minimal payload dla `MESH_MSG_HELLO` — tylko hostname + platform + OS.
/// Wysylany do kazdego peera (trusted/discovered) po nawiazaniu polaczenia,
/// zeby GUI mogl pokazac nazwe hosta przed zakończeniem pairingu.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshHelloPayload {
    pub hostname: String,
    pub platform: String,
    pub os_info: String,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct TrustedKeysSyncPayload {
    pub keys: Vec<TrustedKeyEntry>,
}

/// One scope of the local node's HMAC issuer key state, mirrored to a trusted
/// peer so it can verify tokens we issued.
///
/// `scope` is the wire-stable issuer name: "pickup_token", "frame_url",
/// "recording_url" (matches `services::key_storage` file names). `current_key`
/// is the active 32-byte HMAC secret. `previous_key` carries the still-valid
/// previous-window key after a rotation; `previous_expires_unix_ms = 0`
/// signals no previous-window key is active.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct HmacKeyEntry {
    pub scope: String,
    pub current_key: Vec<u8>,
    pub previous_key: Vec<u8>,
    pub previous_expires_unix_ms: u64,
    /// Truncated SHA-256 of `current_key` — diagnostic only, never used as
    /// trust input. Kept short (8 bytes) to keep log lines readable.
    pub key_id: Vec<u8>,
}

/// Payload of `MESH_MSG_HMAC_KEYS_SYNC` — one entry per issuer scope held by
/// the sender. F1b P3.B sends three: pickup_token, frame_url, recording_url.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct HmacKeysSyncPayload {
    pub from_node_id: String,
    pub keys: Vec<HmacKeyEntry>,
}

/// F1b P3.C — wire-stable mirror of `services::frame_storage::FrameMetadata`.
/// The in-memory struct uses a Rust `enum` for pixel format and an `Option`
/// for the PTS, which do not round-trip cleanly through every CBOR
/// derivation chain we support. The wire form keeps everything as primitives
/// + strings so receivers in any language / version pair safely.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct FrameMetadataWire {
    pub camera_id: String,
    pub width: u32,
    pub height: u32,
    /// Wire-stable pixel format name. Currently always "rgb24" (only
    /// variant produced by the F1a GStreamer pipeline); new connectors
    /// will add additional names here without breaking older receivers.
    pub pixel_format: String,
    pub timestamp_unix_ms: u64,
}

/// F1b P3.C — payload of `MESH_MSG_FRAME_PROXY_REQUEST`.
///
/// Two ways to address a frame on the owning node, distinguished by `camera_id`:
///   - by-ref — `camera_id` is `None`/empty: the caller already knows the
///     storage-layer `raw_ref` embedded in a signed `frame_url` (validated
///     before this message is sent). Used by the Service-to-Core pickup
///     mesh-fallback path.
///   - latest-for-camera — `camera_id` is `Some` and non-empty: the caller only
///     knows the NODE-LOCAL `camera_id` of a robot owned by the peer (camera
///     rows are never synced), not the peer's latest opaque `frame_<uuid>` ref;
///     `raw_ref` is then empty. The owner resolves the most recent frame for
///     that camera. Used by the dashboard live-tile cross-node path.
///
/// `request_id` is generated by the requester and copied back into the response
/// so the requester can match async replies — multiple in-flight requests share
/// the same uni-stream peer connection.
///
/// WIRE COMPAT: this is a struct (not an enum) so it is forward/backward
/// compatible across mixed-version mesh nodes. `camera_id` was APPENDED last as
/// an optional field — old nodes (which only send/expect `raw_ref` +
/// `request_id`) decode a payload from a new node by ignoring the trailing map
/// key, and a new node decoding an old payload defaults the missing `camera_id`
/// to `None` via `#[serde(default)]` (ciborium codec, same APPEND-AT-END rule as
/// every other mesh payload).
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct FrameProxyRequestPayload {
    pub raw_ref: String,
    pub request_id: String,
    #[serde(default)]
    pub camera_id: Option<String>,
}

/// F1b P3.C — payload of `MESH_MSG_FRAME_PROXY_RESPONSE`.
///
/// `Found` carries the encoded frame bytes + metadata. `NotFound` is sent
/// when the frame's owning node has no record of `raw_ref` (already
/// evicted, never existed). `Unavailable` covers the mid-fetch failure
/// case (source connector disconnected while we were assembling the
/// response, IO error, etc.) so the requester can distinguish a hard miss
/// from a transient one.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub enum FrameProxyResponsePayload {
    Found {
        raw_ref: String,
        request_id: String,
        bytes: Vec<u8>,
        metadata: FrameMetadataWire,
    },
    NotFound {
        raw_ref: String,
        request_id: String,
    },
    Unavailable {
        raw_ref: String,
        request_id: String,
        reason: String,
    },
}

/// Wire payload dla `MESH_MSG_PAIRING_REQUEST` — wysylany przez istniejacy mesh
/// stream przez inicjatora parowania. `from_node_id` to Ed25519 pubkey hex
/// (= iroh endpoint id). `public_key` to X25519 pubkey hex uzywany do KEX.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshPairingRequestPayload {
    pub from_node_id: String,
    pub public_key: String,
    pub pin: String,
}

/// Payload pierwszego kontaktu na osobnym ALPN `tentaflow-pairing/v2`.
/// Kodowany CBOR-em, nie JSON-em. `sender_node_id` musi byc rowny iroh
/// `remote_id`, a pierwsze 64 znaki `sender_public_key_hex` musza byc tym
/// samym Ed25519 public key.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct PairingFirstContactRequest {
    pub sender_node_id: String,
    pub sender_public_key_hex: String,
    pub sender_hostname: String,
    pub pin: String,
    pub sender_addresses: Vec<String>,
    pub sender_relay_url: String,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct PairingTrustedKeyEntry {
    pub node_id: String,
    pub public_key_hex: String,
    /// Originating `approved_at`, same purpose as `TrustedKeyEntry::approved_at`:
    /// a key propagated during first-contact pairing must not reset the receiver's
    /// trust-expiry TTL clock. Empty from un-upgraded peers (serde default).
    #[serde(default)]
    pub approved_at: String,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PairingFirstContactResponse {
    Confirm {
        receiver_public_key_hex: String,
        receiver_hostname: String,
        trusted_keys: Vec<PairingTrustedKeyEntry>,
    },
    Pending {
        receiver_hostname: String,
    },
    Reject {
        reason: String,
    },
}

/// Wire payload dla `MESH_MSG_PAIRING_CONFIRM` — wysylany w odpowiedzi przez
/// receivera po walidacji PIN-u przez admina.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshPairingConfirmPayload {
    pub from_node_id: String,
    pub public_key: String,
    pub hostname: String,
    pub pin: String,
}

/// Wire payload dla `MESH_MSG_PAIRING_REJECT` — wysylany gdy admin odrzuca prosbe.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshPairingRejectPayload {
    pub from_node_id: String,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct NodeLeavingPayload {
    pub node_id: String,
}

/// Podsumowanie uslugi dostepnej na zdalnym nodzie — wysylane w TopologyAnnounce.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct ServiceSummary {
    pub name: String,
    pub service_type: String,
    pub ready: bool,
}

/// Podsumowanie modelu zaladowanego na zdalnym nodzie — wysylane w TopologyAnnounce.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct ModelSummary {
    pub alias: String,
    pub backend: String,
    pub loaded: bool,
}

/// Jeden wpis w TopologyAnnounce — metadane noda + jego bezposredni sasiedzi + uslugi.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct TopologyEntry {
    pub node_id: String,
    pub hostname: String,
    pub platform: String,
    pub os_info: String,
    pub connected_to: Vec<String>,
    pub services: Vec<ServiceSummary>,
    pub models: Vec<ModelSummary>,
    pub direct_addrs: Vec<String>,
    pub port: u16,
}

/// Pojedynczy wpis w KnownPeersPayload — minimalne dane potrzebne zeby
/// spoznionemu nodowi udalo sie dial'nac peera bez polegania na mDNS.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct KnownPeerEntry {
    pub node_id: String,
    pub hostname: String,
    pub direct_addrs: Vec<String>,
    pub port: u16,
}

/// Payload KnownPeers — wysylany po PeerConnected przez nowo podlaczonego
/// peera. Zawiera liste wszystkich aktualnie polaczonych peerow, zeby odbiorca
/// mogl proboxac sie z nimi polaczyc bez mDNS.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct KnownPeersPayload {
    pub peers: Vec<KnownPeerEntry>,
}

/// Payload gossip topologii — floodowany z dedupem.
/// `origin_node_id` + `epoch` identyfikuja unikalna wersje wiadomosci.
/// `ttl` zmniejszane przy kazdym rebroadcascie (start 5, drop przy 0).
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct TopologyAnnouncePayload {
    pub origin_node_id: String,
    pub epoch: u64,
    pub ttl: u8,
    pub entries: Vec<TopologyEntry>,
}

// =============================================================================
// Mesh services registry — wire payloads (krok N3a)
// =============================================================================
//
// Cross-node services sync flows over four discriminants 0x40..0x43. The full
// `ServiceInfo` struct lives in `message_body` (it is also returned by the
// local `ServiceListRequest`); we re-use it here so receivers can drop a
// snapshot straight into the in-memory `MeshServicesRegistry`.

/// Pull request: nowo polaczony peer prosi o pelny snapshot serwisow.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshServicesGetPayload {
    pub from_node_id: String,
}

/// Odpowiedz na `MeshServicesGetPayload` — pelen snapshot lokalnego nodu.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshServicesGetResponsePayload {
    pub from_node_id: String,
    pub services: Vec<crate::message_body::ServiceInfo>,
}

/// Periodyczny anti-drift broadcast (co ~5 min). Pelen stan zastepuje to co
/// odbiorca trzyma w `MeshServicesRegistry` dla danego nodu.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshServicesAnnouncePayload {
    pub from_node_id: String,
    pub services: Vec<crate::message_body::ServiceInfo>,
}

/// Push delta — wysylane natychmiast po lokalnej mutacji (deploy/stop/pin/
/// pause/rename/delete). Odbiorca aplikuje `change` na swoim widoku nodu.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshServicesUpdatePayload {
    pub from_node_id: String,
    pub change: crate::message_body::ServiceChange,
}

// =============================================================================
// Sync Ledger — wire payloads
// =============================================================================

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshSyncOperationWire {
    pub op_id: Vec<u8>,
    // Partition stays for routing/materialization; the chain axis is now the
    // authoring node's `node_seq`, carried inside the encoded operation body.
    // Empty string for a redacted op: the requester is not permitted to learn
    // which resource the op touches, so even the partition name is withheld.
    pub partition_id: String,
    pub node_seq: u64,
    // Full operation body (CBOR) for an op the requester is permitted to receive.
    // Empty when `redacted` is set — a redacted op carries no body at all.
    pub operation: Vec<u8>,
    // Set when the serving peer holds this chain position but the requester is
    // NOT a sync target for the underlying resource. It carries only the
    // signed chain proof (signature over `op_id`, plus `prev_node_hash`), which
    // lets the requester verify continuity and advance its node-frontier WITHOUT
    // ever seeing the resource body. `None` for a normal, fully-served op.
    #[serde(default)]
    pub redacted: Option<RedactedOperationWire>,
}

/// The minimal, signature-verifiable proof of a single chain position served in
/// place of a full operation the requester may not read. `op_id` (carried on the
/// enclosing `MeshSyncOperationWire`) IS the operation hash, and the signature is
/// over `op_id`, so the requester can verify authorship and link the chain via
/// `prev_node_hash` without the body. Because `op_id = blake3(body)` is a one-way
/// hash, nothing about the redacted resource leaks (no body, no partition, no
/// resource id, no HLC wall-clock).
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct RedactedOperationWire {
    pub actor_node_id: String,
    pub prev_node_hash: Option<[u8; 32]>,
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshSyncPushPayload {
    pub from_node_id: String,
    pub operations: Vec<MeshSyncOperationWire>,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshSyncAckPayload {
    pub from_node_id: String,
    pub operation_ids: Vec<Vec<u8>>,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshSyncPullPayload {
    pub from_node_id: String,
    // Authoring node whose chain we need from `from_node_seq` onward. Any peer
    // may relay another node's chain, so this is independent of `from_node_id`.
    pub target_node_id: String,
    pub from_node_seq: u64,
    pub limit: u32,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshSyncPullResponsePayload {
    pub from_node_id: String,
    pub target_node_id: String,
    pub from_node_seq: u64,
    pub operations: Vec<MeshSyncOperationWire>,
    // The lowest node_seq the serving peer can still relay from its node_log for
    // `target_node_id`. When the requester asked from a seq below this floor the
    // peer has compacted that prefix away and cannot fill the gap from the log, so
    // the requester must escalate to a snapshot pull instead of looping forever.
    // `0` means "no compaction floor" (peer can serve from seq 1).
    pub serving_floor_node_seq: u64,
    // The highest node_seq the serving peer holds for `target_node_id`. Once the
    // requester's frontier reaches this tip there is nothing more to pull, so the
    // catch-up repair is satisfied and can be cleared. `0` means the peer holds
    // nothing for that chain.
    pub serving_tip_node_seq: u64,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshSyncSnapshotPullPayload {
    pub from_node_id: String,
    pub partition_id: String,
    pub up_to_sequence: u64,
    pub snapshot_id: String,
    pub include_tail: bool,
    pub tail_limit: u32,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshSyncSnapshotResponsePayload {
    pub from_node_id: String,
    pub partition_id: String,
    pub up_to_sequence: u64,
    pub snapshot_id: String,
    pub snapshot_bytes: Vec<u8>,
    pub blob_bytes: Vec<u8>,
    pub operations_after_snapshot: Vec<MeshSyncOperationWire>,
}

// =============================================================================
// Sync Ledger — baseline-adopt pairing (faza A: definicje typow)
// =============================================================================
//
// Baseline adopt: gdy nowy node dolacza do mesh, wybierany jest donor, ktory
// przesyla pelny baseline core'a (snapshot tabel) w chunkach. Typy ponizej
// opisuja wire format negocjacji donora i transferu. Faza A dodaje wylacznie
// definicje + (de)serializacje; podpiecie do ledgera nastapi w fazie B.

/// Monotoniczny epoch baseline'u. Canonical total order: a HIGHER `counter`
/// wins; on a tie the LOWER `origin_node` (lexicographically) wins. The order is
/// what every node converges to under `EpochMismatch` reconciliation — exactly
/// one epoch (the global maximum by this order) survives a mesh-wide reset and
/// every other node adopts it. The low-origin tie-break is deliberate: after a
/// migration where each node independently bumps to `counter:1` under its own
/// `origin_node`, the winner is the node with the lowest `node_id`, which is the
/// same node the baseline-adopt election (`decide_roles`) picks as donor, so the
/// epoch winner and the data donor agree without extra negotiation.
#[derive(Debug, Clone, Default, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BaselineEpoch {
    pub counter: u64,
    pub origin_node: String,
}

impl BaselineEpoch {
    /// `true` iff `self` is the canonical winner over `other`: higher `counter`,
    /// or equal `counter` with a lexicographically LOWER `origin_node`. Equal
    /// epochs do not win over each other. This is the single decision the
    /// `EpochMismatch` reconciler uses to choose which side adopts the other's
    /// baseline, so it must be antisymmetric and transitive (covered by tests).
    pub fn wins_over(&self, other: &Self) -> bool {
        matches!(self.cmp(other), std::cmp::Ordering::Greater)
    }
}

impl Ord for BaselineEpoch {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Higher counter is greater; on a tie a LOWER origin_node is greater, so
        // the canonical winner (lowest origin at equal counter) is the maximum.
        self.counter
            .cmp(&other.counter)
            .then_with(|| other.origin_node.cmp(&self.origin_node))
    }
}

impl PartialOrd for BaselineEpoch {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Propozycja donora baseline'u wyslana przez dolaczajacy node.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct BaselineElect {
    pub node_id: String,
    pub proposed_donor: String,
    pub epoch_seen: u64,
    /// Number of ledger operations the sender (the dialing node) currently holds.
    /// The donor side uses it to settle the role data-aware: the node with MORE
    /// content is the donor, so an empty node that dials a data-holder is told it
    /// is the joiner (it adopts), never the other way round — which would wipe the
    /// data-holder. `serde(default)` keeps the frame readable from peers that
    /// predate this field (they decode as `0` = "no content advertised").
    #[serde(default)]
    pub sender_op_count: u64,
}

/// Odpowiedz donora na `BaselineElect` — akceptacja albo odrzucenie roli donora.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct BaselineAck {
    pub accepted: bool,
    pub donor: String,
    pub joiner: String,
    pub epoch: u64,
}

/// Naglowek transferu baseline'u — opisuje co i ile zostanie przeslane przed
/// pierwszym chunkiem.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct BaselineHeader {
    pub schema_version: u32,
    pub epoch: u64,
    pub tables: Vec<String>,
    pub row_counts: Vec<u64>,
    pub total_bytes: u64,
    pub max_bytes: u64,
    /// blake3 hash of the FULL reassembled snapshot. Per-chunk hashes only catch
    /// a corrupted chunk in place; this catches a chunk reordered with a rewritten
    /// `seq`, a truncated tail, or any whole-stream tampering the joiner cannot see
    /// from the chunks alone.
    pub content_hash: [u8; 32],
}

/// Pojedynczy chunk baseline'u. `content_hash` to 32-bajtowy hash `bytes`,
/// weryfikowany przez odbiorce przed zlozeniem snapshotu.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct BaselineChunk {
    pub seq: u64,
    pub content_hash: [u8; 32],
    pub bytes: Vec<u8>,
}

/// Potwierdzenie pojedynczego chunka baseline'u.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct BaselineChunkAck {
    pub seq: u64,
    pub ok: bool,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub enum StorageValueWire {
    Null,
    Bool(bool),
    I64(i64),
    F64(f64),
    Text(String),
    Bytes(Vec<u8>),
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub enum StorageProxyRequestKind {
    SqlExec {
        query: String,
        params: Vec<StorageValueWire>,
    },
    SqlQuery {
        query: String,
        params: Vec<StorageValueWire>,
        one: bool,
        limit: Option<u32>,
    },
    KvGet {
        instance_id: String,
        key: String,
    },
    KvSet {
        instance_id: String,
        key: String,
        value: Vec<u8>,
    },
    KvDelete {
        instance_id: String,
        key: String,
    },
    KvList {
        instance_id: String,
        prefix: Option<String>,
    },
    BlobGetChunk {
        sha256: String,
        offset: u64,
        length: u32,
    },
    BlobPutChunk {
        blob_id: String,
        sha256: String,
        mime: String,
        size_bytes: u64,
        chunk_index: u32,
        chunk_count: u32,
        chunk_sha256: String,
        bytes: Vec<u8>,
    },
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct StorageProxyRequestPayload {
    pub request_id: String,
    pub from_node_id: String,
    pub org_id: String,
    pub addon_id: String,
    pub resource_type: String,
    pub resource_id: String,
    pub actor_user_id: Option<String>,
    pub kind: StorageProxyRequestKind,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub enum StorageProxyResponseKind {
    SqlExec {
        rows_affected: u64,
        last_insert_id: i64,
    },
    SqlRows {
        columns: Vec<String>,
        rows: Vec<Vec<StorageValueWire>>,
    },
    SqlOne {
        row: Option<Vec<StorageValueWire>>,
    },
    KvValue {
        value: Option<Vec<u8>>,
    },
    KvWrite {
        rows_affected: u64,
    },
    KvKeys {
        keys: Vec<String>,
    },
    BlobChunk {
        sha256: String,
        mime: String,
        size_bytes: u64,
        offset: u64,
        bytes: Vec<u8>,
    },
    BlobWrite {
        blob_id: String,
        sha256: String,
        complete: bool,
        received_chunks: u32,
    },
    Error {
        code: String,
        message: String,
    },
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct StorageProxyResponsePayload {
    pub request_id: String,
    pub from_node_id: String,
    pub kind: StorageProxyResponseKind,
}

// =============================================================================
// Typy protokolu meeting bot sidecar
// =============================================================================

/// Wiadomosc transkrypcji z sidecara meeting bot
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeetingTranscript {
    /// Nazwa mowcy
    pub speaker: String,
    /// Tekst transkrypcji
    pub text: String,
    /// Timestamp w milisekundach (unix epoch)
    pub timestamp_ms: u64,
}

/// Komenda mowienia wysylana do sidecara meeting bot (TTS)
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeetingSpeakCommand {
    /// Tekst do wypowiedzenia
    pub text: String,
    /// Identyfikator glosu TTS
    pub voice: String,
    /// Model TTS do uzycia
    pub model: String,
}

/// Kontrola spotkania — komendy i zdarzenia miedzy addonem a sidecarem
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub enum MeetingControl {
    /// Dolacz do spotkania pod podanym URL
    Join { meeting_url: String },
    /// Opusc spotkanie
    Leave,
    /// Wycisz/odcisz mikrofon
    Mute { muted: bool },
    /// Zmiana stanu spotkania (zdarzenie z sidecara)
    StateChanged { state: MeetingState },
    /// Healthcheck sidecara
    SidecarHealth { healthy: bool, uptime_s: u64 },
}

/// Stan spotkania raportowany przez sidecar
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub enum MeetingState {
    /// Laczenie ze spotkaniem
    Joining,
    /// Polaczony
    Connected,
    /// Ponowne laczenie po utracie polaczenia
    Reconnecting,
    /// Spotkanie zakonczone
    Ended { reason: String },
    /// Autoryzacja wygasla
    AuthExpired,
    /// Wyrzucony ze spotkania
    Kicked { reason: String },
}

// =============================================================================
// Helpery serializacji
// =============================================================================

impl MeshMessage {
    /// Serializacja do bajtow CBOR.
    pub fn serialize_cbor(&self) -> Result<Vec<u8>, String> {
        crate::cbor::encode(self)
    }

    /// Deserializacja z bajtow CBOR.
    pub fn deserialize_cbor(bytes: &[u8]) -> Result<MeshMessage, String> {
        crate::cbor::decode(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn epoch(counter: u64, origin: &str) -> BaselineEpoch {
        BaselineEpoch {
            counter,
            origin_node: origin.to_string(),
        }
    }

    #[test]
    fn higher_counter_epoch_wins_over_lower() {
        // A higher counter wins regardless of origin_node (even a "higher" origin).
        let high = epoch(2, "node_z");
        let low = epoch(1, "node_a");
        assert!(high.wins_over(&low));
        assert!(!low.wins_over(&high));
        assert!(high > low);
    }

    #[test]
    fn equal_counter_lower_origin_wins() {
        // The migration case: every node bumped to counter:1 under its own id. The
        // canonical winner is the LOWEST origin_node, so everyone converges to the
        // lowest-node-id epoch.
        let a = epoch(1, "node_a");
        let b = epoch(1, "node_b");
        assert!(a.wins_over(&b));
        assert!(!b.wins_over(&a));
        assert!(a > b);
    }

    #[test]
    fn epoch_ordering_total_and_deterministic() {
        let samples = [
            epoch(0, ""),
            epoch(1, "node_a"),
            epoch(1, "node_b"),
            epoch(1, "node_c"),
            epoch(2, "node_a"),
            epoch(2, "node_z"),
        ];
        for a in &samples {
            // Reflexive: an epoch never wins over itself.
            assert!(!a.wins_over(a));
            for b in &samples {
                // Antisymmetric: at most one direction wins; ties (equal epochs)
                // win in neither.
                if a == b {
                    assert!(!a.wins_over(b) && !b.wins_over(a));
                } else {
                    assert!(a.wins_over(b) ^ b.wins_over(a));
                }
                // wins_over agrees with Ord.
                assert_eq!(a.wins_over(b), a.cmp(b) == std::cmp::Ordering::Greater);
            }
        }
        // Transitive over a chain a > b > c.
        let a = epoch(2, "node_a");
        let b = epoch(1, "node_a");
        let c = epoch(1, "node_b");
        assert!(a.wins_over(&b) && b.wins_over(&c) && a.wins_over(&c));
    }

    #[test]
    fn migration_set_converges_to_lowest_node_id() {
        // N nodes each minted counter:1 under their own id. The unique max by the
        // canonical order is the epoch of the lexicographically lowest node_id —
        // the single epoch the whole mesh adopts.
        let epochs = [
            epoch(1, "node_c"),
            epoch(1, "node_a"),
            epoch(1, "node_d"),
            epoch(1, "node_b"),
        ];
        let winner = epochs.iter().max().expect("non-empty");
        assert_eq!(winner.origin_node, "node_a");
        for e in &epochs {
            if e != winner {
                assert!(winner.wins_over(e));
            }
        }
    }

    #[test]
    fn test_ping_roundtrip() {
        let msg = MeshMessage::Ping {
            from: "node-1".to_string(),
            incarnation: 42,
        };

        let bytes = msg
            .serialize_cbor()
            .expect("Serializacja ping powinna sie udac");
        let decoded =
            MeshMessage::deserialize_cbor(&bytes).expect("Deserializacja ping powinna sie udac");

        match decoded {
            MeshMessage::Ping { from, incarnation } => {
                assert_eq!(from.as_str(), "node-1");
                assert_eq!(incarnation, 42);
            }
            _ => panic!("Oczekiwano wariantu Ping"),
        }
    }

    #[test]
    fn test_join_roundtrip() {
        let msg = MeshMessage::Join {
            node_id: "node-2".to_string(),
            addr: "192.168.1.10:4433".to_string(),
            role: "worker".to_string(),
            capabilities: vec!["llm".to_string(), "embedding".to_string()],
        };

        let bytes = msg
            .serialize_cbor()
            .expect("Serializacja join powinna sie udac");
        let decoded =
            MeshMessage::deserialize_cbor(&bytes).expect("Deserializacja join powinna sie udac");

        match decoded {
            MeshMessage::Join {
                node_id,
                addr,
                role,
                capabilities,
            } => {
                assert_eq!(node_id.as_str(), "node-2");
                assert_eq!(addr.as_str(), "192.168.1.10:4433");
                assert_eq!(role.as_str(), "worker");
                assert_eq!(capabilities.len(), 2);
            }
            _ => panic!("Oczekiwano wariantu Join"),
        }
    }

    #[test]
    fn test_forward_roundtrip() {
        let payload = vec![1u8, 2, 3, 4, 5];
        let msg = MeshMessage::ForwardRequest {
            request_id: "req-001".to_string(),
            target_node: "node-5".to_string(),
            payload: payload.clone(),
        };

        let bytes = msg
            .serialize_cbor()
            .expect("Serializacja forward powinna sie udac");
        let decoded =
            MeshMessage::deserialize_cbor(&bytes).expect("Deserializacja forward powinna sie udac");

        match decoded {
            MeshMessage::ForwardRequest {
                request_id,
                target_node,
                payload: decoded_payload,
            } => {
                assert_eq!(request_id.as_str(), "req-001");
                assert_eq!(target_node.as_str(), "node-5");
                assert_eq!(decoded_payload.as_slice(), &[1, 2, 3, 4, 5]);
            }
            _ => panic!("Oczekiwano wariantu ForwardRequest"),
        }
    }

    // =========================================================================
    // Testy typow baseline-adopt
    // =========================================================================

    #[test]
    fn baseline_epoch_orders_by_counter_then_origin() {
        let a = BaselineEpoch {
            counter: 1,
            origin_node: "node_z".to_string(),
        };
        let b = BaselineEpoch {
            counter: 2,
            origin_node: "node_a".to_string(),
        };
        assert!(b > a);

        let same_counter_a = BaselineEpoch {
            counter: 5,
            origin_node: "node_a".to_string(),
        };
        let same_counter_b = BaselineEpoch {
            counter: 5,
            origin_node: "node_b".to_string(),
        };
        // At an equal counter the LOWER origin_node is the winner, so it must be
        // the maximum: that is the same node `decide_roles` elects as donor, and
        // the two decisions have to agree without further negotiation.
        assert!(same_counter_a > same_counter_b);
        assert!(same_counter_a.wins_over(&same_counter_b));
        assert!(!same_counter_b.wins_over(&same_counter_a));
    }

    #[test]
    fn baseline_epoch_roundtrip() {
        let epoch = BaselineEpoch {
            counter: 42,
            origin_node: "donor-1".to_string(),
        };
        let bytes = crate::cbor::encode(&epoch).expect("encode");
        let decoded = crate::cbor::decode::<BaselineEpoch>(&bytes).expect("decode");
        assert_eq!(decoded, epoch);
    }

    #[test]
    fn baseline_elect_and_ack_roundtrip() {
        let elect = BaselineElect {
            node_id: "joiner-1".to_string(),
            proposed_donor: "donor-1".to_string(),
            epoch_seen: 7,
            sender_op_count: 42,
        };
        let bytes = crate::cbor::encode(&elect).expect("encode");
        let decoded = crate::cbor::decode::<BaselineElect>(&bytes).expect("decode");
        assert_eq!(decoded.node_id, "joiner-1");
        assert_eq!(decoded.proposed_donor, "donor-1");
        assert_eq!(decoded.epoch_seen, 7);
        assert_eq!(decoded.sender_op_count, 42);

        let ack = BaselineAck {
            accepted: true,
            donor: "donor-1".to_string(),
            joiner: "joiner-1".to_string(),
            epoch: 7,
        };
        let bytes = crate::cbor::encode(&ack).expect("encode");
        let decoded = crate::cbor::decode::<BaselineAck>(&bytes).expect("decode");
        assert!(decoded.accepted);
        assert_eq!(decoded.epoch, 7);
    }

    #[test]
    fn baseline_header_and_chunk_roundtrip() {
        let header = BaselineHeader {
            schema_version: 52,
            epoch: 7,
            tables: vec!["flows".to_string(), "roles".to_string()],
            row_counts: vec![3, 5],
            total_bytes: 4096,
            max_bytes: 1_048_576,
            content_hash: [7u8; 32],
        };
        let bytes = crate::cbor::encode(&header).expect("encode");
        let decoded = crate::cbor::decode::<BaselineHeader>(&bytes).expect("decode");
        assert_eq!(decoded.tables, vec!["flows", "roles"]);
        assert_eq!(decoded.row_counts, vec![3, 5]);
        assert_eq!(decoded.schema_version, 52);

        let chunk = BaselineChunk {
            seq: 2,
            content_hash: [9u8; 32],
            bytes: vec![1, 2, 3, 4],
        };
        let bytes = crate::cbor::encode(&chunk).expect("encode");
        let decoded = crate::cbor::decode::<BaselineChunk>(&bytes).expect("decode");
        assert_eq!(decoded.seq, 2);
        assert_eq!(decoded.content_hash, [9u8; 32]);
        assert_eq!(decoded.bytes, vec![1, 2, 3, 4]);

        let ack = BaselineChunkAck { seq: 2, ok: true };
        let bytes = crate::cbor::encode(&ack).expect("encode");
        let decoded = crate::cbor::decode::<BaselineChunkAck>(&bytes).expect("decode");
        assert_eq!(decoded.seq, 2);
        assert!(decoded.ok);
    }

    // =========================================================================
    // Testy typow meeting bot
    // =========================================================================

    /// Pomocnicza makra do roundtrip testow CBOR dla typow meeting bot.
    /// Serializuje do bajtow i deserializuje z decoded — zwraca &Decoded.
    macro_rules! cbor_serialize {
        ($value:expr) => {
            crate::cbor::encode($value).expect("Serializacja CBOR powinna sie udac")
        };
    }

    #[test]
    fn test_meeting_transcript_roundtrip() {
        // Serializacja i deserializacja transkrypcji spotkania
        let transcript = MeetingTranscript {
            speaker: "Jan Kowalski".to_string(),
            text: "Dzien dobry, zaczynamy spotkanie.".to_string(),
            timestamp_ms: 1_710_000_000_000,
        };

        let bytes = cbor_serialize!(&transcript);
        let decoded = crate::cbor::decode::<MeetingTranscript>(&bytes).expect("decode");

        assert_eq!(decoded.speaker.as_str(), "Jan Kowalski");
        assert_eq!(decoded.text.as_str(), "Dzien dobry, zaczynamy spotkanie.");
        assert_eq!(decoded.timestamp_ms, 1_710_000_000_000);
    }

    #[test]
    fn test_meeting_transcript_empty_fields() {
        // Transkrypcja z pustymi polami
        let transcript = MeetingTranscript {
            speaker: "".to_string(),
            text: "".to_string(),
            timestamp_ms: 0,
        };

        let bytes = cbor_serialize!(&transcript);
        let decoded = crate::cbor::decode::<MeetingTranscript>(&bytes).expect("decode");

        assert_eq!(decoded.speaker.as_str(), "");
        assert_eq!(decoded.text.as_str(), "");
        assert_eq!(decoded.timestamp_ms, 0);
    }

    #[test]
    fn test_meeting_speak_command_roundtrip() {
        // Serializacja komendy mowienia TTS
        let cmd = MeetingSpeakCommand {
            text: "Prosze o ciszę.".to_string(),
            voice: "alloy".to_string(),
            model: "tts-1".to_string(),
        };

        let bytes = cbor_serialize!(&cmd);
        let decoded = crate::cbor::decode::<MeetingSpeakCommand>(&bytes).expect("decode");

        assert_eq!(decoded.text.as_str(), "Prosze o ciszę.");
        assert_eq!(decoded.voice.as_str(), "alloy");
        assert_eq!(decoded.model.as_str(), "tts-1");
    }

    #[test]
    fn test_meeting_control_join_roundtrip() {
        let ctrl = MeetingControl::Join {
            meeting_url: "https://teams.microsoft.com/l/meetup-join/abc".to_string(),
        };

        let bytes = cbor_serialize!(&ctrl);
        let decoded = crate::cbor::decode::<MeetingControl>(&bytes).expect("decode");

        match decoded {
            MeetingControl::Join { meeting_url } => {
                assert_eq!(
                    meeting_url.as_str(),
                    "https://teams.microsoft.com/l/meetup-join/abc"
                );
            }
            _ => panic!("Oczekiwano wariantu Join"),
        }
    }

    #[test]
    fn test_meeting_control_leave_roundtrip() {
        let ctrl = MeetingControl::Leave;

        let bytes = cbor_serialize!(&ctrl);
        let decoded = crate::cbor::decode::<MeetingControl>(&bytes).expect("decode");

        assert!(matches!(decoded, MeetingControl::Leave));
    }

    #[test]
    fn test_meeting_control_mute_roundtrip() {
        // Mute i unmute
        for muted_val in [true, false] {
            let ctrl = MeetingControl::Mute { muted: muted_val };

            let bytes = cbor_serialize!(&ctrl);
            let decoded = crate::cbor::decode::<MeetingControl>(&bytes).expect("decode");

            match decoded {
                MeetingControl::Mute { muted } => assert_eq!(muted, muted_val),
                _ => panic!("Oczekiwano wariantu Mute"),
            }
        }
    }

    #[test]
    fn test_meeting_control_state_changed_joining() {
        let ctrl = MeetingControl::StateChanged {
            state: MeetingState::Joining,
        };

        let bytes = cbor_serialize!(&ctrl);
        let decoded = crate::cbor::decode::<MeetingControl>(&bytes).expect("decode");

        match decoded {
            MeetingControl::StateChanged { state } => {
                assert!(matches!(state, MeetingState::Joining));
            }
            _ => panic!("Oczekiwano wariantu StateChanged"),
        }
    }

    #[test]
    fn test_meeting_control_state_changed_connected() {
        let ctrl = MeetingControl::StateChanged {
            state: MeetingState::Connected,
        };

        let bytes = cbor_serialize!(&ctrl);
        let decoded = crate::cbor::decode::<MeetingControl>(&bytes).expect("decode");

        match decoded {
            MeetingControl::StateChanged { state } => {
                assert!(matches!(state, MeetingState::Connected));
            }
            _ => panic!("Oczekiwano wariantu StateChanged"),
        }
    }

    #[test]
    fn test_meeting_control_state_changed_reconnecting() {
        let ctrl = MeetingControl::StateChanged {
            state: MeetingState::Reconnecting,
        };

        let bytes = cbor_serialize!(&ctrl);
        let decoded = crate::cbor::decode::<MeetingControl>(&bytes).expect("decode");

        match decoded {
            MeetingControl::StateChanged { state } => {
                assert!(matches!(state, MeetingState::Reconnecting));
            }
            _ => panic!("Oczekiwano wariantu StateChanged"),
        }
    }

    #[test]
    fn test_meeting_control_state_changed_ended() {
        let ctrl = MeetingControl::StateChanged {
            state: MeetingState::Ended {
                reason: "host ended".to_string(),
            },
        };

        let bytes = cbor_serialize!(&ctrl);
        let decoded = crate::cbor::decode::<MeetingControl>(&bytes).expect("decode");

        match decoded {
            MeetingControl::StateChanged { state } => match state {
                MeetingState::Ended { reason } => {
                    assert_eq!(reason.as_str(), "host ended");
                }
                _ => panic!("Oczekiwano MeetingState::Ended"),
            },
            _ => panic!("Oczekiwano wariantu StateChanged"),
        }
    }

    #[test]
    fn test_meeting_control_state_changed_auth_expired() {
        let ctrl = MeetingControl::StateChanged {
            state: MeetingState::AuthExpired,
        };

        let bytes = cbor_serialize!(&ctrl);
        let decoded = crate::cbor::decode::<MeetingControl>(&bytes).expect("decode");

        match decoded {
            MeetingControl::StateChanged { state } => {
                assert!(matches!(state, MeetingState::AuthExpired));
            }
            _ => panic!("Oczekiwano wariantu StateChanged"),
        }
    }

    #[test]
    fn test_meeting_control_state_changed_kicked() {
        let ctrl = MeetingControl::StateChanged {
            state: MeetingState::Kicked {
                reason: "disruption".to_string(),
            },
        };

        let bytes = cbor_serialize!(&ctrl);
        let decoded = crate::cbor::decode::<MeetingControl>(&bytes).expect("decode");

        match decoded {
            MeetingControl::StateChanged { state } => match state {
                MeetingState::Kicked { reason } => {
                    assert_eq!(reason.as_str(), "disruption");
                }
                _ => panic!("Oczekiwano MeetingState::Kicked"),
            },
            _ => panic!("Oczekiwano wariantu StateChanged"),
        }
    }

    #[test]
    fn test_meeting_control_sidecar_health_roundtrip() {
        let ctrl = MeetingControl::SidecarHealth {
            healthy: true,
            uptime_s: 3600,
        };

        let bytes = cbor_serialize!(&ctrl);
        let decoded = crate::cbor::decode::<MeetingControl>(&bytes).expect("decode");

        match decoded {
            MeetingControl::SidecarHealth { healthy, uptime_s } => {
                assert!(healthy);
                assert_eq!(uptime_s, 3600);
            }
            _ => panic!("Oczekiwano wariantu SidecarHealth"),
        }
    }

    #[test]
    fn test_meeting_state_ended_with_reason() {
        let state = MeetingState::Ended {
            reason: "Meeting ended by host".to_string(),
        };

        let bytes = cbor_serialize!(&state);
        let decoded = crate::cbor::decode::<MeetingState>(&bytes).expect("decode");

        match decoded {
            MeetingState::Ended { reason } => {
                assert_eq!(reason.as_str(), "Meeting ended by host");
            }
            _ => panic!("Oczekiwano wariantu Ended"),
        }
    }

    #[test]
    fn test_meeting_state_kicked_with_reason() {
        let state = MeetingState::Kicked {
            reason: "Removed by moderator".to_string(),
        };

        let bytes = cbor_serialize!(&state);
        let decoded = crate::cbor::decode::<MeetingState>(&bytes).expect("decode");

        match decoded {
            MeetingState::Kicked { reason } => {
                assert_eq!(reason.as_str(), "Removed by moderator");
            }
            _ => panic!("Oczekiwano wariantu Kicked"),
        }
    }

    #[test]
    fn test_meeting_transcript_serde_json_roundtrip() {
        // Serializacja/deserializacja JSON transkrypcji
        let transcript = MeetingTranscript {
            speaker: "Anna Nowak".to_string(),
            text: "Test JSON roundtrip".to_string(),
            timestamp_ms: 999,
        };

        let json = serde_json::to_string(&transcript).unwrap();
        let result: MeetingTranscript = serde_json::from_str(&json).unwrap();
        assert_eq!(result.speaker, "Anna Nowak");
        assert_eq!(result.text, "Test JSON roundtrip");
        assert_eq!(result.timestamp_ms, 999);
    }

    #[test]
    fn test_meeting_control_serde_json_roundtrip() {
        // JSON roundtrip dla kazdego wariantu MeetingControl
        let controls = vec![
            MeetingControl::Join {
                meeting_url: "https://test".to_string(),
            },
            MeetingControl::Leave,
            MeetingControl::Mute { muted: true },
            MeetingControl::SidecarHealth {
                healthy: false,
                uptime_s: 0,
            },
        ];

        for ctrl in &controls {
            let json = serde_json::to_string(ctrl).unwrap();
            let result: MeetingControl = serde_json::from_str(&json).unwrap();
            assert_eq!(
                std::mem::discriminant(&result),
                std::mem::discriminant(ctrl)
            );
        }
    }

    #[test]
    fn test_serde_json_roundtrip() {
        let msg = MeshMessage::Ping {
            from: "node-1".to_string(),
            incarnation: 7,
        };

        let json = serde_json::to_string(&msg).expect("Serializacja JSON powinna sie udac");
        let deserialized: MeshMessage =
            serde_json::from_str(&json).expect("Deserializacja JSON powinna sie udac");

        match deserialized {
            MeshMessage::Ping { from, incarnation } => {
                assert_eq!(from, "node-1");
                assert_eq!(incarnation, 7);
            }
            _ => panic!("Oczekiwano wariantu Ping"),
        }
    }

    #[test]
    fn test_heartbeat_roundtrip() {
        let msg = MeshMessage::Heartbeat(MeshHeartbeat {
            node_id: "node-10".to_string(),
            hostname: "worker-01".to_string(),
            ip_addresses: vec!["192.168.1.10".to_string()],
            timestamp_ms: 1_710_000_000_000,
            cpu_usage_percent: 42.5,
            ram_used_mb: 16384,
            ram_total_mb: 32768,
            gpu_metrics: vec![
                MeshGpuMetric {
                    index: 0,
                    usage_percent: 87.3,
                    vram_used_mb: 20000,
                    vram_total_mb: 24576,
                    temperature_c: 72.0,
                },
                MeshGpuMetric {
                    index: 1,
                    usage_percent: 15.0,
                    vram_used_mb: 2048,
                    vram_total_mb: 24576,
                    temperature_c: 45.5,
                },
            ],
            load_avg_1m: 3.14,
            active_requests: 8,
            platform: "linux".to_string(),
            services_count: 3,
            docker_running: true,
        });

        let bytes = msg
            .serialize_cbor()
            .expect("Serializacja heartbeat powinna sie udac");
        let decoded = MeshMessage::deserialize_cbor(&bytes)
            .expect("Deserializacja heartbeat powinna sie udac");

        match decoded {
            MeshMessage::Heartbeat(hb) => {
                assert_eq!(hb.node_id.as_str(), "node-10");
                assert_eq!(hb.timestamp_ms, 1_710_000_000_000);
                assert_eq!(hb.gpu_metrics.len(), 2);
                assert_eq!(hb.gpu_metrics[0].index, 0);
                assert_eq!(hb.gpu_metrics[1].vram_total_mb, 24576);
                assert_eq!(hb.active_requests, 8);
            }
            _ => panic!("Oczekiwano wariantu Heartbeat"),
        }
    }

    #[test]
    fn test_full_state_roundtrip() {
        let msg = MeshMessage::FullStateExchange(MeshFullState {
            node_id: "node-20".to_string(),
            hostname: "gpu-farm-01".to_string(),
            ip_addresses: vec!["10.0.0.20".to_string()],
            role: "worker".to_string(),
            capabilities: vec!["llm".to_string(), "tts".to_string()],
            models: vec![MeshModelInfo {
                name: "llama3-70b".to_string(),
                size_bytes: 40_000_000_000,
                backend: "vllm".to_string(),
                max_context: 8192,
                quantization: "FP16".to_string(),
            }],
            containers: vec![MeshContainerInfo {
                id: "abc123".to_string(),
                name: "vllm-server".to_string(),
                image: "vllm/vllm:latest".to_string(),
                status: "running".to_string(),
                ports: vec!["8000:8000".to_string()],
                cpu_percent: 55.0,
                memory_mb: 4096,
            }],
            version_vector: vec![(0xCAFE, 200), (0xBEEF, 150)],
            platform: "linux".to_string(),
            cpu_count: 16,
            docker_available: true,
            docker_version: "24.0.7".to_string(),
            cluster_id: Some("gpu-farm".to_string()),
        });

        let bytes = msg
            .serialize_cbor()
            .expect("Serializacja full state powinna sie udac");
        let decoded = MeshMessage::deserialize_cbor(&bytes)
            .expect("Deserializacja full state powinna sie udac");

        match decoded {
            MeshMessage::FullStateExchange(state) => {
                assert_eq!(state.node_id.as_str(), "node-20");
                assert_eq!(state.role.as_str(), "worker");
                assert_eq!(state.capabilities.len(), 2);
                assert_eq!(state.models.len(), 1);
                assert_eq!(state.models[0].name.as_str(), "llama3-70b");
                assert_eq!(state.models[0].max_context, 8192);
                assert_eq!(state.containers.len(), 1);
                assert_eq!(state.containers[0].name.as_str(), "vllm-server");
                assert_eq!(state.version_vector.len(), 2);
            }
            _ => panic!("Oczekiwano wariantu FullStateExchange"),
        }
    }

    #[test]
    fn mesh_command_response_payload_variants_round_trip() {
        let payloads = vec![
            MeshCommandResponsePayload::Empty,
            MeshCommandResponsePayload::ImageList(vec!["img-a".into(), "img-b".into()]),
            MeshCommandResponsePayload::BandwidthProbeServerStarted {
                tcp_port: 5001,
                rdma_port: 5002,
            },
            MeshCommandResponsePayload::BandwidthProbeClientResult {
                bandwidth_mbps: 9876.5,
                bytes_transferred: 1_000_000,
                duration_ms: 2000,
                latency_us: 120,
                streams_completed: 4u8,
                rdma: false,
            },
            MeshCommandResponsePayload::Text("Total reclaimed space: 1.2GB".into()),
            MeshCommandResponsePayload::WebResearchResult {
                response_json: r#"{"type":"search","results":[]}"#.into(),
            },
        ];
        for p in payloads {
            let bytes = crate::cbor::encode(&p).expect("encode");
            crate::cbor::decode::<MeshCommandResponsePayload>(&bytes)
                .expect("decode");
        }
    }

    #[test]
    fn web_research_mesh_command_round_trip() {
        let command = MeshCommandType::WebResearch {
            request_json: r#"{"type":"search","query":"rust"}"#.into(),
        };
        let bytes = crate::cbor::encode(&command).expect("encode");
        let decoded = crate::cbor::decode::<MeshCommandType>(&bytes).expect("decode");

        match decoded {
            MeshCommandType::WebResearch { request_json } => {
                assert!(request_json.contains("\"query\":\"rust\""));
            }
            _ => panic!("Oczekiwano wariantu WebResearch"),
        }
    }

    #[test]
    fn sync_ledger_payloads_roundtrip_cbor() {
        let op = MeshSyncOperationWire {
            op_id: vec![7; 32],
            partition_id: "addon/contacts/persons/1".to_string(),
            node_seq: 4,
            operation: vec![1, 2, 3, 4],
            redacted: None,
        };
        let push = MeshSyncPushPayload {
            from_node_id: "node-a".to_string(),
            operations: vec![op.clone()],
        };
        let bytes = crate::cbor::encode(&push).expect("encode push");
        let decoded = crate::cbor::decode::<MeshSyncPushPayload>(&bytes)
            .expect("decode push");
        assert_eq!(decoded.operations[0].op_id, op.op_id);
        assert_eq!(decoded.operations[0].node_seq, 4);

        let ack = MeshSyncAckPayload {
            from_node_id: "node-b".to_string(),
            operation_ids: vec![vec![7; 32]],
        };
        let bytes = crate::cbor::encode(&ack).expect("encode ack");
        let decoded = crate::cbor::decode::<MeshSyncAckPayload>(&bytes)
            .expect("decode ack");
        assert_eq!(decoded.operation_ids.len(), 1);

        let pull = MeshSyncPullPayload {
            from_node_id: "node-a".to_string(),
            target_node_id: "node-b".to_string(),
            from_node_seq: 2,
            limit: 128,
        };
        let bytes = crate::cbor::encode(&pull).expect("encode pull");
        let decoded = crate::cbor::decode::<MeshSyncPullPayload>(&bytes)
            .expect("decode pull");
        assert_eq!(decoded.from_node_seq, 2);

        let response = MeshSyncPullResponsePayload {
            from_node_id: "node-b".to_string(),
            target_node_id: "node-b".to_string(),
            from_node_seq: 2,
            operations: vec![op],
            serving_floor_node_seq: 1,
            serving_tip_node_seq: 2,
        };
        let bytes = crate::cbor::encode(&response).expect("encode response");
        let decoded = crate::cbor::decode::<MeshSyncPullResponsePayload>(&bytes)
            .expect("decode response");
        assert_eq!(decoded.operations.len(), 1);

        let snapshot_pull = MeshSyncSnapshotPullPayload {
            from_node_id: "node-a".to_string(),
            partition_id: "addon/contacts/persons/1".to_string(),
            up_to_sequence: 10,
            snapshot_id: "snapshot-a".to_string(),
            include_tail: true,
            tail_limit: 64,
        };
        let bytes =
            crate::cbor::encode(&snapshot_pull).expect("encode snapshot pull");
        let decoded = crate::cbor::decode::<MeshSyncSnapshotPullPayload>(&bytes)
            .expect("decode snapshot pull");
        assert_eq!(decoded.snapshot_id, "snapshot-a");
        assert!(decoded.include_tail);

        let snapshot_response = MeshSyncSnapshotResponsePayload {
            from_node_id: "node-b".to_string(),
            partition_id: "addon/contacts/persons/1".to_string(),
            up_to_sequence: 10,
            snapshot_id: "snapshot-a".to_string(),
            snapshot_bytes: vec![1, 2, 3],
            blob_bytes: vec![4, 5, 6],
            operations_after_snapshot: vec![MeshSyncOperationWire {
                op_id: vec![9; 32],
                partition_id: "addon/contacts/persons/1".to_string(),
                node_seq: 11,
                operation: vec![7, 8],
                redacted: None,
            }],
        };
        let bytes = crate::cbor::encode(&snapshot_response).expect("encode snapshot response");
        let decoded =
            crate::cbor::decode::<MeshSyncSnapshotResponsePayload>(&bytes)
                .expect("decode snapshot response");
        assert_eq!(decoded.blob_bytes, vec![4, 5, 6]);
        assert_eq!(decoded.operations_after_snapshot.len(), 1);
    }

    // =========================================================================
    // F1b P3.C-1 — frame proxy wire payload roundtrips
    // =========================================================================

    fn sample_metadata() -> FrameMetadataWire {
        FrameMetadataWire {
            camera_id: "cam-front-door".into(),
            width: 1920,
            height: 1080,
            pixel_format: "rgb24".into(),
            timestamp_unix_ms: 1_715_000_000_123,
        }
    }

    #[test]
    fn test_frame_metadata_wire_roundtrip() {
        let meta = sample_metadata();
        let bytes = crate::cbor::encode(&meta).expect("encode metadata");
        let decoded =
            crate::cbor::decode::<FrameMetadataWire>(&bytes).expect("decode");
        assert_eq!(decoded.camera_id, "cam-front-door");
        assert_eq!(decoded.width, 1920);
        assert_eq!(decoded.height, 1080);
        assert_eq!(decoded.pixel_format, "rgb24");
        assert_eq!(decoded.timestamp_unix_ms, 1_715_000_000_123);
    }

    #[test]
    fn test_frame_proxy_request_payload_roundtrip() {
        // by-ref form: camera_id is None.
        let req = FrameProxyRequestPayload {
            raw_ref: "frame-store/cam-front-door/2026-05-15T10:00:00.123".into(),
            request_id: "req-abc-001".into(),
            camera_id: None,
        };
        let bytes = crate::cbor::encode(&req).expect("encode");
        let decoded = crate::cbor::decode::<FrameProxyRequestPayload>(&bytes)
            .expect("decode");
        assert_eq!(decoded.request_id, "req-abc-001");
        assert_eq!(decoded.raw_ref, "frame-store/cam-front-door/2026-05-15T10:00:00.123");
        assert_eq!(decoded.camera_id, None);
    }

    #[test]
    fn test_frame_proxy_request_latest_for_camera_roundtrip() {
        // latest-for-camera form: camera_id is Some, raw_ref empty.
        let req = FrameProxyRequestPayload {
            raw_ref: String::new(),
            request_id: "req-latest-1".into(),
            camera_id: Some("cam-uuid-7".into()),
        };
        let bytes = crate::cbor::encode(&req).expect("encode");
        let decoded = crate::cbor::decode::<FrameProxyRequestPayload>(&bytes)
            .expect("decode");
        assert_eq!(decoded.request_id, "req-latest-1");
        assert_eq!(decoded.camera_id.as_deref(), Some("cam-uuid-7"));
    }

    /// Wire-compat: a payload encoded by an OLD node (no `camera_id` field at
    /// all) must decode on a NEW node with `camera_id` defaulting to `None`.
    #[test]
    fn test_frame_proxy_request_decodes_legacy_without_camera_id() {
        #[derive(SerdeSerialize)]
        struct LegacyRequest {
            raw_ref: String,
            request_id: String,
        }
        let legacy = LegacyRequest {
            raw_ref: "frame-store/legacy/1".into(),
            request_id: "req-legacy".into(),
        };
        let bytes = crate::cbor::encode(&legacy).expect("encode legacy");
        let decoded = crate::cbor::decode::<FrameProxyRequestPayload>(&bytes)
            .expect("new node decodes legacy payload");
        assert_eq!(decoded.raw_ref, "frame-store/legacy/1");
        assert_eq!(decoded.request_id, "req-legacy");
        assert_eq!(decoded.camera_id, None);
    }

    #[test]
    fn test_frame_proxy_response_found_roundtrip() {
        let resp = FrameProxyResponsePayload::Found {
            raw_ref: "frame-store/cam-1/abc".into(),
            request_id: "req-1".into(),
            bytes: vec![0x10, 0x20, 0x30, 0x40, 0x50],
            metadata: sample_metadata(),
        };
        let bytes = crate::cbor::encode(&resp).expect("encode");
        let decoded = crate::cbor::decode::<FrameProxyResponsePayload>(&bytes)
            .expect("decode");
        match decoded {
            FrameProxyResponsePayload::Found {
                raw_ref,
                request_id,
                bytes,
                metadata,
            } => {
                assert_eq!(raw_ref, "frame-store/cam-1/abc");
                assert_eq!(request_id, "req-1");
                assert_eq!(bytes, vec![0x10, 0x20, 0x30, 0x40, 0x50]);
                assert_eq!(metadata.camera_id, "cam-front-door");
                assert_eq!(metadata.width, 1920);
            }
            other => panic!("expected Found, got {:?}", other),
        }
    }

    #[test]
    fn test_frame_proxy_response_not_found_roundtrip() {
        let resp = FrameProxyResponsePayload::NotFound {
            raw_ref: "frame-store/cam-1/missing".into(),
            request_id: "req-2".into(),
        };
        let bytes = crate::cbor::encode(&resp).expect("encode");
        let decoded = crate::cbor::decode::<FrameProxyResponsePayload>(&bytes)
            .expect("decode");
        match decoded {
            FrameProxyResponsePayload::NotFound {
                raw_ref,
                request_id,
            } => {
                assert_eq!(raw_ref, "frame-store/cam-1/missing");
                assert_eq!(request_id, "req-2");
            }
            other => panic!("expected NotFound, got {:?}", other),
        }
    }

    #[test]
    fn test_frame_proxy_response_unavailable_roundtrip() {
        let resp = FrameProxyResponsePayload::Unavailable {
            raw_ref: "frame-store/cam-1/abc".into(),
            request_id: "req-3".into(),
            reason: "source connector disconnected mid-fetch".into(),
        };
        let bytes = crate::cbor::encode(&resp).expect("encode");
        let decoded = crate::cbor::decode::<FrameProxyResponsePayload>(&bytes)
            .expect("decode");
        match decoded {
            FrameProxyResponsePayload::Unavailable {
                raw_ref,
                request_id,
                reason,
            } => {
                assert_eq!(raw_ref, "frame-store/cam-1/abc");
                assert_eq!(request_id, "req-3");
                assert_eq!(reason, "source connector disconnected mid-fetch");
            }
            other => panic!("expected Unavailable, got {:?}", other),
        }
    }
}

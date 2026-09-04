// =============================================================================
// Plik: tts/sherpa.rs
// Opis: Adapter sherpa-onnx VITS TTS przez crate sherpa-rs. Wkompilowany w
//       binarke tentaflow przez Cargo feature `inference-sherpa`. Zaczyna
//       od konfiguracji VITS Piper (model + tokens + opcjonalny espeak-ng-
//       data); generate zwraca surowe sample float32 + sample rate.
// =============================================================================

use anyhow::{anyhow, Context, Result};
use sherpa_rs::tts::{CommonTtsConfig, VitsTts, VitsTtsConfig};
use sherpa_rs::OnnxConfig;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tracing::info;

use super::piper_tokens::{generate_tokens_from_piper_json, heal_missing_space_token};
use super::{SynthesizeParams, SynthesizeResult, TtsEngine, TtsModelInfo};

/// Katalog cache na pobrane bundle VITS Piper. Wspolny prefix dla wszystkich
/// repozytoriow sherpa-onnx — kazde repo ma swoj podkatalog (zsanityzowana
/// nazwa repo). Lokalizacja: `<dirs::data_dir>/tentaflow/models/sherpa-onnx/`.
fn sherpa_cache_dir() -> PathBuf {
    let base = dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("tentaflow")
        .join("models")
        .join("sherpa-onnx");
    std::fs::create_dir_all(&base).ok();
    base
}

/// Repo HF z gwarantowanym katalogiem `espeak-ng-data/` — uzywany jako shared
/// fallback dla raw Piper voices (ktore maja tylko `<voice>.onnx` + `.onnx.json`,
/// bez espeak data). ~25 MB, sprawdzony bundle.
const ESPEAK_FALLBACK_REPO: &str = "csukuangfj/vits-piper-en_US-amy-medium";

/// Per-voice inference tuning for known-weak community voices. Piper ships
/// models with training-time defaults (noise_scale 0.667, length_scale 1.0),
/// but some fine-tunes trained on tiny datasets articulate poorly there —
/// whole words smear into neighbors (ASR hears "łódź" as "uc"/"łycz").
/// Slower tempo plus less sampling noise stabilizes articulation; measured
/// with a whisper-small round-trip (3/3 clean repetitions vs 0/3 on defaults).
/// Keyed by a fragment of the picked `<voice>.onnx` stem, so multi-voice
/// repos only affect the matching voice.
const VOICE_SYNTH_TUNING: &[(&str, f32, f32)] = &[
    // WitoldG/polish_piper_models — fine-tuned from the en_US lessac
    // checkpoint on ~10 h of speech; mushes [w] („ł”) and soft affricates
    // (ć/dź) at training defaults.
    ("jarvis_wg_glos", 1.3, 0.45),
];

/// Returns `(length_scale, noise_scale)` for a voice stem, if tuned.
fn voice_tuning(model_stem: &str) -> Option<(f32, f32)> {
    VOICE_SYNTH_TUNING
        .iter()
        .find(|(key, _, _)| model_stem.contains(key))
        .map(|(_, ls, ns)| (*ls, *ns))
}

/// Pobiera bundle VITS Piper z HuggingFace i przygotowuje go do uzycia przez
/// sherpa-onnx. Obsluguje dwa formaty repozytoriow:
///
/// 1. Sherpa-compatible bundle — w korzeniu `<voice>.onnx` + `tokens.txt`
///    + `espeak-ng-data/`. Pobranie jeden-do-jeden.
///
/// 2. Raw Piper voice — `<voice>.onnx` + `<voice>.onnx.json` (Piper config),
///    bez `tokens.txt` i czesto bez `espeak-ng-data/`. Funkcja wtedy:
///      a) generuje `tokens.txt` z `phoneme_id_map` w `.onnx.json`,
///      b) doklada `espeak-ng-data/` ze wspolnego cache (pobiera raz z
///         `ESPEAK_FALLBACK_REPO`, potem kopiuje per-repo).
///
/// Wieloplikowe repo (np. `WitoldG/polish_piper_models` z kilkoma voices w
/// podkatalogach) jest splaszczane: wybieramy alfabetycznie pierwszy `.onnx`
/// i pobieramy tylko pliki z jego podkatalogu, zapisujac je w korzeniu
/// lokalnego cache.
///
/// Cache jest idempotentny — jezeli `tokens.txt` + `<x>.onnx` juz istnieja,
/// funkcja zwraca natychmiast.
pub async fn prepare_model(repo_id: &str) -> Result<PathBuf> {
    let safe_name = repo_id
        .replace('/', "_")
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-' || *c == '.')
        .collect::<String>();
    let target = sherpa_cache_dir().join(&safe_name);
    std::fs::create_dir_all(&target).ok();

    // Idempotencja: jesli mamy juz tokens.txt + .onnx + espeak-ng-data,
    // konczymy bez sieciowych operacji.
    if target.join("tokens.txt").exists()
        && target.join("espeak-ng-data").is_dir()
        && find_file_with_ext(&target, ".onnx").is_some()
    {
        info!(
            "[sherpa-onnx] uzywam istniejacego cache: {}",
            target.display()
        );
        // Cache z wczesniejszej wersji moze nie miec wstrzyknietych metadanych
        // ONNX dla raw Piper voices — domykamy idempotentnie. tokens.txt moze
        // tez pochodzic z wersji pomijajacej phoneme spacji — naprawiamy
        // (patrz piper_tokens::heal_missing_space_token).
        let t = target.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            if let Some(onnx_json) = find_file_with_ext(&t, ".onnx.json") {
                heal_missing_space_token(&t.join("tokens.txt"), &onnx_json)?;
            }
            ensure_piper_onnx_metadata(&t)
        })
        .await
        .context("blocking task panic")??;
        return Ok(target);
    }

    let repo = repo_id.to_string();
    let target_clone = target.clone();
    info!("[sherpa-onnx] pobieranie {} -> {}", repo, target.display());

    tokio::task::spawn_blocking(move || -> Result<()> {
        download_and_prepare(&repo, &target_clone)?;
        ensure_piper_onnx_metadata(&target_clone)
    })
    .await
    .context("blocking task panic")??;

    Ok(target)
}

/// Klient HTTP do pobierania modeli z HF. Bez hf-hub (patrz `download_and_prepare`
/// — symlinki hf-hub padaja EPERM na iOS). User-agent jak reszta naszych downloadow.
fn hf_blocking_client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .user_agent(concat!("tentaflow/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(600))
        .connect_timeout(std::time::Duration::from_secs(30))
        .build()
        .context("build reqwest blocking client")
}

/// Lista plikow w repo HF przez publiczne API (`/api/models/<repo>`). Zwraca
/// `siblings[].rfilename`. Tylko HTTP GET — nic nie pisze na dysk (iOS-safe).
fn hf_list_files(client: &reqwest::blocking::Client, repo: &str) -> Result<Vec<String>> {
    let url = format!("https://huggingface.co/api/models/{repo}");
    let json: serde_json::Value = client
        .get(&url)
        .send()
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("HF API info({repo})"))?
        .json()
        .with_context(|| format!("parse HF model info ({repo})"))?;
    let files = json
        .get("siblings")
        .and_then(|s| s.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| {
                    s.get("rfilename")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(files)
}

/// Pobiera pojedynczy plik z repo HF (`resolve/main/<rfilename>`) wprost do
/// `dest`, streamingiem. Tworzy katalogi rodzica. Bez symlinkow — dziala na iOS.
fn hf_download_file(
    client: &reqwest::blocking::Client,
    repo: &str,
    rfilename: &str,
    dest: &Path,
) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create dir {}", parent.display()))?;
    }
    let url = format!("https://huggingface.co/{repo}/resolve/main/{rfilename}");
    let mut resp = client
        .get(&url)
        .send()
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("download {rfilename}"))?;
    let mut file =
        std::fs::File::create(dest).with_context(|| format!("create {}", dest.display()))?;
    std::io::copy(&mut resp, &mut file).with_context(|| format!("write {}", dest.display()))?;
    Ok(())
}

/// Sciaga pliki z HF i normalizuje katalog do formatu wymaganego przez
/// `SherpaTtsEngine::load_model`. Wykonywane w `spawn_blocking` bo uzywamy
/// `reqwest::blocking` (proste streaming IO bez async wariantu).
///
/// Pobieranie idzie przez `reqwest` (publiczne HF API + `resolve/main`), NIE
/// przez hf-hub: hf-hub linkuje snapshot->blob `std::os::unix::fs::symlink`,
/// a iOS sandbox zwraca na `symlink()` EPERM ("Operation not permitted") —
/// pobranie padalo i mylacy bail "nie zawiera pliku .onnx" leciał mimo ze plik
/// w repo jest. reqwest pisze plik wprost, bez symlinkow — dziala na iOS i wszedzie.
fn download_and_prepare(repo: &str, target: &Path) -> Result<()> {
    let client = hf_blocking_client()?;
    let files = hf_list_files(&client, repo)?;

    // Wybieramy pojedynczy voice: alfabetycznie pierwszy `.onnx` w repo.
    // Subdir tego pliku staje sie prefixem ktory zdejmujemy ze sciezek
    // wszystkich kopiowanych plikow (placzymy strukture).
    let mut onnx_candidates: Vec<&String> = files
        .iter()
        .filter(|f| f.ends_with(".onnx") && !f.starts_with('.'))
        .collect();
    onnx_candidates.sort();
    let onnx_path = onnx_candidates
        .first()
        .ok_or_else(|| anyhow!("repo {} nie zawiera zadnego pliku .onnx", repo))?;
    let voice_subdir: String = match onnx_path.rfind('/') {
        Some(idx) => onnx_path[..=idx].to_string(),
        None => String::new(),
    };
    // Stem wybranego voice bez ".onnx" (np. "pl_PL-jarvis_wg_glos-medium").
    // W PLASKICH multi-voice repo (WitoldG: jarvis/justyna/meski/zenski w
    // korzeniu, voice_subdir="") filtruje pliki .onnx/.onnx.json do JEDNEGO
    // glosu — bez tego pobieralibysmy wszystkie cztery, a tokens.txt
    // generowalby sie z losowego `.onnx.json` (objaw: wybrano jarvisa, tokeny
    // z zenski). Pliki wspoldzielone (tokens.txt, lexicon, espeak) przechodza.
    let voice_stem: String = onnx_path
        .strip_suffix(".onnx")
        .unwrap_or(onnx_path)
        .to_string();
    info!(
        "[sherpa-onnx] wybrany voice: {} (subdir: '{}')",
        onnx_path, voice_subdir
    );

    let mut got_onnx = false;
    let mut got_tokens = false;
    let mut got_onnx_json = false;

    for fname in &files {
        if fname.starts_with('.') {
            continue;
        }
        // Akceptujemy tylko pliki z wybranego voice_subdir lub z korzenia
        // (espeak-ng-data zawsze w korzeniu jesli istnieje).
        let in_voice = voice_subdir.is_empty() || fname.starts_with(&voice_subdir);
        let is_espeak = fname.starts_with("espeak-ng-data/");
        if !in_voice && !is_espeak {
            continue;
        }

        let rel = if !voice_subdir.is_empty() && fname.starts_with(&voice_subdir) {
            &fname[voice_subdir.len()..]
        } else {
            fname.as_str()
        };
        if rel.is_empty() {
            continue;
        }

        let is_required = rel == "tokens.txt"
            || rel.ends_with(".onnx")
            || rel.ends_with(".onnx.json")
            || fname.starts_with("espeak-ng-data/")
            || rel == "lexicon.txt"
            || rel == "dict_dir/lexicon.txt";
        if !is_required {
            continue;
        }

        // Plaskie multi-voice repo: odrzuc .onnx/.onnx.json INNEGO glosu niz
        // wybrany (voice_stem). Pliki wspoldzielone (tokens.txt, lexicon,
        // espeak) nie sa voice-specyficzne — przechodza.
        let is_voice_file = rel.ends_with(".onnx") || rel.ends_with(".onnx.json");
        if voice_subdir.is_empty() && is_voice_file && !fname.starts_with(&voice_stem) {
            continue;
        }

        // Splaszczamy: pliki z voice_subdir trafiaja do korzenia target,
        // espeak-ng-data zachowuje swoja strukture katalogu. Pobieramy wprost
        // do docelowej sciezki (reqwest, bez symlinkow hf-hub).
        let dst_rel = if is_espeak { fname.as_str() } else { rel };
        let dst = target.join(dst_rel);
        if let Err(e) = hf_download_file(&client, repo, fname, &dst) {
            info!("[sherpa-onnx] pomijam {}: {}", fname, e);
            continue;
        }

        if rel.ends_with(".onnx") && !rel.ends_with(".onnx.json") {
            got_onnx = true;
        }
        if rel == "tokens.txt" {
            got_tokens = true;
        }
        if rel.ends_with(".onnx.json") {
            got_onnx_json = true;
        }
    }

    if !got_onnx {
        anyhow::bail!("repo {} nie zawiera pliku .onnx", repo);
    }

    // Brak tokens.txt: probujemy wyprodukowac z `<voice>.onnx.json` (raw Piper).
    if !got_tokens {
        if !got_onnx_json {
            anyhow::bail!(
                "repo {} nie ma tokens.txt ani <voice>.onnx.json — nie da sie zbudowac tokenow",
                repo
            );
        }
        let onnx_json = find_file_with_ext(target, ".onnx.json").ok_or_else(|| {
            anyhow!(
                "oczekiwano <voice>.onnx.json w {} po pobraniu",
                target.display()
            )
        })?;
        info!(
            "[sherpa-onnx] generuje tokens.txt z {}",
            onnx_json.display()
        );
        generate_tokens_from_piper_json(&onnx_json, &target.join("tokens.txt"))?;
    }

    // Brak espeak-ng-data: dokladamy z shared cache. Piper voices zawsze
    // potrzebuja eSpeak phonemizera, wiec brak tego katalogu = brak dzwieku.
    let espeak_local = target.join("espeak-ng-data");
    if !espeak_local.is_dir() {
        let shared = ensure_shared_espeak_data()?;
        info!(
            "[sherpa-onnx] kopiuje espeak-ng-data z shared cache do {}",
            espeak_local.display()
        );
        copy_dir_recursive(&shared, &espeak_local).with_context(|| {
            format!(
                "kopiowanie espeak-ng-data z {} -> {}",
                shared.display(),
                espeak_local.display()
            )
        })?;
    }

    Ok(())
}

/// Raw Piper voices (`<voice>.onnx` + `<voice>.onnx.json`) nie maja metadanych
/// ONNX, ktorych wymaga loader VITS w sherpa-onnx (`sample_rate`, `n_speakers`,
/// `language`, `comment`). Bez nich `OfflineTtsVitsModel::Init` rzuca
/// "'sample_rate' does not exist in the metadata". Wstrzykujemy je raz, czytajac
/// wartosci z `<voice>.onnx.json` (Piper config) i dopisujac `metadata_props` do
/// protobuf modelu. Idempotentne (marker per voice). Voices z formatu
/// sherpa-bundle (bez `.onnx.json`) pomijamy — maja juz metadane.
fn ensure_piper_onnx_metadata(dir: &Path) -> Result<()> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let onnx = entry.path();
        let Some(name) = onnx.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if !name.ends_with(".onnx") || name.ends_with(".onnx.json") {
            continue;
        }
        let json = dir.join(format!("{name}.json"));
        if !json.exists() {
            continue;
        }
        let marker = dir.join(format!("{name}.sherpa-meta"));
        if marker.exists() {
            continue;
        }
        let meta = piper_metadata_from_json(&json)?;
        append_onnx_metadata(&onnx, &meta)
            .with_context(|| format!("wstrzykiwanie metadanych VITS do {}", onnx.display()))?;
        std::fs::write(&marker, b"1").ok();
        info!(
            "[sherpa-onnx] wstrzyknieto metadane VITS do {}",
            onnx.display()
        );
    }
    Ok(())
}

/// Buduje wpisy metadanych VITS z Piper `<voice>.onnx.json`. Wszystkie wartosci
/// sa stringami — ONNX `metadata_props` to mapa string→string, sherpa parsuje
/// inty z napisow.
fn piper_metadata_from_json(json_path: &Path) -> Result<Vec<(String, String)>> {
    let bytes =
        std::fs::read(json_path).with_context(|| format!("read {}", json_path.display()))?;
    let v: serde_json::Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse json {}", json_path.display()))?;

    let sample_rate = v
        .get("audio")
        .and_then(|a| a.get("sample_rate"))
        .and_then(|x| x.as_i64())
        .unwrap_or(22050);
    let n_speakers = v
        .get("num_speakers")
        .and_then(|x| x.as_i64())
        .unwrap_or(1)
        .max(1);
    let voice = v
        .get("espeak")
        .and_then(|e| e.get("voice"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    // `language` jest wymagane przez sherpa (brak defaultu); spadamy na espeak
    // voice gdy Piper json nie ma `language.code`.
    let language = v
        .get("language")
        .and_then(|l| l.get("code"))
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            if voice.is_empty() {
                "unknown".to_string()
            } else {
                voice.clone()
            }
        });

    Ok(vec![
        ("model_type".to_string(), "vits".to_string()),
        // `comment` musi zawierac "piper" — sherpa po tym wykrywa is_piper i
        // wlacza espeak phonemizer + interspersed blanks.
        ("comment".to_string(), "piper".to_string()),
        ("language".to_string(), language),
        ("voice".to_string(), voice),
        ("sample_rate".to_string(), sample_rate.to_string()),
        ("n_speakers".to_string(), n_speakers.to_string()),
        // Piper trenuje z blank tokenami; sherpa default 0 dalby zly prozodyjnie
        // / niezrozumialy dzwiek.
        ("add_blank".to_string(), "1".to_string()),
    ])
}

/// Dopisuje wpisy `metadata_props` (ModelProto field 14) na koniec
/// zserializowanego ONNX. Protobuf scala powtarzalne pola dopisane na koncu
/// wiadomosci, wiec nie musimy parsowac/przepisywac calego ModelProto. Raw Piper
/// `.onnx` nie ma `metadata_props`, wiec nie powstaja duplikaty kluczy.
fn append_onnx_metadata(onnx_path: &Path, entries: &[(String, String)]) -> Result<()> {
    let mut bytes =
        std::fs::read(onnx_path).with_context(|| format!("read {}", onnx_path.display()))?;
    for (key, value) in entries {
        // StringStringEntryProto { key = field 1, value = field 2 }
        let mut entry = Vec::new();
        pb_string_field(&mut entry, 1, key);
        pb_string_field(&mut entry, 2, value);
        // ModelProto.metadata_props = field 14 (length-delimited message)
        pb_len_field(&mut bytes, 14, &entry);
    }
    std::fs::write(onnx_path, &bytes).with_context(|| format!("write {}", onnx_path.display()))?;
    Ok(())
}

fn pb_varint(buf: &mut Vec<u8>, mut n: u64) {
    loop {
        let byte = (n & 0x7f) as u8;
        n >>= 7;
        if n != 0 {
            buf.push(byte | 0x80);
        } else {
            buf.push(byte);
            break;
        }
    }
}

fn pb_string_field(buf: &mut Vec<u8>, field: u64, s: &str) {
    pb_varint(buf, (field << 3) | 2);
    pb_varint(buf, s.len() as u64);
    buf.extend_from_slice(s.as_bytes());
}

fn pb_len_field(buf: &mut Vec<u8>, field: u64, data: &[u8]) {
    pb_varint(buf, (field << 3) | 2);
    pb_varint(buf, data.len() as u64);
    buf.extend_from_slice(data);
}

/// Pobiera (raz, idempotentnie) `espeak-ng-data/` ze znanego sherpa-compatible
/// repo i zwraca sciezke do lokalnego shared cache. Kolejne wywolania zwracaja
/// istniejacy katalog bez ruchu sieciowego.
fn ensure_shared_espeak_data() -> Result<PathBuf> {
    let shared_root = sherpa_cache_dir().join("_shared");
    let shared = shared_root.join("espeak-ng-data");
    if shared.is_dir()
        && shared
            .read_dir()
            .map(|mut it| it.next().is_some())
            .unwrap_or(false)
    {
        return Ok(shared);
    }
    std::fs::create_dir_all(&shared_root).ok();
    info!(
        "[sherpa-onnx] pobieranie shared espeak-ng-data z {}",
        ESPEAK_FALLBACK_REPO
    );

    // reqwest zamiast hf-hub — symlinki hf-hub padaja EPERM na iOS (patrz
    // `download_and_prepare`).
    let client = hf_blocking_client()?;
    let files = hf_list_files(&client, ESPEAK_FALLBACK_REPO)?;

    let mut copied_any = false;
    for fname in files {
        if !fname.starts_with("espeak-ng-data/") {
            continue;
        }
        let dst = shared_root.join(&fname);
        if let Err(e) = hf_download_file(&client, ESPEAK_FALLBACK_REPO, &fname, &dst) {
            info!("[sherpa-onnx] pomijam shared {}: {}", fname, e);
            continue;
        }
        copied_any = true;
    }
    if !copied_any {
        anyhow::bail!(
            "shared repo {} nie zawiera espeak-ng-data/",
            ESPEAK_FALLBACK_REPO
        );
    }
    Ok(shared)
}

/// Plytka rekurencyjna kopia katalogu plik-po-pliku. Wystarczajaca dla
/// `espeak-ng-data/` (~kilka tysiecy malych plikow). Symlinki nie dzialaja
/// na Windows bez admin'a, wiec robimy fizyczna kopie.
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst).with_context(|| format!("create dir {}", dst.display()))?;
    for entry in std::fs::read_dir(src).with_context(|| format!("read dir {}", src.display()))? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if ft.is_file() {
            std::fs::copy(&from, &to)
                .with_context(|| format!("copy {} -> {}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

/// Embedded TTS engine wokol sherpa-onnx VITS Piper. Loaduje model z
/// katalogu zawierajacego `<model>.onnx` + `tokens.txt` + opcjonalnie
/// `espeak-ng-data/` (wymagane dla wiekszosci VITS Piper voices).
pub struct SherpaTtsEngine {
    inner: Mutex<Option<VitsTts>>,
    model_info: Mutex<Option<TtsModelInfo>>,
    /// Podpowiedz ktory voice wybrac z wielogłosowego repo (np. preset
    /// `vits-piper-pl_PL-jarvis_wg_glos-medium`). `load_model` preferuje
    /// `<voice>.onnx`, ktorego stem jest zawarty w tej podpowiedzi; bez niej
    /// (lub gdy brak dopasowania) bierze pierwszy `.onnx` w katalogu.
    voice_hint: Mutex<Option<String>>,
}

impl Default for SherpaTtsEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl SherpaTtsEngine {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(None),
            model_info: Mutex::new(None),
            voice_hint: Mutex::new(None),
        }
    }

    /// Ustawia podpowiedz voice (preset/model_name) przed `load_model`.
    pub fn set_voice_hint(&self, hint: Option<&str>) {
        *self.voice_hint.lock().unwrap() = hint
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
    }
}

/// Wybiera `<voice>.onnx` z katalogu pasujacy do podpowiedzi voice (stem pliku
/// zawarty w `hint`, np. `pl_PL-jarvis_wg_glos-medium` w
/// `vits-piper-pl_PL-jarvis_wg_glos-medium`). Bez dopasowania spada na pierwszy
/// `.onnx` — zachowanie dla single-voice repo.
fn pick_onnx_for_voice(dir: &Path, hint: Option<&str>) -> Option<PathBuf> {
    if let Some(hint) = hint {
        let entries = std::fs::read_dir(dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if !name.ends_with(".onnx") || name.ends_with(".onnx.json") {
                continue;
            }
            let stem = name.trim_end_matches(".onnx");
            if !stem.is_empty() && hint.contains(stem) {
                return Some(path);
            }
        }
    }
    find_file_with_ext(dir, ".onnx")
}

/// Znajduje pierwszy plik o danym suffix w katalogu (przyklad: `.onnx` /
/// `tokens.txt`). Zwraca pelna sciezke albo None.
fn find_file_with_ext(dir: &Path, ext: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    // Specjalny przypadek: szukajac `.onnx` chcemy wykluczyc `.onnx.json`,
    // bo to plik konfiguracyjny Pipera, nie model.
    let exclude_onnx_json = ext == ".onnx";
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                if exclude_onnx_json && name.ends_with(".onnx.json") {
                    continue;
                }
                if name.ends_with(ext) {
                    return Some(path);
                }
            }
        }
    }
    None
}

impl TtsEngine for SherpaTtsEngine {
    fn backend_name(&self) -> &str {
        "sherpa-onnx"
    }

    fn load_model(&mut self, model_dir: &Path) -> Result<TtsModelInfo> {
        let hint = self.voice_hint.lock().unwrap().clone();
        let model_path = pick_onnx_for_voice(model_dir, hint.as_deref())
            .ok_or_else(|| anyhow!("brak pliku .onnx w {}", model_dir.display()))?;
        let tokens_path = model_dir.join("tokens.txt");
        if !tokens_path.exists() {
            anyhow::bail!("brak tokens.txt w {}", model_dir.display());
        }
        let espeak_dir = model_dir.join("espeak-ng-data");
        let data_dir_str = if espeak_dir.exists() {
            espeak_dir.to_string_lossy().into_owned()
        } else {
            String::new()
        };

        let model_stem = model_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let (length_scale, noise_scale) = voice_tuning(&model_stem).unwrap_or((1.0, 0.667));

        let config = VitsTtsConfig {
            model: model_path.to_string_lossy().into_owned(),
            tokens: tokens_path.to_string_lossy().into_owned(),
            data_dir: data_dir_str,
            length_scale,
            noise_scale,
            noise_scale_w: 0.8,
            silence_scale: 0.0,
            onnx_config: OnnxConfig {
                provider: "cpu".to_string(),
                num_threads: 2,
                debug: false,
                ..Default::default()
            },
            tts_config: CommonTtsConfig {
                max_num_sentences: 1,
                silence_scale: 0.0,
                ..Default::default()
            },
            ..Default::default()
        };

        let tts = VitsTts::new(config);
        // Sample rate poznajemy po pierwszej syntezie — ustawiamy domyslny
        // VITS 22050 Hz; faktyczna wartosc dopowiada SynthesizeResult.
        let info = TtsModelInfo {
            name: model_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("vits")
                .to_string(),
            backend: "sherpa-onnx".to_string(),
            sample_rate: 22050,
            speakers: 1,
        };

        *self.inner.lock().unwrap() = Some(tts);
        *self.model_info.lock().unwrap() = Some(info.clone());
        Ok(info)
    }

    fn synthesize(&self, params: SynthesizeParams) -> Result<SynthesizeResult> {
        let mut guard = self.inner.lock().unwrap();
        let tts = guard.as_mut().ok_or_else(|| anyhow!("model not loaded"))?;
        let audio = tts
            .create(&params.text, params.speaker_id, params.speed)
            .map_err(|e| anyhow!("sherpa create: {e:?}"))?;
        Ok(SynthesizeResult {
            samples: audio.samples,
            sample_rate: audio.sample_rate,
        })
    }

    fn model_info(&self) -> Option<&TtsModelInfo> {
        // Mutex nie pozwala na safe & — caller dostaje clone przez load_model.
        // Zwracamy None zeby nie naruszac borrow rules; w praktyce caller
        // trzyma zwrocony info z load_model.
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), b"\x08\x07").unwrap();
    }

    #[test]
    fn pick_onnx_for_voice_prefers_matching_voice() {
        let dir = tempfile::tempdir().unwrap();
        // Wielogłosowe repo — kilka voices obok siebie.
        touch(dir.path(), "pl_PL-jarvis_wg_glos-medium.onnx");
        touch(dir.path(), "pl_PL-justyna_wg_glos-medium.onnx");
        touch(dir.path(), "pl_PL-zenski_wg_glos-medium.onnx");
        // Piper config sibling — picker musi go pomijac (to nie model).
        touch(dir.path(), "pl_PL-jarvis_wg_glos-medium.onnx.json");

        let picked =
            pick_onnx_for_voice(dir.path(), Some("vits-piper-pl_PL-jarvis_wg_glos-medium"))
                .expect("powinien znalezc voice");
        assert_eq!(
            picked.file_name().and_then(|s| s.to_str()),
            Some("pl_PL-jarvis_wg_glos-medium.onnx"),
            "voice hint musi wybrac Jarvisa, nie pierwszy z dysku"
        );
    }

    #[test]
    fn pick_onnx_for_voice_falls_back_without_hint_or_match() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "only-voice.onnx");
        // Brak hinta → pierwszy .onnx.
        assert!(pick_onnx_for_voice(dir.path(), None).is_some());
        // Hint bez dopasowania → tez fallback na pierwszy (single-voice repo).
        assert!(pick_onnx_for_voice(dir.path(), Some("nieistniejacy-voice")).is_some());
    }

    #[test]
    fn voice_tuning_hits_jarvis_stem_only() {
        assert_eq!(
            voice_tuning("pl_PL-jarvis_wg_glos-medium"),
            Some((1.3, 0.45))
        );
        assert_eq!(voice_tuning("en_US-amy-medium"), None);
        assert_eq!(voice_tuning("pl_PL-gosia-medium"), None);
    }
}

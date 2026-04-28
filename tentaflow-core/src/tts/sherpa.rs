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
        return Ok(target);
    }

    let repo = repo_id.to_string();
    let target_clone = target.clone();
    info!("[sherpa-onnx] pobieranie {} -> {}", repo, target.display());

    tokio::task::spawn_blocking(move || -> Result<()> {
        download_and_prepare(&repo, &target_clone)
    })
    .await
    .context("blocking task panic")??;

    Ok(target)
}

/// Sciaga pliki z HF i normalizuje katalog do formatu wymaganego przez
/// `SherpaTtsEngine::load_model`. Wykonywane w `spawn_blocking` bo hf-hub
/// sync API blokuje, a synchroniczne IO jest tu prostsze niz async wariant.
fn download_and_prepare(repo: &str, target: &Path) -> Result<()> {
    use hf_hub::api::sync::Api;

    let api = Api::new().context("hf-hub Api::new")?;
    let r = api.model(repo.to_string());
    let info_repo = r
        .info()
        .with_context(|| format!("hf-hub info({})", repo))?;
    let files: Vec<String> = info_repo
        .siblings
        .into_iter()
        .map(|s| s.rfilename)
        .collect();

    // Wybieramy pojedynczy voice: alfabetycznie pierwszy `.onnx` w repo.
    // Subdir tego pliku staje sie prefixem ktory zdejmujemy ze sciezek
    // wszystkich kopiowanych plikow (placzymy strukture).
    let mut onnx_candidates: Vec<&String> = files
        .iter()
        .filter(|f| f.ends_with(".onnx") && !f.starts_with('.'))
        .collect();
    onnx_candidates.sort();
    let onnx_path = onnx_candidates.first().ok_or_else(|| {
        anyhow!("repo {} nie zawiera zadnego pliku .onnx", repo)
    })?;
    let voice_subdir: String = match onnx_path.rfind('/') {
        Some(idx) => onnx_path[..=idx].to_string(),
        None => String::new(),
    };
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

        let src = match r.get(fname) {
            Ok(p) => p,
            Err(e) => {
                info!("[sherpa-onnx] pomijam {}: {}", fname, e);
                continue;
            }
        };
        // Splaszczamy: pliki z voice_subdir trafiaja do korzenia target,
        // espeak-ng-data zachowuje swoja strukture katalogu.
        let dst_rel = if is_espeak { fname.as_str() } else { rel };
        let dst = target.join(dst_rel);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create dir {}", parent.display()))?;
        }
        std::fs::copy(&src, &dst)
            .with_context(|| format!("copy {} -> {}", src.display(), dst.display()))?;

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
            anyhow!("oczekiwano <voice>.onnx.json w {} po pobraniu", target.display())
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

/// Konwertuje Piper `.onnx.json` -> sherpa `tokens.txt`. Format Piper:
/// `phoneme_id_map: { "<phoneme>": [<id>, ...] }` — sherpa uzywa pierwszego
/// ID z tablicy. Phoneme zlozone z samych whitespace pomijamy, bo sherpa
/// parsuje tokens.txt po `split(' ')` i nie ma sposobu zakodowac tokena
/// "spacja" jednoznacznie.
fn generate_tokens_from_piper_json(json_path: &Path, out_path: &Path) -> Result<()> {
    let bytes = std::fs::read(json_path)
        .with_context(|| format!("read {}", json_path.display()))?;
    let v: serde_json::Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse json {}", json_path.display()))?;
    let map = v
        .get("phoneme_id_map")
        .and_then(|x| x.as_object())
        .ok_or_else(|| anyhow!("brak phoneme_id_map w {}", json_path.display()))?;

    let mut entries: Vec<(String, i64)> = Vec::with_capacity(map.len());
    for (phoneme, ids) in map.iter() {
        if phoneme.is_empty() || phoneme.chars().all(char::is_whitespace) {
            continue;
        }
        let first_id = ids
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|x| x.as_i64())
            .ok_or_else(|| {
                anyhow!(
                    "phoneme_id_map['{}'] nie jest tablica intow w {}",
                    phoneme,
                    json_path.display()
                )
            })?;
        entries.push((phoneme.clone(), first_id));
    }
    entries.sort_by_key(|(_, id)| *id);

    let mut out = String::with_capacity(entries.len() * 8);
    for (phoneme, id) in &entries {
        out.push_str(phoneme);
        out.push(' ');
        out.push_str(&id.to_string());
        out.push('\n');
    }
    std::fs::write(out_path, out)
        .with_context(|| format!("write {}", out_path.display()))?;
    Ok(())
}

/// Pobiera (raz, idempotentnie) `espeak-ng-data/` ze znanego sherpa-compatible
/// repo i zwraca sciezke do lokalnego shared cache. Kolejne wywolania zwracaja
/// istniejacy katalog bez ruchu sieciowego.
fn ensure_shared_espeak_data() -> Result<PathBuf> {
    use hf_hub::api::sync::Api;

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

    let api = Api::new().context("hf-hub Api::new (shared)")?;
    let r = api.model(ESPEAK_FALLBACK_REPO.to_string());
    let info_repo = r
        .info()
        .with_context(|| format!("hf-hub info({})", ESPEAK_FALLBACK_REPO))?;

    let mut copied_any = false;
    for s in info_repo.siblings {
        let fname = s.rfilename;
        if !fname.starts_with("espeak-ng-data/") {
            continue;
        }
        let src = match r.get(&fname) {
            Ok(p) => p,
            Err(e) => {
                info!("[sherpa-onnx] pomijam shared {}: {}", fname, e);
                continue;
            }
        };
        let dst = shared_root.join(&fname);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create dir {}", parent.display()))?;
        }
        std::fs::copy(&src, &dst)
            .with_context(|| format!("copy {} -> {}", src.display(), dst.display()))?;
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
    std::fs::create_dir_all(dst)
        .with_context(|| format!("create dir {}", dst.display()))?;
    for entry in std::fs::read_dir(src)
        .with_context(|| format!("read dir {}", src.display()))?
    {
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
        }
    }
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
        let model_path = find_file_with_ext(model_dir, ".onnx")
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

        let config = VitsTtsConfig {
            model: model_path.to_string_lossy().into_owned(),
            tokens: tokens_path.to_string_lossy().into_owned(),
            data_dir: data_dir_str,
            length_scale: 1.0,
            noise_scale: 0.667,
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

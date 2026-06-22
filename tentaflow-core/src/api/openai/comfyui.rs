// =============================================================================
// Plik: api/openai/comfyui.rs
// Opis: Minimalny klient HTTP dla ComfyUI uzywany przez `/v1/images/generations`.
//       Buduje text2img workflow SD1.5, kolejkuje go przez `/prompt`, czeka na
//       wynik przez `/history/<id>` i sciaga bajty obrazow przez `/view`.
// Przyklad:
//   let client = ComfyClient::new(base_url);
//   let pngs = client.text2img(&params).await?;
// =============================================================================

use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use tracing::{debug, warn};

/// Maksymalna liczba obrazow na jeden request. ComfyUI generuje batch w jednym
/// przebiegu KSamplera, ale duzy batch latwo wysyca VRAM — limit chroni przed
/// przypadkowym DoS na pojedynczy GPU.
pub const MAX_IMAGES: u32 = 8;

/// Domyslny checkpoint gdy nie da sie wykryc zaladowanego pliku ani nie podano
/// override w configu serwisu. SD1.5 to preset `sd-1-5` z manifestu comfyui.
const DEFAULT_CHECKPOINT: &str = "v1-5-pruned-emaonly.safetensors";

/// Parametry pojedynczego text2img — wszystko juz zwalidowane przez handler.
pub struct Text2ImgParams {
    pub prompt: String,
    pub negative_prompt: String,
    pub width: u32,
    pub height: u32,
    pub batch_size: u32,
    pub steps: u32,
    pub cfg: f32,
    pub sampler: String,
    pub scheduler: String,
    pub seed: u64,
    /// Nazwa pliku checkpointu (`.safetensors`/`.ckpt`) widziana przez
    /// `CheckpointLoaderSimple`. Gdy `None`, klient sam wykryje pierwszy
    /// zaladowany checkpoint przez `/object_info`, a w ostatecznosci uzyje
    /// `DEFAULT_CHECKPOINT`.
    pub checkpoint: Option<String>,
}

/// Blad klienta ComfyUI — zwracany do handlera, ktory mapuje go na odpowiedz
/// OpenAI error.
#[derive(Debug)]
pub enum ComfyError {
    /// Blad transportu HTTP (polaczenie, timeout pojedynczego zadania).
    Http(String),
    /// ComfyUI odrzucil workflow albo zwrocil blad wykonania.
    Backend(String),
    /// Przekroczono globalny budzet czasu na wygenerowanie obrazu.
    Timeout,
}

impl std::fmt::Display for ComfyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(m) => write!(f, "ComfyUI HTTP: {m}"),
            Self::Backend(m) => write!(f, "ComfyUI backend: {m}"),
            Self::Timeout => write!(f, "ComfyUI: przekroczono czas generowania obrazu"),
        }
    }
}

/// Odpowiedz `/prompt` — interesuje nas tylko `prompt_id`.
#[derive(Deserialize)]
struct PromptResponse {
    prompt_id: String,
}

/// Klient zwiazany z jednym serwisem ComfyUI (bazowy URL bez `/v1`).
pub struct ComfyClient {
    base: String,
    http: reqwest::Client,
    /// Identyfikator klienta wysylany do ComfyUI w `/prompt` — pozwala
    /// powiazac kolejkowane zadanie z tym requestem w historii.
    client_id: String,
}

impl ComfyClient {
    /// Tworzy klienta dla danego bazowego URL. Per-zadanie timeout jest krotki
    /// (samo kolejkowanie/polling jest szybkie); calkowity budzet czasu na
    /// generacje pilnuje petla `text2img`.
    pub fn new(base_url: impl Into<String>) -> Result<Self, ComfyError> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| ComfyError::Http(e.to_string()))?;
        Ok(Self {
            base: base_url.into().trim_end_matches('/').to_string(),
            http,
            client_id: uuid::Uuid::new_v4().to_string(),
        })
    }

    /// Pelny przebieg text2img: rozwiaz checkpoint, zbuduj workflow, zakolejkuj,
    /// odczekaj na wynik, sciagnij bajty kazdego obrazu (PNG). Zwraca wektor
    /// bajtow — po jednym wpisie na obraz (zgodnie z `batch_size`).
    pub async fn text2img(&self, params: &Text2ImgParams) -> Result<Vec<Vec<u8>>, ComfyError> {
        let checkpoint = self.resolve_checkpoint(params.checkpoint.as_deref()).await;
        debug!(checkpoint = %checkpoint, "ComfyUI text2img: wybrany checkpoint");

        let workflow = build_sd15_workflow(params, &checkpoint);
        let prompt_id = self.queue_prompt(workflow).await?;

        // Budzet czasu: generacja SD1.5 batcha bywa wolna na CPU/slabym GPU,
        // wiec dajemy kilka minut, odpytujac historie co sekunde.
        let deadline = std::time::Instant::now() + Duration::from_secs(300);
        let interval = Duration::from_millis(1000);

        loop {
            if std::time::Instant::now() >= deadline {
                return Err(ComfyError::Timeout);
            }

            if let Some(images) = self.poll_history(&prompt_id).await? {
                if images.is_empty() {
                    return Err(ComfyError::Backend(
                        "workflow zakonczony bez obrazow na wyjsciu".to_string(),
                    ));
                }
                let mut out = Vec::with_capacity(images.len());
                for img in &images {
                    out.push(self.fetch_image(img).await?);
                }
                return Ok(out);
            }

            tokio::time::sleep(interval).await;
        }
    }

    /// Wykrywa nazwe checkpointu do uzycia. Priorytet: jawny override z requestu/
    /// configu, potem pierwszy checkpoint zwrocony przez `/object_info`
    /// (faktycznie zaladowany przez serwis), a na koncu staly `DEFAULT_CHECKPOINT`.
    async fn resolve_checkpoint(&self, override_name: Option<&str>) -> String {
        if let Some(name) = override_name {
            if !name.is_empty() {
                return name.to_string();
            }
        }
        match self.list_checkpoints().await {
            Ok(names) => match names.into_iter().next() {
                Some(first) => first,
                None => {
                    warn!("ComfyUI nie zglosil zadnego checkpointu — uzywam domyslnego");
                    DEFAULT_CHECKPOINT.to_string()
                }
            },
            Err(e) => {
                warn!("ComfyUI /object_info nieosiagalne ({e}) — uzywam domyslnego checkpointu");
                DEFAULT_CHECKPOINT.to_string()
            }
        }
    }

    /// Czyta liste zaladowanych checkpointow z `/object_info/CheckpointLoaderSimple`.
    /// ComfyUI zwraca dostepne nazwy w `input.required.ckpt_name[0]` (lista).
    async fn list_checkpoints(&self) -> Result<Vec<String>, ComfyError> {
        let url = format!("{}/object_info/CheckpointLoaderSimple", self.base);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| ComfyError::Http(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(ComfyError::Backend(format!(
                "/object_info status {}",
                resp.status()
            )));
        }
        let body: Value = resp
            .json()
            .await
            .map_err(|e| ComfyError::Http(e.to_string()))?;

        // Sciezka: CheckpointLoaderSimple.input.required.ckpt_name -> [ [name, ...], {..} ]
        let names = body
            .get("CheckpointLoaderSimple")
            .and_then(|n| n.get("input"))
            .and_then(|n| n.get("required"))
            .and_then(|n| n.get("ckpt_name"))
            .and_then(|n| n.as_array())
            .and_then(|arr| arr.first())
            .and_then(|first| first.as_array())
            .map(|list| {
                list.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Ok(names)
    }

    /// Kolejkuje workflow przez `POST /prompt`. Zwraca `prompt_id` do pollingu.
    async fn queue_prompt(&self, workflow: Value) -> Result<String, ComfyError> {
        let url = format!("{}/prompt", self.base);
        let payload = json!({ "prompt": workflow, "client_id": self.client_id });
        let resp = self
            .http
            .post(&url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| ComfyError::Http(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            // ComfyUI zwraca 400 + JSON z `error`/`node_errors` gdy graf jest zly.
            let body = resp.text().await.unwrap_or_default();
            return Err(ComfyError::Backend(format!(
                "/prompt status {status}: {body}"
            )));
        }
        let parsed: PromptResponse = resp
            .json()
            .await
            .map_err(|e| ComfyError::Backend(format!("niepoprawna odpowiedz /prompt: {e}")))?;
        Ok(parsed.prompt_id)
    }

    /// Odpytuje `GET /history/<id>`. Zwraca `Some(images)` gdy zadanie sie
    /// zakonczylo (wpis pojawil sie w historii), albo `None` gdy jeszcze trwa.
    /// Blad wykonania z ComfyUI mapowany jest na `ComfyError::Backend`.
    async fn poll_history(&self, prompt_id: &str) -> Result<Option<Vec<ImageRef>>, ComfyError> {
        let url = format!("{}/history/{}", self.base, prompt_id);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| ComfyError::Http(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(ComfyError::Backend(format!(
                "/history status {}",
                resp.status()
            )));
        }
        let body: Value = resp
            .json()
            .await
            .map_err(|e| ComfyError::Http(e.to_string()))?;

        // Pusta mapa = zadanie jeszcze w kolejce/wykonaniu.
        let entry = match body.get(prompt_id) {
            Some(e) => e,
            None => return Ok(None),
        };

        // ComfyUI sygnalizuje blad wykonania w `status.status_str == "error"`
        // (lub niepustym `status.messages` z typem "execution_error").
        if let Some(status_str) = entry
            .get("status")
            .and_then(|s| s.get("status_str"))
            .and_then(|s| s.as_str())
        {
            if status_str == "error" {
                return Err(ComfyError::Backend(format!(
                    "wykonanie workflow nie powiodlo sie: {}",
                    entry.get("status").cloned().unwrap_or(Value::Null)
                )));
            }
        }

        // Zbierz obrazy ze wszystkich wezlow wyjsciowych (SaveImage emituje
        // `outputs.<node>.images = [{filename, subfolder, type}]`).
        let mut images = Vec::new();
        if let Some(outputs) = entry.get("outputs").and_then(|o| o.as_object()) {
            for node in outputs.values() {
                if let Some(arr) = node.get("images").and_then(|i| i.as_array()) {
                    for img in arr {
                        let filename = img.get("filename").and_then(|v| v.as_str());
                        if let Some(filename) = filename {
                            images.push(ImageRef {
                                filename: filename.to_string(),
                                subfolder: img
                                    .get("subfolder")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                                image_type: img
                                    .get("type")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("output")
                                    .to_string(),
                            });
                        }
                    }
                }
            }
        }
        Ok(Some(images))
    }

    /// Sciaga bajty obrazu przez `GET /view?filename=..&subfolder=..&type=..`.
    async fn fetch_image(&self, img: &ImageRef) -> Result<Vec<u8>, ComfyError> {
        let url = format!(
            "{}/view?filename={}&subfolder={}&type={}",
            self.base,
            urlencoding::encode(&img.filename),
            urlencoding::encode(&img.subfolder),
            urlencoding::encode(&img.image_type),
        );
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| ComfyError::Http(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(ComfyError::Backend(format!(
                "/view status {} dla {}",
                resp.status(),
                img.filename
            )));
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| ComfyError::Http(e.to_string()))?;
        Ok(bytes.to_vec())
    }
}

/// Referencja do jednego obrazu w wynikach ComfyUI (z `outputs.*.images`).
struct ImageRef {
    filename: String,
    subfolder: String,
    image_type: String,
}

/// Buduje graf workflow ComfyUI dla text2img SD1.5. Klucze wezlow to stringi
/// (ComfyUI wymaga string-keys), a `inputs` linkuja wezly przez `[ "node_id",
/// output_index ]`. Lancuch: Checkpoint -> CLIP(+/-) -> EmptyLatent -> KSampler
/// -> VAEDecode -> SaveImage.
fn build_sd15_workflow(params: &Text2ImgParams, checkpoint: &str) -> Value {
    json!({
        "4": {
            "class_type": "CheckpointLoaderSimple",
            "inputs": { "ckpt_name": checkpoint }
        },
        "6": {
            "class_type": "CLIPTextEncode",
            "inputs": { "text": params.prompt, "clip": ["4", 1] }
        },
        "7": {
            "class_type": "CLIPTextEncode",
            "inputs": { "text": params.negative_prompt, "clip": ["4", 1] }
        },
        "5": {
            "class_type": "EmptyLatentImage",
            "inputs": {
                "width": params.width,
                "height": params.height,
                "batch_size": params.batch_size
            }
        },
        "3": {
            "class_type": "KSampler",
            "inputs": {
                "seed": params.seed,
                "steps": params.steps,
                "cfg": params.cfg,
                "sampler_name": params.sampler,
                "scheduler": params.scheduler,
                "denoise": 1.0,
                "model": ["4", 0],
                "positive": ["6", 0],
                "negative": ["7", 0],
                "latent_image": ["5", 0]
            }
        },
        "8": {
            "class_type": "VAEDecode",
            "inputs": { "samples": ["3", 0], "vae": ["4", 2] }
        },
        "9": {
            "class_type": "SaveImage",
            "inputs": { "filename_prefix": "tentaflow", "images": ["8", 0] }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_params() -> Text2ImgParams {
        Text2ImgParams {
            prompt: "a cat".to_string(),
            negative_prompt: String::new(),
            width: 512,
            height: 768,
            batch_size: 2,
            steps: 20,
            cfg: 7.0,
            sampler: "euler".to_string(),
            scheduler: "normal".to_string(),
            seed: 42,
            checkpoint: Some("model.safetensors".to_string()),
        }
    }

    #[test]
    fn workflow_links_nodes_and_carries_params() {
        let wf = build_sd15_workflow(&sample_params(), "model.safetensors");

        // Checkpoint przekazany do loadera.
        assert_eq!(wf["4"]["inputs"]["ckpt_name"], "model.safetensors");
        // Pozytywny prompt w wezle 6, negatywny w 7.
        assert_eq!(wf["6"]["inputs"]["text"], "a cat");
        assert_eq!(wf["7"]["inputs"]["text"], "");
        // Wymiary i batch w EmptyLatentImage.
        assert_eq!(wf["5"]["inputs"]["width"], 512);
        assert_eq!(wf["5"]["inputs"]["height"], 768);
        assert_eq!(wf["5"]["inputs"]["batch_size"], 2);
        // KSampler linkuje model/positive/negative/latent i niesie sampler params.
        assert_eq!(wf["3"]["inputs"]["seed"], 42);
        assert_eq!(wf["3"]["inputs"]["steps"], 20);
        assert_eq!(wf["3"]["inputs"]["sampler_name"], "euler");
        assert_eq!(wf["3"]["inputs"]["scheduler"], "normal");
        assert_eq!(wf["3"]["inputs"]["model"], json!(["4", 0]));
        assert_eq!(wf["3"]["inputs"]["positive"], json!(["6", 0]));
        assert_eq!(wf["3"]["inputs"]["negative"], json!(["7", 0]));
        assert_eq!(wf["3"]["inputs"]["latent_image"], json!(["5", 0]));
        // VAEDecode bierze sample z KSamplera i vae z checkpointu.
        assert_eq!(wf["8"]["inputs"]["samples"], json!(["3", 0]));
        assert_eq!(wf["8"]["inputs"]["vae"], json!(["4", 2]));
        // SaveImage zbiera dekodowane obrazy.
        assert_eq!(wf["9"]["inputs"]["images"], json!(["8", 0]));
    }
}

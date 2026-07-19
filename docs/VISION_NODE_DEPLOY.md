# Vision recognition node — deployment runbook

Audience: an autonomous AI operator setting up a fresh TentaFlow node that must run the
ADR / dangerous-goods placard + license-plate + sticker-state recognition pipeline on its
local GPU, pulling the pre-trained model weights over HTTPS from another TentaFlow instance.

This runbook is self-contained. Execute the steps in order. Everything is driven by the
repo's own scripts; there are no hidden manual steps. Where a value must be supplied it is
written as `<PLACEHOLDER>` — fill it in from the "Inputs" section.

---

## 0. Inputs (fill these in before starting)

| Name | Meaning | Example |
|------|---------|---------|
| `SOURCE_URL` | Base URL of the TentaFlow instance that hosts the trained models. Must be reachable from this node (LAN, mesh, or a public/forwarded HTTPS endpoint). | `https://192.168.11.143:8090` or `https://models.example.net` |
| `MODEL_API_KEY` | Bearer API key on `SOURCE_URL` scoped `('model_bundle','vision-all')`. Provided out-of-band. Rotatable in the source GUI (Access & API keys). | `sk-…` |
| `REPO_DIR` | Where this repo lives on the node. | `/opt/TentaFlow` |

The source instance serves the bundle over its Tier-2 HTTPS endpoints
`GET /models/manifest/<bundle>` and `GET /models/file/<bundle>/<name>`, authenticated by the
Bearer key (default-DENY, per-bundle scope). TLS may be self-signed → use `curl -k` / the
built-in pull path already tolerates the intra-fleet trust posture.

---

## 1. Pull the new version

```bash
cd "$REPO_DIR"
git fetch origin
git checkout main
git pull --ff-only origin main
git log --oneline -1        # confirm you are on the intended HEAD
```

No workspace `Cargo.toml` — each crate builds independently. The main binary is `tentaflow`.

---

## 2. Provision native GPU libraries (platform-agnostic)

One script handles both `x86_64` and `aarch64` (Grace-Blackwell / Blackwell) and auto-detects
the GPU + compute capability. It vendors the correct ONNX Runtime GPU execution providers
(TensorRT → CUDA → CPU fallback) plus the runtime libs the providers need, into
`native-libs/<platform>/lib-dynamic/`.

```bash
cd "$REPO_DIR"
./scripts/native-libs/build-all.sh
```

- The script detects `uname -m` and the NVIDIA GPU (`nvidia-smi` compute cap) and picks the
  matching ONNX Runtime GPU build + CUDA architecture automatically. Env overrides exist but
  the defaults are meant to "just work" per detected hardware.
- On a **Blackwell datacenter GPU (B300 / Grace-Blackwell)** the script targets the node's
  compute capability. If the machine only has the CUDA toolkit but not the GPU execution
  provider shared objects, the script vendors them; if a prebuilt provider does not cover this
  GPU's SM, the script builds ONNX Runtime from source against the installed CUDA. This can
  take a while on first run — let it finish.
- Result check — the vision GPU providers must be present:
  ```bash
  ls native-libs/$(uname -m | sed 's/x86_64/linux-x86_64/;s/aarch64/linux-aarch64/')/lib-dynamic/ \
    | grep -E 'libonnxruntime(\.so|_providers_(cuda|tensorrt))'
  ```
  You should see `libonnxruntime.so.*`, `libonnxruntime_providers_cuda.so`, and (if available
  for this GPU) `libonnxruntime_providers_tensorrt.so`.

Host requirement: an NVIDIA driver compatible with the installed CUDA. The script vendors the
CUDA/cuDNN/TensorRT runtime libs it needs next to the binary, so a full system CUDA install is
not strictly required at runtime — only the driver.

---

## 3. Build the binary

```bash
cd "$REPO_DIR/tentaflow"
cargo build --release        # production build (fat LTO — slow link, run once)
# For fast iteration you may use: cargo build --profile release-fast
```

`tentaflow/build.rs` copies `native-libs/<platform>/lib-dynamic/*` next to the binary
(`$ORIGIN` rpath), so the vendored ONNX Runtime + GPU providers are found at runtime. The
vision detector/classifier/OCR run in-process via ONNX Runtime with the TensorRT/CUDA
execution provider when the providers are present (this is what makes recognition run on the
GPU instead of the CPU).

---

## 4. First run + data dir

Run against the node's own data dir. **Do NOT set `TENTAFLOW_HOME`** (the Ed25519 node key
derivation breaks if you do). The default `.runtime/` under the repo is used.

```bash
cd "$REPO_DIR"
./tentaflow/target/release/tentaflow --config config.toml
# (background it however your environment prefers; it listens on HTTPS/QUIC :8090 by default)
```

Startup notes:
- If a previous process was hard-killed and the Sync Ledger refuses to open, do NOT delete
  `.runtime/sync/ledger/lock` (that breaks it with a NotFound). Kill any stale process
  cleanly by exact name (`pkill -x tentaflow`) and restart. Only if the ledger is genuinely
  corrupt: `mv .runtime/sync/ledger .runtime/sync/ledger.broken && mkdir .runtime/sync/ledger`
  then restart — a fresh ledger re-syncs from the mesh.

Confirm the GPU vision path is active — the detector load line must name the GPU provider,
not CPU-only:
```bash
grep -E '\[rfdetr\] loaded' <your-run-log>   # expect "backend ort TensorRT→CUDA→CPU"
nvidia-smi                                    # GPU util should rise once inference runs
```

---

## 5. Pull the trained models over the API

The trained weights (RF-DETR ADR detector, sticker-state classifier, plate OCR) are NOT in
git — pull them from `SOURCE_URL` into this node's `vision_models_dir` (`.runtime/models/vision/`).

Two equivalent ways:

### 5a. Built-in pull at deploy time (preferred, GUI-set)
In the source-instance model sharing you can point this node's deploy at the manifest URL.
On THIS node, set the two settings (Settings → Dostępy zewnętrzne, stored encrypted), then
deploy the vision services (§6) — the embedded-vision deploy fetches + sha256-verifies each
file from the manifest automatically:
- `vision_bundle_base_url` = `<SOURCE_URL>/models/manifest/vision-all`
- `vision_bundle_api_key`  = `<MODEL_API_KEY>`

### 5b. Direct pull (scriptable, no GUI)
```bash
DEST="$REPO_DIR/.runtime/models/vision"; mkdir -p "$DEST"
curl -sk -H "Authorization: Bearer <MODEL_API_KEY>" \
  "<SOURCE_URL>/models/manifest/vision-all" -o /tmp/vman.json
python3 - "$DEST" <<'PY'
import json,sys,subprocess,hashlib,os
dest=sys.argv[1]; d=json.load(open('/tmp/vman.json')); base="<SOURCE_URL>"
key="<MODEL_API_KEY>"
for f in d['files']:
    out=os.path.join(dest,f['name'])
    subprocess.run(["curl","-sk","-H",f"Authorization: Bearer {key}",base+f['url'],"-o",out],check=True)
    got=hashlib.sha256(open(out,'rb').read()).hexdigest()
    assert got==f['sha256'], f"sha256 mismatch {f['name']}"
    print("ok",f['name'],f['size'])
PY
```
All files land sha256-verified. The bundle includes both the `.onnx` (GPU/ort path) and the
compiled `.bpk` weights plus the class/config sidecars (`rfdetr-classes.json`,
`stan-classes.json`, `plate-ocr-config.json`, `adr-list.json`).

Sidecar note for the ORT classifier: `model_stan.onnx` references its external weights by a
fixed internal name; the loader self-heals this, but if you see an external-data NotFound,
ensure a sibling exists: `ln -f "$DEST/model_stan.onnx.data" "$DEST/model.onnx.data"`.

---

## 6. Deploy the vision services on this node

Three embedded vision engines must be registered + running: `rfdetr-adr` (detector),
`nalepka-stan` (sticker-state classifier), `plate-ocr` (plate OCR). They load their weights
lazily from `vision_models_dir` on first inference.

Preferred: deploy them through the GUI (Katalog serwisów / Services) — the embedded-vision
deploy provisions the bundle (§5a) and registers the model in the catalog.

If the embedded engines are not offered as catalog-deployable on this platform, they can be
registered directly (they are boot-pinned engines). Insert the service + model-registry rows
matching a working node and restart — the supervisor auto-deploys pinned embedded services on
boot and finds the weights already on disk. The three engines and their model names:

| engine_id | model_name | category |
|-----------|-----------|----------|
| `rfdetr-adr` | `rfdetr-adr-base` | vision |
| `nalepka-stan` | `nalepka-stan-mnv4` | vision |
| `plate-ocr` | `plate-ocr-fast` | vision |

After deploy, confirm `status=running` for all three and that the model registry lists their
model names.

---

## 7. Wire the camera analysis pipeline

Recognition runs as a per-camera configurable CV pipeline (stages: detect → classify state →
OCR plate/ADR). A default pipeline reproducing the ADR behaviour is seeded automatically
(`camera_cv_pipelines`, the `is_default` row). Each camera resolves to the default pipeline
unless assigned a specific one.

- The pipeline stages reference models by **alias**, not by hardcoded model id. The relevant
  aliases (already seeded): `tentavision-detect → rfdetr-adr-base`,
  `tentavision-stan → nalepka-stan-mnv4`, `tentavision-ocr → plate-ocr-fast`.
- Edit / assign per camera from the TentaVision addon panel: camera row → "Pipeline analizy".
- To distribute inference across several GPU nodes in the mesh, set an alias's **Strategy** to
  `round_robin` (Services → alias editor). With the same model deployed on N mesh nodes the
  resolver produces N candidates and rotates per request — this is the generic mesh mechanism,
  identical for every model type (LLM, embeddings, vision), not vision-specific.

Add cameras (RTSP/ONVIF/…) from the TentaVision addon; the always-on analysis loop feeds the
default pipeline and publishes detections to the dashboard live view (bounding boxes +
recognized state/plate). The 17 placard/plate classes are defined in the shipped
`rfdetr-classes.json`; the ADR Kemler/UN mapping is in `adr-list.json`.

---

## 8. Verify recognition end-to-end

1. Both/all vision services `running`, models registered.
2. Detector log line shows `backend ort TensorRT→CUDA→CPU` and a `pool=N session(s)` line.
3. `nvidia-smi` shows GPU utilization while a camera is analyzed (NOT idle → confirms GPU, not
   CPU fallback).
4. In the dashboard TentaVision → Live view, an analyzed camera shows detection boxes with the
   recognized placard class, sticker state, and plate/ADR text.
5. VRAM is stable over time (the ort session pool uses dedicated per-session threads + a capped
   TensorRT workspace; `TENTAFLOW_TRT_WORKSPACE_MB` default 1024). If you scale sessions, budget
   VRAM: `TENTAFLOW_VISION_DETECTOR_SESSIONS`, `TENTAFLOW_VISION_INFLIGHT`,
   `TENTAFLOW_STAN_SESSIONS`, `TENTAFLOW_PLATE_SESSIONS`. On a large-VRAM Blackwell card you can
   raise the detector sessions well above the ~2–3 a 24 GB card allows.

---

## 9. Tuning knobs (env)

| Env | Default | Effect |
|-----|---------|--------|
| `TENTAFLOW_TRT_WORKSPACE_MB` | 1024 | TensorRT scratch cap per session (unbounded default over-allocates VRAM). |
| `TENTAFLOW_VISION_DETECTOR_SESSIONS` | 1 | Concurrent RF-DETR ort sessions (each a full model copy on GPU). |
| `TENTAFLOW_VISION_INFLIGHT` | =sessions | Max concurrent forwards issued by the engine loop. |
| `TENTAFLOW_STAN_SESSIONS` / `TENTAFLOW_PLATE_SESSIONS` | 1 | Classifier / OCR session pool sizes. |
| `TENTAFLOW_VISION_GPUS` | auto (device 0) | Comma list or count of CUDA devices to spread sessions across (multi-GPU). |

---

## Recovery cheatsheet

- Stale process holding the port/ledger: `pkill -x tentaflow` (never `pkill -f`, it matches
  your own shell), wait, restart. Never `rm` the ledger lock file.
- Model file missing / sha mismatch: re-pull §5, the manifest carries the authoritative sha256.
- Detector loads on CPU instead of GPU: the GPU provider `.so` files are missing from
  `native-libs/<platform>/lib-dynamic/` — re-run `./scripts/native-libs/build-all.sh` and rebuild.

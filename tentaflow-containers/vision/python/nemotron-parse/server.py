# =============================================================================
# Plik: server.py
# Opis: Serwer FastAPI dla Nemotron-Parse (NVIDIA-Nemotron-Parse-v1.2). Model to
#       transformers VLM (trust_remote_code) z wlasnym task-promptem i logits
#       processorami; uzywamy oficjalnej sciezki z example_with_processor.py.
#       POST /parse zwraca markdown + bloki layoutu (klasa, bbox, tekst).
# Przyklad: curl -F image=@faktura.png http://127.0.0.1:8094/parse
# =============================================================================

import base64
import io
import os
import threading

import torch
from fastapi import FastAPI, File, HTTPException, UploadFile
from PIL import Image
from pydantic import BaseModel
from transformers import AutoModel, AutoProcessor, AutoTokenizer, GenerationConfig

# Skrypty pomocnicze z repo HF (skopiowane do /app przez Dockerfile).
from postprocessing import extract_classes_bboxes, transform_bbox_to_original, postprocess_text
from hf_logits_processor import TableInsertionLogitsProcessor, RepetitionStopProcessor

MODEL_ID = os.environ.get("MODEL", "nvidia/NVIDIA-Nemotron-Parse-v1.2")
# Prompt zadania: predykcja bbox + klas + markdown (jak w example_with_processor.py).
TASK_PROMPT = "</s><s><predict_bbox><predict_classes><output_markdown><predict_no_text_in_pic>"

app = FastAPI(title="Nemotron-Parse")

# Stan ladowany leniwie przy pierwszym zadaniu, zeby /health odpowiadal wczesniej.
_state: dict = {"model": None, "tokenizer": None, "processor": None, "gen": None}


class ParseBase64Request(BaseModel):
    image_base64: str


def _require_cuda() -> None:
    if not torch.cuda.is_available():
        raise HTTPException(
            status_code=503,
            detail="Nemotron-Parse wymaga CUDA — brak dostepnego GPU.",
        )


_LOAD_LOCK = threading.Lock()


def _ensure_model() -> None:
    if _state["model"] is not None:
        return
    with _LOAD_LOCK:
        if _state["model"] is not None:
            return
        _require_cuda()
        model = AutoModel.from_pretrained(
            MODEL_ID, trust_remote_code=True, torch_dtype=torch.bfloat16
        ).to("cuda").eval()
        _state["tokenizer"] = AutoTokenizer.from_pretrained(MODEL_ID)
        _state["processor"] = AutoProcessor.from_pretrained(MODEL_ID, trust_remote_code=True)
        _state["gen"] = GenerationConfig.from_pretrained(MODEL_ID, trust_remote_code=True)
        _state["model"] = model


# Model ladowany w WATKU TLA przy starcie — synchroniczne ladowanie w handlerze
# blokowaloby event-loop uvicorna, przez co /health przestawal odpowiadac i
# supervisor Core ubijal+restartowal proces w petli (zwlaszcza dla wiekszych
# modeli). W tle /health odpowiada od razu, a /parse zwraca 503 do czasu gotowosci.
@app.on_event("startup")
def _start_background_load() -> None:
    threading.Thread(target=_ensure_model, name="model-load", daemon=True).start()


def _decode_image(raw: bytes) -> Image.Image:
    try:
        return Image.open(io.BytesIO(raw)).convert("RGB")
    except Exception as exc:  # noqa: BLE001 — zwracamy czytelny blad klientowi
        raise HTTPException(status_code=400, detail=f"Nieprawidlowy obraz: {exc}")


def _run_parse(image: Image.Image) -> dict:
    if _state["model"] is None:
        raise HTTPException(status_code=503, detail="Model jeszcze sie laduje, sprobuj za chwile.")
    model = _state["model"]
    tokenizer = _state["tokenizer"]
    processor = _state["processor"]
    inputs = processor(
        images=[image], text=TASK_PROMPT, return_tensors="pt", add_special_tokens=False
    ).to(model.device)

    table_processor = TableInsertionLogitsProcessor(
        tokenizer=tokenizer, table_prefix="\\begin{tabular}"
    )
    repetition_processor = RepetitionStopProcessor(
        tokenizer=tokenizer, max_repetitions=10, ngram_sizes=[3, 4, 5, 6], window_size=500
    )
    with torch.inference_mode():
        outputs = model.generate(
            **inputs,
            generation_config=_state["gen"],
            logits_processor=[table_processor, repetition_processor],
        )
    table_processor.reset()
    repetition_processor.reset()

    generated_text = processor.batch_decode(outputs, skip_special_tokens=True)[0]
    classes, bboxes, texts = extract_classes_bboxes(generated_text)
    bboxes = [transform_bbox_to_original(b, image.width, image.height) for b in bboxes]
    texts = [
        postprocess_text(t, cls=c, table_format="HTML", text_format="markdown", blank_text_in_figures=False)
        for t, c in zip(texts, classes)
    ]
    blocks = [
        {"class": c, "bbox": b, "text": t}
        for c, b, t in zip(classes, bboxes, texts)
    ]
    markdown = "\n\n".join(t for t in texts if t)
    return {"markdown": markdown, "blocks": blocks}


@app.get("/health")
def health() -> dict:
    return {"status": "ok", "model": MODEL_ID, "loaded": _state["model"] is not None}


@app.post("/parse")
async def parse(image: UploadFile = File(...)) -> dict:
    raw = await image.read()
    return _run_parse(_decode_image(raw))


@app.post("/parse/base64")
def parse_base64(req: ParseBase64Request) -> dict:
    try:
        raw = base64.b64decode(req.image_base64)
    except Exception as exc:  # noqa: BLE001
        raise HTTPException(status_code=400, detail=f"Bledny base64: {exc}")
    return _run_parse(_decode_image(raw))

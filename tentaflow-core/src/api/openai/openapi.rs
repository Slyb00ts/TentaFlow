// =============================================================================
// Plik: api/openai/openapi.rs
// Opis: Publiczna, interaktywna dokumentacja REST API (OpenAPI 3.1 + Scalar UI).
//       Buduje pelna specyfikacje jako serde_json::Value oraz serwuje
//       samowystarczalna strone Scalar (zbundlowany JS) pod /docs.
// Przyklad: GET /openapi.json -> spec, GET /docs -> Scalar API reference.
// =============================================================================

use serde_json::{json, Value};

/// Wersja API raportowana w `info.version` — bierzemy z Cargo, zeby nie
/// rozjezdzala sie z wydaniem binarki.
const API_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Zbundlowany standalone Scalar — wstrzykniety w binarke, zeby /docs dzialalo
/// offline bez zewnetrznego CDN (pelna samowystarczalnosc).
const SCALAR_JS: &str = include_str!("../../../www/js/vendor/scalar.standalone.js");

/// Zwraca tresc zbundlowanego Scalar JS (serwowane pod /docs/scalar.js).
pub fn scalar_js() -> &'static str {
    SCALAR_JS
}

/// Strona HTML osadzajaca Scalar. Auto-render: Scalar wyszukuje
/// `#api-reference[data-url]` i pobiera spec z /openapi.json. Relatywne URL-e
/// w spec (server "/") sprawiaja, ze "Try it out" trafia w ten sam host.
pub fn docs_html() -> String {
    r#"<!DOCTYPE html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>TentaFlow REST API</title>
    <style>
      body { margin: 0; }
    </style>
  </head>
  <body>
    <script id="api-reference" data-url="/openapi.json"></script>
    <script src="/docs/scalar.js"></script>
  </body>
</html>"#
        .to_string()
}

/// Buduje pelna specyfikacje OpenAPI 3.1 dla zewnetrznego REST API TentaFlow.
///
/// Spec jest publiczna (serwowana bez auth), ale udokumentowane endpointy /v1/*
/// nadal wymagaja `Authorization: Bearer <key>` — globalny `bearerAuth` jest
/// nadpisywany pustym `security` tylko na publicznych/HMAC trasach.
pub fn build_spec() -> Value {
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "TentaFlow REST API",
            "version": API_VERSION,
            "description": "Zewnetrzne, OpenAI-kompatybilne REST API TentaFlow dla aplikacji \
                korzystajacych z modeli hostowanych na wezle. Uwierzytelnianie kluczem API \
                (`Authorization: Bearer <key>`). Endpointy Service-to-Core oraz pobierania \
                plikow uzywaja HMAC / podpisanych URL-i, nie klucza API. To NIE jest interfejs \
                dashboardu — dashboard komunikuje sie binarnym protokolem (WebTransport/WebSocket)."
        },
        "servers": [
            { "url": "/", "description": "Ten wezel (relatywny URL — Try-it-out trafia w ten sam host)" }
        ],
        "security": [ { "bearerAuth": [] } ],
        "tags": [
            { "name": "Models", "description": "Lista modeli dostepnych dla klucza API." },
            { "name": "Chat", "description": "Chat completions (tekst i vision), blocking lub SSE streaming." },
            { "name": "Embeddings", "description": "Generowanie wektorow embeddingow." },
            { "name": "Reranking", "description": "Rerank dokumentow wzgledem zapytania (kontrakty Cohere/Jina oraz NVIDIA NeMo)." },
            { "name": "Vision", "description": "Wizyjny inference NVIDIA NIM (OCR, detekcja, poza, emocje)." },
            { "name": "Images", "description": "Generowanie obrazow z promptu tekstowego." },
            { "name": "Audio", "description": "Transkrypcja (STT) oraz synteza mowy (TTS), w tym streaming." },
            { "name": "Health", "description": "Liveness/readiness — publiczne, bez auth." },
            { "name": "Service-to-Core / signed downloads", "description": "Endpointy HMAC / podpisanych URL-i (nie uzywaja klucza API). Frame pickup w produkcji wymaga mTLS." }
        ],
        "paths": build_paths(),
        "components": build_components()
    })
}

/// Definicje wszystkich tras (paths) z opisami, schematami i przykladami.
fn build_paths() -> Value {
    json!({
        "/v1/models": {
            "get": {
                "tags": ["Models"],
                "summary": "Lista modeli",
                "description": "Zwraca modele widoczne dla uwierzytelnionego klucza API \
                    (filtrowane per-principal, fail-closed). Format zgodny z OpenAI `GET /v1/models`.",
                "operationId": "listModels",
                "responses": {
                    "200": {
                        "description": "Lista modeli",
                        "content": { "application/json": {
                            "schema": { "$ref": "#/components/schemas/ModelList" },
                            "example": {
                                "object": "list",
                                "data": [
                                    { "id": "qwen3-27b", "object": "model", "owned_by": "tentaflow" },
                                    { "id": "whisper-large-v3", "object": "model", "owned_by": "tentaflow" }
                                ]
                            }
                        } }
                    },
                    "401": { "$ref": "#/components/responses/Unauthorized" }
                }
            }
        },
        "/v1/chat/completions": {
            "post": {
                "tags": ["Chat"],
                "summary": "Chat completions (tekst i vision)",
                "description": "OpenAI-kompatybilny chat. `messages[].content` moze byc stringiem \
                    albo tablica czesci: `{type:\"text\",text}` oraz `{type:\"image_url\",image_url:{url}}` \
                    (vision — `url` to data-URI base64 lub publiczny URL). Gdy `stream:true`, odpowiedz \
                    jest strumieniem Server-Sent Events (`text/event-stream`) z chunkami \
                    `chat.completion.chunk` zakonczonymi `data: [DONE]`.",
                "operationId": "createChatCompletion",
                "requestBody": {
                    "required": true,
                    "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/ChatCompletionRequest" },
                        "examples": {
                            "text": {
                                "summary": "Prosty czat tekstowy",
                                "value": {
                                    "model": "qwen3-27b",
                                    "messages": [
                                        { "role": "system", "content": "You are a helpful assistant." },
                                        { "role": "user", "content": "Say hello in Polish." }
                                    ],
                                    "max_tokens": 128,
                                    "temperature": 0.7
                                }
                            },
                            "vision": {
                                "summary": "Vision — obraz jako data-URI",
                                "value": {
                                    "model": "qwen3-vl",
                                    "messages": [
                                        { "role": "user", "content": [
                                            { "type": "text", "text": "What is in this image?" },
                                            { "type": "image_url", "image_url": { "url": "data:image/png;base64,iVBORw0KGgo..." } }
                                        ] }
                                    ]
                                }
                            }
                        }
                    } }
                },
                "responses": {
                    "200": {
                        "description": "Chat completion (JSON) lub strumien SSE gdy `stream:true`",
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/ChatCompletionResponse" },
                                "example": {
                                    "id": "chatcmpl-abc123",
                                    "object": "chat.completion",
                                    "created": 1700000000,
                                    "model": "qwen3-27b",
                                    "choices": [ {
                                        "index": 0,
                                        "message": { "role": "assistant", "content": "Czesc!" },
                                        "finish_reason": "stop"
                                    } ],
                                    "usage": { "prompt_tokens": 21, "completion_tokens": 3, "total_tokens": 24 }
                                }
                            },
                            "text/event-stream": {
                                "schema": { "type": "string" },
                                "example": "data: {\"id\":\"chatcmpl-abc\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Czesc\"}}]}\n\ndata: [DONE]\n\n"
                            }
                        }
                    },
                    "401": { "$ref": "#/components/responses/Unauthorized" }
                }
            }
        },
        "/v1/embeddings": {
            "post": {
                "tags": ["Embeddings"],
                "summary": "Generowanie embeddingow",
                "description": "Zwraca wektory embeddingow dla pojedynczego stringa lub tablicy stringow. \
                    Format zgodny z OpenAI `POST /v1/embeddings`.",
                "operationId": "createEmbeddings",
                "requestBody": {
                    "required": true,
                    "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/EmbeddingsRequest" },
                        "example": { "model": "bge-m3", "input": ["Pierwszy tekst", "Drugi tekst"] }
                    } }
                },
                "responses": {
                    "200": {
                        "description": "Lista embeddingow",
                        "content": { "application/json": {
                            "schema": { "$ref": "#/components/schemas/EmbeddingsResponse" },
                            "example": {
                                "object": "list",
                                "model": "bge-m3",
                                "data": [
                                    { "object": "embedding", "index": 0, "embedding": [0.0123, -0.045, 0.98] },
                                    { "object": "embedding", "index": 1, "embedding": [0.21, 0.0, -0.33] }
                                ],
                                "usage": { "prompt_tokens": 6, "total_tokens": 6 }
                            }
                        } }
                    },
                    "401": { "$ref": "#/components/responses/Unauthorized" }
                }
            }
        },
        "/v1/rerank": {
            "post": {
                "tags": ["Reranking"],
                "summary": "Rerank dokumentow (Cohere/Jina)",
                "description": "Ocenia trafnosc kazdego dokumentu wzgledem zapytania i zwraca \
                    posortowane wyniki. Kontrakt zgodny z Cohere/Jina `POST /v1/rerank`. \
                    `top_n` ogranicza liczbe zwracanych wynikow, `return_documents` docza tresc dokumentu.",
                "operationId": "rerank",
                "requestBody": {
                    "required": true,
                    "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/RerankRequest" },
                        "example": {
                            "model": "nemotron-rerank",
                            "query": "Jaka jest stolica Polski?",
                            "documents": ["Warszawa to stolica Polski.", "Krakow to dawna stolica.", "Wisla to rzeka."],
                            "top_n": 2,
                            "return_documents": true
                        }
                    } }
                },
                "responses": {
                    "200": {
                        "description": "Posortowane wyniki rerankingu",
                        "content": { "application/json": {
                            "schema": { "$ref": "#/components/schemas/RerankResponse" },
                            "example": {
                                "model": "nemotron-rerank",
                                "results": [
                                    { "index": 0, "relevance_score": 0.991, "document": "Warszawa to stolica Polski." },
                                    { "index": 1, "relevance_score": 0.412, "document": "Krakow to dawna stolica." }
                                ]
                            }
                        } }
                    },
                    "401": { "$ref": "#/components/responses/Unauthorized" }
                }
            }
        },
        "/v1/ranking": {
            "post": {
                "tags": ["Reranking"],
                "summary": "Ranking pasaży (NVIDIA NeMo Retriever)",
                "description": "Kontrakt NVIDIA NeMo Retriever: `query` (string lub `{text}`) + `passages` \
                    (`[{text}]`). Zwraca surowe `logit` per pasaż. Core tlumaczy ten kontrakt na \
                    wewnetrzny `/v1/rerank` serwisow vLLM. `truncate` steruje obcinaniem dlugich wejsc.",
                "operationId": "ranking",
                "requestBody": {
                    "required": true,
                    "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/RankingRequest" },
                        "example": {
                            "model": "nemotron-rerank",
                            "query": { "text": "What is the capital of Poland?" },
                            "passages": [ { "text": "Warsaw is the capital of Poland." }, { "text": "The Vistula is a river." } ],
                            "truncate": "END"
                        }
                    } }
                },
                "responses": {
                    "200": {
                        "description": "Rankingi (surowe logit per pasaż)",
                        "content": { "application/json": {
                            "schema": { "$ref": "#/components/schemas/RankingResponse" },
                            "example": { "rankings": [ { "index": 0, "logit": 8.42 }, { "index": 1, "logit": -3.11 } ] }
                        } }
                    },
                    "401": { "$ref": "#/components/responses/Unauthorized" }
                }
            }
        },
        "/v1/infer": {
            "post": {
                "tags": ["Vision"],
                "summary": "Wizyjny inference (NVIDIA NIM)",
                "description": "Reverse-proxy do serwisow wizyjnych (OCR, detekcja obiektow, estymacja pozy, \
                    rozpoznawanie emocji). Request: `{model,input:[{type:\"image_url\",url:\"data:image/...;base64,...\"}]}`. \
                    Ksztalt odpowiedzi ZALEZY od typu modelu (oneOf ponizej). Wspolrzedne sa znormalizowane do 0..1.",
                "operationId": "visionInfer",
                "requestBody": {
                    "required": true,
                    "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/InferRequest" },
                        "example": {
                            "model": "nemotron-ocr-v1",
                            "input": [ { "type": "image_url", "url": "data:image/png;base64,iVBORw0KGgo..." } ]
                        }
                    } }
                },
                "responses": {
                    "200": {
                        "description": "Wynik wizyjny — ksztalt zalezy od typu modelu",
                        "content": { "application/json": {
                            "schema": { "$ref": "#/components/schemas/InferResponse" },
                            "examples": {
                                "ocr": {
                                    "summary": "OCR — wykryty tekst z bounding-boxami",
                                    "value": {
                                        "data": [ { "index": 0, "text_detections": [
                                            { "text_prediction": { "text": "Faktura", "confidence": 0.987 },
                                              "bounding_box": { "points": [ { "x": 0.1, "y": 0.1 }, { "x": 0.4, "y": 0.1 }, { "x": 0.4, "y": 0.15 }, { "x": 0.1, "y": 0.15 } ] } }
                                        ] } ],
                                        "usage": { "images_size_mb": 0.42 }
                                    }
                                },
                                "detection": {
                                    "summary": "Detekcja obiektow",
                                    "value": { "data": [ { "index": 0, "bounding_boxes": {
                                        "person": [ { "x_min": 0.12, "y_min": 0.20, "x_max": 0.48, "y_max": 0.91, "confidence": 0.95 } ]
                                    } } ] }
                                },
                                "pose": {
                                    "summary": "Estymacja pozy",
                                    "value": { "data": [ { "index": 0, "poses": [ {
                                        "bbox": [0.1, 0.1, 0.5, 0.9],
                                        "keypoints": [ { "name": "nose", "x": 0.3, "y": 0.2, "confidence": 0.9 } ]
                                    } ] } ] }
                                },
                                "emotion": {
                                    "summary": "Rozpoznawanie emocji",
                                    "value": { "data": [ { "index": 0, "emotion": {
                                        "label": "happy",
                                        "probabilities": { "happy": 0.82, "neutral": 0.12, "sad": 0.06 },
                                        "valence": 0.7, "arousal": 0.4
                                    } } ] }
                                }
                            }
                        } }
                    },
                    "401": { "$ref": "#/components/responses/Unauthorized" }
                }
            }
        },
        "/v1/images/generations": {
            "post": {
                "tags": ["Images"],
                "summary": "Generowanie obrazow",
                "description": "Generuje obraz(y) z promptu tekstowego. `size` w formacie `WxH` (np. `1024x1024`). \
                    Odpowiedz zawiera obrazy zakodowane base64 (`b64_json`).",
                "operationId": "createImage",
                "requestBody": {
                    "required": true,
                    "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/ImageRequest" },
                        "example": { "model": "flux", "prompt": "A red fox in a snowy forest", "size": "1024x1024", "n": 1 }
                    } }
                },
                "responses": {
                    "200": {
                        "description": "Wygenerowane obrazy (base64)",
                        "content": { "application/json": {
                            "schema": { "$ref": "#/components/schemas/ImageResponse" },
                            "example": { "created": 1700000000, "data": [ { "b64_json": "iVBORw0KGgo..." } ] }
                        } }
                    },
                    "401": { "$ref": "#/components/responses/Unauthorized" }
                }
            }
        },
        "/v1/audio/transcriptions": {
            "post": {
                "tags": ["Audio"],
                "summary": "Transkrypcja audio (STT)",
                "description": "Transkrypcja mowy do tekstu (Whisper). Body jako `multipart/form-data`: \
                    pole `file` (plik audio) + pole `model`.",
                "operationId": "createTranscription",
                "requestBody": {
                    "required": true,
                    "content": { "multipart/form-data": {
                        "schema": { "$ref": "#/components/schemas/TranscriptionRequest" }
                    } }
                },
                "responses": {
                    "200": {
                        "description": "Rozpoznany tekst",
                        "content": { "application/json": {
                            "schema": { "$ref": "#/components/schemas/TranscriptionResponse" },
                            "example": { "text": "Dzien dobry, jak moge pomoc?" }
                        } }
                    },
                    "401": { "$ref": "#/components/responses/Unauthorized" }
                }
            }
        },
        "/v1/audio/speech": {
            "post": {
                "tags": ["Audio"],
                "summary": "Synteza mowy (TTS)",
                "description": "Generuje mowe z tekstu. Odpowiedz to binarne audio (np. `audio/mpeg`, `audio/wav`) \
                    zgodnie z `response_format`.",
                "operationId": "createSpeech",
                "requestBody": {
                    "required": true,
                    "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/SpeechRequest" },
                        "example": { "model": "kokoro", "input": "Dzien dobry!", "voice": "af_heart", "response_format": "mp3" }
                    } }
                },
                "responses": {
                    "200": {
                        "description": "Binarne audio",
                        "content": { "audio/*": { "schema": { "type": "string", "format": "binary" } } }
                    },
                    "401": { "$ref": "#/components/responses/Unauthorized" }
                }
            }
        },
        "/v1/audio/speech/stream": {
            "post": {
                "tags": ["Audio"],
                "summary": "Streaming TTS (rozszerzenie TentaFlow)",
                "description": "Rozszerzenie TentaFlow (NIE OpenAI). Zwraca audio jako chunked transfer — \
                    kolejne fragmenty audio w miare syntezy. Body jak `/v1/audio/speech`.",
                "operationId": "createSpeechStream",
                "requestBody": {
                    "required": true,
                    "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/SpeechRequest" },
                        "example": { "model": "kokoro", "input": "Dlugi tekst do streamowania...", "voice": "af_heart" }
                    } }
                },
                "responses": {
                    "200": {
                        "description": "Strumien audio (chunked)",
                        "content": { "audio/*": { "schema": { "type": "string", "format": "binary" } } }
                    },
                    "401": { "$ref": "#/components/responses/Unauthorized" }
                }
            }
        },
        "/v1/audio/speech/flow-stream": {
            "post": {
                "tags": ["Audio"],
                "summary": "Flow streaming TTS (rozszerzenie TentaFlow)",
                "description": "Rozszerzenie TentaFlow (NIE OpenAI). Request leci przez flow engine w trybie \
                    streaming — audio chunki per zdanie (flow z `tts_stream_bridge`) lub jednym chunkiem \
                    (flow blocking). Body jak `/v1/audio/speech`.",
                "operationId": "createSpeechFlowStream",
                "requestBody": {
                    "required": true,
                    "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/SpeechRequest" },
                        "example": { "model": "kokoro", "input": "Tekst przez flow engine...", "voice": "af_heart" }
                    } }
                },
                "responses": {
                    "200": {
                        "description": "Strumien audio (chunked, per zdanie)",
                        "content": { "audio/*": { "schema": { "type": "string", "format": "binary" } } }
                    },
                    "401": { "$ref": "#/components/responses/Unauthorized" }
                }
            }
        },
        "/health": {
            "get": {
                "tags": ["Health"],
                "summary": "Liveness",
                "description": "Sprawdzenie zywotnosci serwera. Publiczne — bez auth.",
                "operationId": "health",
                "security": [],
                "responses": {
                    "200": { "description": "Serwer zyje", "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/HealthStatus" },
                        "example": { "status": "ok" }
                    } } }
                }
            }
        },
        "/v1/health": {
            "get": {
                "tags": ["Health"],
                "summary": "Liveness (alias /v1)",
                "description": "Alias `/health` pod prefiksem `/v1`. Publiczne — bez auth.",
                "operationId": "healthV1",
                "security": [],
                "responses": {
                    "200": { "description": "Serwer zyje", "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/HealthStatus" },
                        "example": { "status": "ok" }
                    } } }
                }
            }
        },
        "/ready": {
            "get": {
                "tags": ["Health"],
                "summary": "Readiness",
                "description": "Zwraca 200 gdy co najmniej jeden backend jest zdrowy, w przeciwnym razie 503. \
                    Publiczne — bez auth.",
                "operationId": "ready",
                "security": [],
                "responses": {
                    "200": { "description": "Gotowy", "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/HealthStatus" }
                    } } },
                    "503": { "description": "Brak zdrowych backendow" }
                }
            }
        },
        "/v1/ready": {
            "get": {
                "tags": ["Health"],
                "summary": "Readiness (alias /v1)",
                "description": "Alias `/ready` pod prefiksem `/v1`. Publiczne — bez auth.",
                "operationId": "readyV1",
                "security": [],
                "responses": {
                    "200": { "description": "Gotowy" },
                    "503": { "description": "Brak zdrowych backendow" }
                }
            }
        },
        "/core/frame/pickup": {
            "post": {
                "tags": ["Service-to-Core / signed downloads"],
                "summary": "Frame pickup (Service-to-Core, HMAC)",
                "description": "Sciezka Service-to-Core dla inference (yolo/whisper). Uwierzytelnianie NIE kluczem \
                    API, lecz jednorazowym tokenem HMAC w naglowku `X-Pickup-Token` (TTL 30 s). W produkcji \
                    WYMAGANE jest pinowanie certyfikatu klienta mTLS. Body forwardowane verbatim do serwisu.",
                "operationId": "framePickup",
                "security": [ { "pickupToken": [] } ],
                "parameters": [
                    { "name": "X-Pickup-Token", "in": "header", "required": true,
                      "description": "Jednorazowy HMAC SHA-256 token (TTL 30 s).",
                      "schema": { "type": "string" } }
                ],
                "responses": {
                    "200": { "description": "Bajty klatki / wynik inference" },
                    "401": { "description": "Niepoprawny lub wygasly token / brak mTLS" }
                }
            }
        },
        "/frames/{ref}": {
            "get": {
                "tags": ["Service-to-Core / signed downloads"],
                "summary": "Pobranie klatki (podpisany URL)",
                "description": "Pobranie pojedynczej klatki przez podpisany URL. Uwierzytelnianie tokenem HMAC \
                    w query (`?token=...`), ograniczonym czasowo (TTL). NIE uzywa klucza API. \
                    Uwaga: nie wlaczaj `RUST_LOG=hyper=debug` w produkcji bez scrubbera — token z URL-a \
                    moze wyciec do logow.",
                "operationId": "getFrame",
                "security": [],
                "parameters": [
                    { "name": "ref", "in": "path", "required": true,
                      "description": "Referencja klatki.", "schema": { "type": "string" } },
                    { "name": "token", "in": "query", "required": true,
                      "description": "Podpisany token HMAC z TTL.", "schema": { "type": "string" } }
                ],
                "responses": {
                    "200": { "description": "Bajty klatki (obraz)",
                        "content": { "image/*": { "schema": { "type": "string", "format": "binary" } } } },
                    "403": { "description": "Niepoprawny lub wygasly token" }
                }
            }
        },
        "/recordings/{ref}": {
            "get": {
                "tags": ["Service-to-Core / signed downloads"],
                "summary": "Pobranie nagrania (podpisany URL)",
                "description": "Pobranie nagrania przez podpisany URL. Uwierzytelnianie tokenem HMAC w query \
                    (`?token=...`), ograniczonym czasowo (TTL). NIE uzywa klucza API.",
                "operationId": "getRecording",
                "security": [],
                "parameters": [
                    { "name": "ref", "in": "path", "required": true,
                      "description": "Referencja nagrania.", "schema": { "type": "string" } },
                    { "name": "token", "in": "query", "required": true,
                      "description": "Podpisany token HMAC z TTL.", "schema": { "type": "string" } }
                ],
                "responses": {
                    "200": { "description": "Bajty nagrania",
                        "content": { "application/octet-stream": { "schema": { "type": "string", "format": "binary" } } } },
                    "403": { "description": "Niepoprawny lub wygasly token" }
                }
            }
        }
    })
}

/// Komponenty: security schemes, wspoldzielone odpowiedzi i schematy danych.
fn build_components() -> Value {
    json!({
        "securitySchemes": {
            "bearerAuth": {
                "type": "http",
                "scheme": "bearer",
                "description": "Klucz API w naglowku `Authorization: Bearer <key>` (alternatywnie `x-api-key`). \
                    Wymagany dla wszystkich endpointow `/v1/*`."
            },
            "pickupToken": {
                "type": "apiKey",
                "in": "header",
                "name": "X-Pickup-Token",
                "description": "Jednorazowy HMAC SHA-256 token dla `/core/frame/pickup` (TTL 30 s, w produkcji + mTLS)."
            }
        },
        "responses": {
            "Unauthorized": {
                "description": "Brak lub niepoprawny klucz API",
                "content": { "application/json": {
                    "schema": { "$ref": "#/components/schemas/Error" },
                    "example": { "error": { "type": "authentication_error", "message": "Niepoprawny API key", "code": "invalid_api_key" } }
                } }
            }
        },
        "schemas": build_schemas()
    })
}

/// Schematy danych. Wydzielone z `build_components`, bo glebokie zagniezdzenie
/// `json!` w jednym wywolaniu przekracza domyslny `recursion_limit` makra.
fn build_schemas() -> Value {
    let mut schemas = json!({
            "Error": {
                "type": "object",
                "properties": { "error": { "type": "object", "properties": {
                    "type": { "type": "string" },
                    "message": { "type": "string" },
                    "param": { "type": ["string", "null"] },
                    "code": { "type": ["string", "null"] }
                } } }
            },
            "HealthStatus": {
                "type": "object",
                "properties": { "status": { "type": "string", "example": "ok" } }
            },
            "Model": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "object": { "type": "string", "example": "model" },
                    "owned_by": { "type": "string", "example": "tentaflow" }
                },
                "required": ["id", "object", "owned_by"]
            },
            "ModelList": {
                "type": "object",
                "properties": {
                    "object": { "type": "string", "example": "list" },
                    "data": { "type": "array", "items": { "$ref": "#/components/schemas/Model" } }
                },
                "required": ["object", "data"]
            },
            "ChatMessageContentPart": {
                "oneOf": [
                    { "type": "object", "properties": {
                        "type": { "type": "string", "enum": ["text"] },
                        "text": { "type": "string" }
                    }, "required": ["type", "text"] },
                    { "type": "object", "properties": {
                        "type": { "type": "string", "enum": ["image_url"] },
                        "image_url": { "type": "object", "properties": {
                            "url": { "type": "string", "description": "data-URI base64 lub publiczny URL" }
                        }, "required": ["url"] }
                    }, "required": ["type", "image_url"] }
                ]
            },
            "ChatMessage": {
                "type": "object",
                "properties": {
                    "role": { "type": "string", "enum": ["system", "user", "assistant"] },
                    "content": {
                        "description": "String albo tablica czesci (vision).",
                        "oneOf": [
                            { "type": "string" },
                            { "type": "array", "items": { "$ref": "#/components/schemas/ChatMessageContentPart" } }
                        ]
                    }
                },
                "required": ["role", "content"]
            },
            "ChatCompletionRequest": {
                "type": "object",
                "properties": {
                    "model": { "type": "string" },
                    "messages": { "type": "array", "items": { "$ref": "#/components/schemas/ChatMessage" } },
                    "max_tokens": { "type": "integer" },
                    "temperature": { "type": "number" },
                    "top_p": { "type": "number" },
                    "stream": { "type": "boolean", "description": "Gdy true — odpowiedz jako SSE." },
                    "stop": { "oneOf": [ { "type": "string" }, { "type": "array", "items": { "type": "string" } } ] }
                },
                "required": ["model", "messages"]
            },
            "ChatChoice": {
                "type": "object",
                "properties": {
                    "index": { "type": "integer" },
                    "message": { "type": "object", "properties": {
                        "role": { "type": "string" },
                        "content": { "type": "string" }
                    } },
                    "finish_reason": { "type": ["string", "null"] }
                }
            },
            "Usage": {
                "type": "object",
                "properties": {
                    "prompt_tokens": { "type": "integer" },
                    "completion_tokens": { "type": "integer" },
                    "total_tokens": { "type": "integer" }
                }
            },
            "ChatCompletionResponse": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "object": { "type": "string", "example": "chat.completion" },
                    "created": { "type": "integer" },
                    "model": { "type": "string" },
                    "choices": { "type": "array", "items": { "$ref": "#/components/schemas/ChatChoice" } },
                    "usage": { "$ref": "#/components/schemas/Usage" }
                }
            },
            "EmbeddingsRequest": {
                "type": "object",
                "properties": {
                    "model": { "type": "string" },
                    "input": { "oneOf": [ { "type": "string" }, { "type": "array", "items": { "type": "string" } } ] }
                },
                "required": ["model", "input"]
            },
            "EmbeddingsResponse": {
                "type": "object",
                "properties": {
                    "object": { "type": "string", "example": "list" },
                    "model": { "type": "string" },
                    "data": { "type": "array", "items": { "type": "object", "properties": {
                        "object": { "type": "string", "example": "embedding" },
                        "index": { "type": "integer" },
                        "embedding": { "type": "array", "items": { "type": "number" } }
                    } } },
                    "usage": { "$ref": "#/components/schemas/Usage" }
                }
            },
            "RerankRequest": {
                "type": "object",
                "properties": {
                    "model": { "type": "string" },
                    "query": { "type": "string" },
                    "documents": { "type": "array", "items": { "type": "string" } },
                    "top_n": { "type": "integer" },
                    "return_documents": { "type": "boolean" }
                },
                "required": ["model", "query", "documents"]
            },
            "RerankResponse": {
                "type": "object",
                "properties": {
                    "model": { "type": "string" },
                    "results": { "type": "array", "items": { "type": "object", "properties": {
                        "index": { "type": "integer" },
                        "relevance_score": { "type": "number" },
                        "document": { "type": ["string", "null"] }
                    } } }
                }
            },
            "RankingRequest": {
                "type": "object",
                "properties": {
                    "model": { "type": "string" },
                    "query": {
                        "description": "String albo obiekt `{text}`.",
                        "oneOf": [ { "type": "string" }, { "type": "object", "properties": { "text": { "type": "string" } }, "required": ["text"] } ]
                    },
                    "passages": { "type": "array", "items": { "type": "object", "properties": { "text": { "type": "string" } }, "required": ["text"] } },
                    "truncate": { "type": "string", "enum": ["NONE", "END"] }
                },
                "required": ["model", "query", "passages"]
            },
            "RankingResponse": {
                "type": "object",
                "properties": {
                    "rankings": { "type": "array", "items": { "type": "object", "properties": {
                        "index": { "type": "integer" },
                        "logit": { "type": "number" }
                    } } }
                }
            },
            "InferRequest": {
                "type": "object",
                "properties": {
                    "model": { "type": "string" },
                    "input": { "type": "array", "items": { "type": "object", "properties": {
                        "type": { "type": "string", "enum": ["image_url"] },
                        "url": { "type": "string", "description": "data-URI base64, np. data:image/png;base64,..." }
                    }, "required": ["type", "url"] } }
                },
                "required": ["model", "input"]
            },
            "ImageRequest": {
                "type": "object",
                "properties": {
                    "model": { "type": "string" },
                    "prompt": { "type": "string" },
                    "size": { "type": "string", "description": "Format WxH, np. 1024x1024." },
                    "n": { "type": "integer" },
                    "response_format": { "type": "string" }
                },
                "required": ["model", "prompt"]
            },
            "ImageResponse": {
                "type": "object",
                "properties": {
                    "created": { "type": "integer" },
                    "data": { "type": "array", "items": { "type": "object", "properties": { "b64_json": { "type": "string" } } } }
                }
            },
            "TranscriptionRequest": {
                "type": "object",
                "properties": {
                    "file": { "type": "string", "format": "binary", "description": "Plik audio." },
                    "model": { "type": "string" }
                },
                "required": ["file", "model"]
            },
            "TranscriptionResponse": {
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"]
            },
            "SpeechRequest": {
                "type": "object",
                "properties": {
                    "model": { "type": "string" },
                    "input": { "type": "string" },
                    "voice": { "type": "string" },
                    "response_format": { "type": "string", "description": "np. mp3, wav, opus" }
                },
                "required": ["model", "input"]
            }
    });
    // Gleboko zagniezdzony schemat wizyjny — osobne wywolanie `json!`, zeby nie
    // przekroczyc recursion_limit makra w jednym wyrazeniu.
    if let Value::Object(map) = &mut schemas {
        map.insert("InferResponse".to_string(), build_infer_response_schema());
    }
    schemas
}

/// Schemat odpowiedzi wizyjnej `/v1/infer` (oneOf: OCR / detekcja / poza / emocje).
fn build_infer_response_schema() -> Value {
    json!({
        "description": "Ksztalt zalezy od typu modelu (OCR / detekcja / poza / emocje). Wspolrzedne 0..1.",
        "oneOf": [
            { "title": "OCR", "type": "object", "properties": {
                "data": { "type": "array", "items": { "type": "object", "properties": {
                    "index": { "type": "integer" },
                    "text_detections": { "type": "array", "items": { "type": "object", "properties": {
                        "text_prediction": { "type": "object", "properties": { "text": { "type": "string" }, "confidence": { "type": "number" } } },
                        "bounding_box": { "type": "object", "properties": { "points": { "type": "array", "items": { "type": "object", "properties": { "x": { "type": "number" }, "y": { "type": "number" } } } } } }
                    } } }
                } } },
                "usage": { "type": "object", "properties": { "images_size_mb": { "type": "number" } } }
            } },
            { "title": "Detection", "type": "object", "properties": {
                "data": { "type": "array", "items": { "type": "object", "properties": {
                    "index": { "type": "integer" },
                    "bounding_boxes": { "type": "object", "additionalProperties": { "type": "array", "items": { "type": "object", "properties": {
                        "x_min": { "type": "number" }, "y_min": { "type": "number" }, "x_max": { "type": "number" }, "y_max": { "type": "number" }, "confidence": { "type": "number" }
                    } } } }
                } } }
            } },
            { "title": "Pose", "type": "object", "properties": {
                "data": { "type": "array", "items": { "type": "object", "properties": {
                    "index": { "type": "integer" },
                    "poses": { "type": "array", "items": { "type": "object", "properties": {
                        "bbox": { "type": "array", "items": { "type": "number" } },
                        "keypoints": { "type": "array", "items": { "type": "object", "properties": { "name": { "type": "string" }, "x": { "type": "number" }, "y": { "type": "number" }, "confidence": { "type": "number" } } } }
                    } } }
                } } }
            } },
            { "title": "Emotion", "type": "object", "properties": {
                "data": { "type": "array", "items": { "type": "object", "properties": {
                    "index": { "type": "integer" },
                    "emotion": { "type": "object", "properties": {
                        "label": { "type": "string" },
                        "probabilities": { "type": "object", "additionalProperties": { "type": "number" } },
                        "valence": { "type": "number" }, "arousal": { "type": "number" }
                    } }
                } } }
            } }
        ]
    })
}

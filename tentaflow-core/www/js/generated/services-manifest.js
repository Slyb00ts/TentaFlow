// =============================================================================
// Plik: services-manifest.js
// Opis: AUTO-GENERATED przez build.rs — nie edytuj recznie.
//       Zrodlo: tentaflow-containers/*/_services/*.toml
// =============================================================================

export const SCHEMA_VERSION = 2;
export const GENERATED_AT = "2026-04-18T10:27:09Z";
export const SERVICES = [
  {
    "engine": {
      "id": "teams-bot",
      "category": "agents",
      "name": "Teams Bot",
      "description_pl": "Bot Microsoft Teams - laczy sie z meeting, transkrybuje, odpowiada przez TTS.",
      "description_en": "Microsoft Teams bot - joins meetings, transcribes, responds via TTS.",
      "homepage": "https://learn.microsoft.com/en-us/microsoftteams/platform/bots/what-are-bots",
      "license": "MIT",
      "icon": null,
      "default_port": 5000,
      "api": "custom",
      "version": "0.1.0"
    },
    "deploy": {
      "docker": {
        "context_path": "agents/docker/teams-bot",
        "platforms": [
          "linux"
        ],
        "download_image": null,
        "download_size_mb": null
      },
      "native": null,
      "external": null
    },
    "model_preset": []
  },
  {
    "engine": {
      "id": "comfyui",
      "category": "image-gen",
      "name": "ComfyUI",
      "description_pl": "Silnik generowania obrazow oparty o graf wezlow. Stable Diffusion, Flux i inne.",
      "description_en": "Node-based image generation engine. Stable Diffusion, Flux and more.",
      "homepage": "https://github.com/comfyanonymous/ComfyUI",
      "license": "GPL-3.0",
      "icon": "comfyui",
      "default_port": 5000,
      "api": "comfyui",
      "version": "0.3.10"
    },
    "deploy": {
      "docker": {
        "context_path": "image-gen/docker/comfyui",
        "platforms": [
          "linux",
          "windows"
        ],
        "download_image": null,
        "download_size_mb": null
      },
      "native": {
        "platforms": [
          "linux",
          "windows"
        ],
        "runtime": "python-bundle",
        "feature_flag": null,
        "binary_path": null,
        "bundle_path": "image-gen/python/comfyui"
      },
      "external": null
    },
    "model_preset": [
      {
        "id": "sd-1-5",
        "display_name": "Stable Diffusion 1.5",
        "repo": "runwayml/stable-diffusion-v1-5",
        "quantization": null,
        "recommended": true
      }
    ]
  },
  {
    "engine": {
      "id": "stable-diffusion-cpp",
      "category": "image-gen",
      "name": "stable-diffusion.cpp",
      "description_pl": "Lekka implementacja Stable Diffusion w C++. Dziala na CPU i GPU bez Pythona.",
      "description_en": "Lightweight Stable Diffusion in C++. Runs on CPU and GPU without Python.",
      "homepage": "https://github.com/leejet/stable-diffusion.cpp",
      "license": "MIT",
      "icon": null,
      "default_port": 5000,
      "api": "custom",
      "version": "latest"
    },
    "deploy": {
      "docker": null,
      "native": {
        "platforms": [
          "linux",
          "macos",
          "windows"
        ],
        "runtime": "binary",
        "feature_flag": null,
        "binary_path": "image-gen/native/stable-diffusion-cpp",
        "bundle_path": null
      },
      "external": null
    },
    "model_preset": [
      {
        "id": "sd-1-5-gguf",
        "display_name": "Stable Diffusion 1.5 (GGUF)",
        "repo": "leejet/stable-diffusion.cpp/sd-v1-5",
        "quantization": null,
        "recommended": true
      }
    ]
  },
  {
    "engine": {
      "id": "llama-cpp",
      "category": "llm",
      "name": "llama.cpp",
      "description_pl": "Lekki silnik LLM w C/C++. Obsluguje GGUF i embeddingi. Dziala wszedzie.",
      "description_en": "Lightweight C/C++ LLM engine. Supports GGUF and embeddings.",
      "homepage": "https://github.com/ggerganov/llama.cpp",
      "license": "MIT",
      "icon": "llama-cpp",
      "default_port": 8080,
      "api": "openai-compatible",
      "version": "b3974"
    },
    "deploy": {
      "docker": {
        "context_path": "llm/docker/llama-cpp",
        "platforms": [
          "linux",
          "windows"
        ],
        "download_image": null,
        "download_size_mb": null
      },
      "native": {
        "platforms": [
          "linux",
          "macos",
          "windows",
          "ios",
          "android"
        ],
        "runtime": "embedded",
        "feature_flag": "inference-llamacpp",
        "binary_path": null,
        "bundle_path": null
      },
      "external": null
    },
    "model_preset": [
      {
        "id": "qwen3-5-0-8b-q4",
        "display_name": "Qwen 3.5 0.8B Q4_K_M",
        "repo": "Qwen/Qwen3.5-0.8B-GGUF",
        "quantization": "Q4_K_M",
        "recommended": true
      }
    ]
  },
  {
    "engine": {
      "id": "mlx",
      "category": "llm",
      "name": "MLX",
      "description_pl": "Apple ML framework dla Apple Silicon. Najszybszy LLM na M1/M2/M3/M4.",
      "description_en": "Apple's ML framework for Apple Silicon. Fastest on M1/M2/M3/M4.",
      "homepage": "https://github.com/ml-explore/mlx",
      "license": "MIT",
      "icon": "mlx",
      "default_port": 8080,
      "api": "openai-compatible",
      "version": "0.20.0"
    },
    "deploy": {
      "docker": null,
      "native": {
        "platforms": [
          "macos",
          "ios"
        ],
        "runtime": "embedded",
        "feature_flag": "inference-mlx",
        "binary_path": null,
        "bundle_path": null
      },
      "external": null
    },
    "model_preset": [
      {
        "id": "qwen3-5-0-8b-mlx-4bit",
        "display_name": "Qwen 3.5 0.8B (MLX 4-bit)",
        "repo": "mlx-community/Qwen3.5-0.8B-bit4",
        "quantization": "int4",
        "recommended": true
      }
    ]
  },
  {
    "engine": {
      "id": "ollama",
      "category": "llm",
      "name": "Ollama",
      "description_pl": "Lokalny daemon LLM z prostym API. Tryb external (zainstalowany przez usera) lub docker.",
      "description_en": "Local LLM daemon with simple API. External mode (user-installed) or docker.",
      "homepage": "https://ollama.com",
      "license": "MIT",
      "icon": "ollama",
      "default_port": 11434,
      "api": "ollama-native",
      "version": "0.4.0"
    },
    "deploy": {
      "docker": {
        "context_path": "llm/docker/ollama",
        "platforms": [
          "linux",
          "windows"
        ],
        "download_image": null,
        "download_size_mb": null
      },
      "native": null,
      "external": {
        "platforms": [
          "linux",
          "macos",
          "windows"
        ],
        "detection_binary": "ollama",
        "detection_endpoint": "http://localhost:11434",
        "detection_health_path": "/api/tags"
      }
    },
    "model_preset": [
      {
        "id": "qwen3-5-0-8b",
        "display_name": "Qwen 3.5 0.8B",
        "repo": "qwen3.5:0.8b",
        "quantization": null,
        "recommended": true
      }
    ]
  },
  {
    "engine": {
      "id": "sglang",
      "category": "llm",
      "name": "SGLang",
      "description_pl": "Serwer LLM z RadixAttention i strukturalnym output. Szybsze od vLLM dla zlozonych zapytan.",
      "description_en": "LLM server with RadixAttention and structured output. Faster than vLLM for complex prompts.",
      "homepage": "https://github.com/sgl-project/sglang",
      "license": "Apache-2.0",
      "icon": "sglang",
      "default_port": 30000,
      "api": "openai-compatible",
      "version": "0.3.5"
    },
    "deploy": {
      "docker": {
        "context_path": "llm/docker/sglang",
        "platforms": [
          "linux",
          "windows"
        ],
        "download_image": null,
        "download_size_mb": null
      },
      "native": {
        "platforms": [
          "linux",
          "windows"
        ],
        "runtime": "python-bundle",
        "feature_flag": null,
        "binary_path": null,
        "bundle_path": "llm/python/sglang"
      },
      "external": null
    },
    "model_preset": [
      {
        "id": "qwen3-5-0-8b",
        "display_name": "Qwen 3.5 0.8B",
        "repo": "Qwen/Qwen3.5-0.8B",
        "quantization": null,
        "recommended": true
      }
    ]
  },
  {
    "engine": {
      "id": "tensorrt-llm",
      "category": "llm",
      "name": "TensorRT-LLM",
      "description_pl": "Silnik LLM NVIDIA z optymalizacja TensorRT. Najszybsze inference na GPU NVIDIA.",
      "description_en": "NVIDIA's LLM engine with TensorRT optimization. Fastest inference on NVIDIA GPUs.",
      "homepage": "https://github.com/NVIDIA/TensorRT-LLM",
      "license": "Apache-2.0",
      "icon": "nvidia",
      "default_port": 8000,
      "api": "openai-compatible",
      "version": "0.13.0"
    },
    "deploy": {
      "docker": {
        "context_path": "llm/docker/tensorrt-llm",
        "platforms": [
          "linux",
          "windows"
        ],
        "download_image": null,
        "download_size_mb": null
      },
      "native": null,
      "external": null
    },
    "model_preset": [
      {
        "id": "llama-3-1-8b-instruct",
        "display_name": "Llama 3.1 8B Instruct",
        "repo": "meta-llama/Meta-Llama-3.1-8B-Instruct",
        "quantization": null,
        "recommended": true
      }
    ]
  },
  {
    "engine": {
      "id": "vllm",
      "category": "llm",
      "name": "vLLM",
      "description_pl": "Serwer LLM z PagedAttention i continuous batching dla NVIDIA GPU.",
      "description_en": "LLM server with PagedAttention and continuous batching for NVIDIA GPUs.",
      "homepage": "https://github.com/vllm-project/vllm",
      "license": "Apache-2.0",
      "icon": "vllm",
      "default_port": 8000,
      "api": "openai-compatible",
      "version": "0.6.3"
    },
    "deploy": {
      "docker": {
        "context_path": "llm/docker/vllm",
        "platforms": [
          "linux",
          "windows"
        ],
        "download_image": null,
        "download_size_mb": null
      },
      "native": {
        "platforms": [
          "linux",
          "windows"
        ],
        "runtime": "python-bundle",
        "feature_flag": null,
        "binary_path": null,
        "bundle_path": "llm/python/vllm"
      },
      "external": null
    },
    "model_preset": [
      {
        "id": "qwen3-5-0-8b",
        "display_name": "Qwen 3.5 0.8B",
        "repo": "Qwen/Qwen3.5-0.8B",
        "quantization": null,
        "recommended": true
      },
      {
        "id": "llama-3-1-8b-instruct",
        "display_name": "Llama 3.1 8B Instruct",
        "repo": "meta-llama/Meta-Llama-3.1-8B-Instruct",
        "quantization": null,
        "recommended": false
      }
    ]
  },
  {
    "engine": {
      "id": "parakeet",
      "category": "stt",
      "name": "Parakeet",
      "description_pl": "Szybki ASR od NVIDIA (NeMo). Niska latencja, dobra jakosc dla angielskiego.",
      "description_en": "NVIDIA's fast ASR (NeMo). Low latency, good quality for English.",
      "homepage": "https://github.com/NVIDIA/NeMo",
      "license": "Apache-2.0",
      "icon": null,
      "default_port": 5030,
      "api": "openai-compatible",
      "version": "2.0.0"
    },
    "deploy": {
      "docker": {
        "context_path": "stt/docker/parakeet",
        "platforms": [
          "linux",
          "windows"
        ],
        "download_image": null,
        "download_size_mb": null
      },
      "native": {
        "platforms": [
          "linux",
          "windows"
        ],
        "runtime": "python-bundle",
        "feature_flag": null,
        "binary_path": null,
        "bundle_path": "stt/python/parakeet"
      },
      "external": null
    },
    "model_preset": [
      {
        "id": "parakeet-tdt-1-1b",
        "display_name": "Parakeet TDT 1.1B",
        "repo": "nvidia/parakeet-tdt-1.1b",
        "quantization": null,
        "recommended": true
      }
    ]
  },
  {
    "engine": {
      "id": "qwen-asr",
      "category": "stt",
      "name": "Qwen3-ASR",
      "description_pl": "Multijezykowy ASR od Alibaba. Bardzo dobra jakosc dla chinskiego i angielskiego.",
      "description_en": "Alibaba's multilingual ASR. Excellent quality for Chinese and English.",
      "homepage": "https://github.com/QwenLM/Qwen-Audio",
      "license": "Apache-2.0",
      "icon": null,
      "default_port": 5030,
      "api": "openai-compatible",
      "version": "1.0.0"
    },
    "deploy": {
      "docker": {
        "context_path": "stt/docker/qwen-asr",
        "platforms": [
          "linux",
          "windows"
        ],
        "download_image": null,
        "download_size_mb": null
      },
      "native": {
        "platforms": [
          "linux",
          "windows"
        ],
        "runtime": "python-bundle",
        "feature_flag": null,
        "binary_path": null,
        "bundle_path": "stt/python/qwen-asr"
      },
      "external": null
    },
    "model_preset": [
      {
        "id": "qwen-audio-asr",
        "display_name": "Qwen-Audio ASR",
        "repo": "Qwen/Qwen-Audio",
        "quantization": null,
        "recommended": true
      }
    ]
  },
  {
    "engine": {
      "id": "whisper",
      "category": "stt",
      "name": "Whisper",
      "description_pl": "Silnik STT od OpenAI. Multijezykowa transkrypcja audio → tekst.",
      "description_en": "OpenAI's STT engine. Multilingual audio-to-text transcription.",
      "homepage": "https://github.com/openai/whisper",
      "license": "MIT",
      "icon": "whisper",
      "default_port": 5030,
      "api": "openai-compatible",
      "version": "1.7.4"
    },
    "deploy": {
      "docker": {
        "context_path": "stt/docker/whisper",
        "platforms": [
          "linux",
          "windows"
        ],
        "download_image": null,
        "download_size_mb": null
      },
      "native": {
        "platforms": [
          "linux",
          "macos",
          "windows",
          "ios",
          "android"
        ],
        "runtime": "embedded",
        "feature_flag": "inference-whisper",
        "binary_path": null,
        "bundle_path": null
      },
      "external": null
    },
    "model_preset": [
      {
        "id": "whisper-large-v3-turbo",
        "display_name": "Whisper Large v3 Turbo",
        "repo": "openai/whisper-large-v3-turbo",
        "quantization": null,
        "recommended": true
      },
      {
        "id": "whisper-base",
        "display_name": "Whisper Base",
        "repo": "openai/whisper-base",
        "quantization": null,
        "recommended": false
      }
    ]
  },
  {
    "engine": {
      "id": "sherpa-onnx",
      "category": "tts",
      "name": "sherpa-onnx",
      "description_pl": "Silnik TTS przez ONNX Runtime, wsparcie wielu jezykow.",
      "description_en": "TTS engine via ONNX Runtime, multilingual support.",
      "homepage": "https://github.com/k2-fsa/sherpa-onnx",
      "license": "Apache-2.0",
      "icon": "sherpa",
      "default_port": 5020,
      "api": "sherpa-tts",
      "version": "1.10.0"
    },
    "deploy": {
      "docker": {
        "context_path": "tts/docker/sherpa-onnx",
        "platforms": [
          "linux",
          "windows"
        ],
        "download_image": null,
        "download_size_mb": null
      },
      "native": {
        "platforms": [
          "linux",
          "macos",
          "windows"
        ],
        "runtime": "binary",
        "feature_flag": null,
        "binary_path": "tts/native/sherpa-onnx",
        "bundle_path": null
      },
      "external": null
    },
    "model_preset": [
      {
        "id": "vits-piper-en-amy-medium",
        "display_name": "VITS Piper en (Amy, medium)",
        "repo": "rhasspy/piper-voices/en/en_US/amy/medium",
        "quantization": null,
        "recommended": true
      }
    ]
  },
  {
    "engine": {
      "id": "voxcpm",
      "category": "tts",
      "name": "VoxCPM2",
      "description_pl": "Silnik TTS od OpenBMB. Naturalny glos, wiele jezykow.",
      "description_en": "OpenBMB's TTS engine. Natural voice, multilingual.",
      "homepage": "https://github.com/OpenBMB/VoxCPM",
      "license": "Apache-2.0",
      "icon": null,
      "default_port": 5020,
      "api": "openai-compatible",
      "version": "0.1.0"
    },
    "deploy": {
      "docker": {
        "context_path": "tts/docker/voxcpm",
        "platforms": [
          "linux",
          "windows"
        ],
        "download_image": null,
        "download_size_mb": null
      },
      "native": {
        "platforms": [
          "linux",
          "windows"
        ],
        "runtime": "python-bundle",
        "feature_flag": null,
        "binary_path": null,
        "bundle_path": "tts/python/voxcpm"
      },
      "external": null
    },
    "model_preset": [
      {
        "id": "voxcpm-base",
        "display_name": "VoxCPM2 Base",
        "repo": "openbmb/VoxCPM-base",
        "quantization": null,
        "recommended": true
      }
    ]
  },
  {
    "engine": {
      "id": "xtts",
      "category": "tts",
      "name": "XTTS v2",
      "description_pl": "Silnik TTS od Coqui z funkcja klonowania glosu (voice cloning).",
      "description_en": "Coqui TTS engine with voice cloning support.",
      "homepage": "https://github.com/coqui-ai/TTS",
      "license": "CPML",
      "icon": "xtts",
      "default_port": 5020,
      "api": "openai-compatible",
      "version": "2.0.3"
    },
    "deploy": {
      "docker": {
        "context_path": "tts/docker/xtts",
        "platforms": [
          "linux",
          "windows"
        ],
        "download_image": null,
        "download_size_mb": null
      },
      "native": {
        "platforms": [
          "linux",
          "windows"
        ],
        "runtime": "python-bundle",
        "feature_flag": null,
        "binary_path": null,
        "bundle_path": "tts/python/xtts"
      },
      "external": null
    },
    "model_preset": [
      {
        "id": "xtts-v2",
        "display_name": "XTTS v2",
        "repo": "coqui/XTTS-v2",
        "quantization": null,
        "recommended": true
      }
    ]
  }
];

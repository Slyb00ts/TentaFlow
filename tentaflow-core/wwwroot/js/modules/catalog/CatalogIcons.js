// =============================================================================
// Plik: modules/catalog/CatalogIcons.js
// Opis: Ikony SVG inline dla kafelkow katalogu uslug. Mapowanie per silnik
//       (engine.icon) z fallbackiem na ikone kategorii.
// =============================================================================

const CatalogIcons = (() => {
  'use strict';

  // Mapowanie identyfikatorow uslug/kategorii na nazwy funkcji ikon.
  const SERVICE_MAP = {
    tts: 'speaker',
    stt: 'mic',
    embeddings: 'vector',
    rag: 'search',
    tools: 'wrench',
    memory: 'brain',
    reranker: 'sort',
    llm: 'cpu',
    comfyui: 'image',
    'image-gen': 'image',
    agents: 'wrench',
    tentaflow: 'grid',
    bms: 'building',
    'llama-cpp': 'llamacpp',
  };

  // Wspolny poczatek SVG (stroke).
  function svgOpen(size) {
    return `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">`;
  }

  // Pomocnicza ikona literowa w prostokacie — jednolity styl dla brandow.
  function letter(label, size) {
    const s = size || 24;
    return `<svg xmlns="http://www.w3.org/2000/svg" width="${s}" height="${s}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">` +
      `<rect x="3" y="3" width="18" height="18" rx="3" ry="3"/>` +
      `<text x="12" y="16" text-anchor="middle" font-family="system-ui, sans-serif" font-size="11" font-weight="700" fill="currentColor" stroke="none">${label}</text>` +
      `</svg>`;
  }

  // Glosnik z falami dzwieku (TTS).
  function speaker(size = 24) {
    return `${svgOpen(size)}<polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5"/><path d="M15.54 8.46a5 5 0 0 1 0 7.07"/><path d="M19.07 4.93a10 10 0 0 1 0 14.14"/></svg>`;
  }

  // Mikrofon (STT).
  function mic(size = 24) {
    return `${svgOpen(size)}<path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z"/><path d="M19 10v2a7 7 0 0 1-14 0v-2"/><line x1="12" y1="19" x2="12" y2="23"/><line x1="8" y1="23" x2="16" y2="23"/></svg>`;
  }

  // Wezly polaczone liniami — graf/siec (embeddings).
  function vector(size = 24) {
    return `${svgOpen(size)}<circle cx="5" cy="6" r="2"/><circle cx="19" cy="6" r="2"/><circle cx="12" cy="18" r="2"/><circle cx="19" cy="18" r="2"/><line x1="6.7" y1="7.3" x2="10.6" y2="16.4"/><line x1="17.3" y1="7.3" x2="13.4" y2="16.4"/><line x1="17" y1="18" x2="14" y2="18"/><line x1="17.3" y1="7.3" x2="19" y2="16"/></svg>`;
  }

  // Lupa z dokumentem (RAG).
  function search(size = 24) {
    return `${svgOpen(size)}<circle cx="11" cy="11" r="8"/><line x1="21" y1="21" x2="16.65" y2="16.65"/><line x1="8" y1="8" x2="14" y2="8"/><line x1="8" y1="11" x2="14" y2="11"/><line x1="8" y1="14" x2="11" y2="14"/></svg>`;
  }

  // Klucz/narzedzia (tools, agents).
  function wrench(size = 24) {
    return `${svgOpen(size)}<path d="M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94l-3.76 3.76z"/></svg>`;
  }

  // Mozg (memory).
  function brain(size = 24) {
    return `${svgOpen(size)}<path d="M12 2a5 5 0 0 1 4.5 2.8A4 4 0 0 1 20 9a4 4 0 0 1-1.5 3.1A4.5 4.5 0 0 1 16 17H8a4.5 4.5 0 0 1-2.5-4.9A4 4 0 0 1 4 9a4 4 0 0 1 3.5-4.2A5 5 0 0 1 12 2z"/><path d="M12 2v20"/><path d="M8 9h8"/><path d="M7 13h10"/></svg>`;
  }

  // Procesor/chip (LLM, fallback).
  function cpu(size = 24) {
    return `${svgOpen(size)}<rect x="4" y="4" width="16" height="16" rx="2" ry="2"/><rect x="9" y="9" width="6" height="6"/><line x1="9" y1="1" x2="9" y2="4"/><line x1="15" y1="1" x2="15" y2="4"/><line x1="9" y1="20" x2="9" y2="23"/><line x1="15" y1="20" x2="15" y2="23"/><line x1="20" y1="9" x2="23" y2="9"/><line x1="20" y1="15" x2="23" y2="15"/><line x1="1" y1="9" x2="4" y2="9"/><line x1="1" y1="15" x2="4" y2="15"/></svg>`;
  }

  // Logo NVIDIA — kwadrat z literka N.
  function nvidia(size = 24) {
    return `${svgOpen(size)}<rect x="3" y="3" width="18" height="18" rx="3" ry="3"/><path d="M8 17V7l8 10V7"/></svg>`;
  }

  // Blyskawica (SGLang).
  function sglang(size = 24) {
    return `${svgOpen(size)}<polygon points="13 2 3 14 12 14 11 22 21 10 12 10 13 2"/></svg>`;
  }

  // Strzalki w gore — szybkosc (vLLM).
  function vllm(size = 24) {
    return `${svgOpen(size)}<line x1="12" y1="20" x2="12" y2="4"/><polyline points="8 8 12 4 16 8"/><line x1="6" y1="20" x2="6" y2="10"/><polyline points="2 14 6 10 10 14"/><line x1="18" y1="20" x2="18" y2="10"/><polyline points="14 14 18 10 22 14"/></svg>`;
  }

  // Serwer/box (Ollama).
  function ollama(size = 24) {
    return `${svgOpen(size)}<rect x="2" y="2" width="20" height="8" rx="2" ry="2"/><rect x="2" y="14" width="20" height="8" rx="2" ry="2"/><line x1="6" y1="6" x2="6.01" y2="6"/><line x1="6" y1="18" x2="6.01" y2="18"/></svg>`;
  }

  // Terminal (LLama.cpp).
  function llamacpp(size = 24) {
    return `${svgOpen(size)}<polyline points="4 17 10 11 4 5"/><line x1="12" y1="19" x2="20" y2="19"/></svg>`;
  }

  // Apple MLX — literka M.
  function mlx(size = 24) {
    return letter('M', size);
  }

  // Whisper (STT).
  function whisper(size = 24) {
    return letter('W', size);
  }

  // Sherpa-onnx — sylwetka gory.
  function sherpa(size = 24) {
    return `${svgOpen(size)}<polyline points="3 20 9 9 13 14 17 7 21 20"/><line x1="3" y1="20" x2="21" y2="20"/></svg>`;
  }

  // XTTS — literka X.
  function xtts(size = 24) {
    return letter('X', size);
  }

  // Obraz/krajobraz (image generation).
  function image(size = 24) {
    return `${svgOpen(size)}<rect x="3" y="3" width="18" height="18" rx="2" ry="2"/><circle cx="8.5" cy="8.5" r="1.5"/><polyline points="21 15 16 10 5 21"/></svg>`;
  }

  // Siatka/dashboard (TentaFlow).
  function grid(size = 24) {
    return `${svgOpen(size)}<rect x="3" y="3" width="7" height="7"/><rect x="14" y="3" width="7" height="7"/><rect x="3" y="14" width="7" height="7"/><rect x="14" y="14" width="7" height="7"/></svg>`;
  }

  // Sortowanie (reranker).
  function sort(size = 24) {
    return `${svgOpen(size)}<line x1="4" y1="6" x2="16" y2="6"/><line x1="4" y1="12" x2="13" y2="12"/><line x1="4" y1="18" x2="10" y2="18"/><polyline points="18 15 21 18 18 21"/><line x1="21" y1="18" x2="21" y2="6"/></svg>`;
  }

  // Budynek (BMS).
  function building(size = 24) {
    return `${svgOpen(size)}<rect x="4" y="2" width="16" height="20" rx="1"/><line x1="9" y1="22" x2="9" y2="16"/><line x1="15" y1="22" x2="15" y2="16"/><rect x="8" y="6" width="3" height="3"/><rect x="13" y="6" width="3" height="3"/><rect x="8" y="11" width="3" height="3"/><rect x="13" y="11" width="3" height="3"/></svg>`;
  }

  // Kontener Docker — prostokat z modulami i fragmentem wieloryba.
  function docker(size = 24) {
    return `${svgOpen(size)}<rect x="2" y="9" width="20" height="11" rx="1"/><rect x="5" y="6" width="3" height="3"/><rect x="9" y="6" width="3" height="3"/><rect x="13" y="6" width="3" height="3"/><rect x="9" y="2" width="3" height="3"/><path d="M22 13s-1 2-3 2c-1 0-2-1-2-2"/></svg>`;
  }

  // Blyskawica — natywne uruchomienie (proces hosta).
  function native(size = 24) {
    return `${svgOpen(size)}<polygon points="13 2 3 14 12 14 11 22 21 10 12 10 13 2"/></svg>`;
  }

  // Lancuch — zewnetrzne polaczenie (existing service).
  function external(size = 24) {
    return `${svgOpen(size)}<path d="M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71"/><path d="M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71"/></svg>`;
  }

  // Pakiet/paczka — fallback dla nieznanego trybu deploy.
  function pkg(size = 24) {
    return `${svgOpen(size)}<path d="M21 16V8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16z"/><polyline points="3.27 6.96 12 12.01 20.73 6.96"/><line x1="12" y1="22.08" x2="12" y2="12"/></svg>`;
  }

  // Gwiazdka — wskaznik rekomendowanego presetu.
  function star(size = 24) {
    return `${svgOpen(size)}<polygon points="12 2 15.09 8.26 22 9.27 17 14.14 18.18 21.02 12 17.77 5.82 21.02 7 14.14 2 9.27 8.91 8.26 12 2"/></svg>`;
  }

  // Rejestr funkcji ikon.
  const ICONS = {
    speaker, mic, vector, search, wrench, brain, cpu, nvidia,
    sglang, vllm, ollama, llamacpp, mlx, whisper, sherpa, xtts,
    image, grid, building, sort,
    docker, native, external, pkg, star
  };

  // Pobranie ikony po identyfikatorze (klucz silnika lub kategorii).
  function get(iconId, size) {
    const fnName = SERVICE_MAP[iconId] || iconId;
    const fn = ICONS[fnName];
    return fn ? fn(size) : '';
  }

  // Render z fallbackiem na 'cpu' (uzywane przez ServiceCatalog).
  function render(iconKey, size) {
    const html = get(iconKey, size);
    if (html) return html;
    return cpu(size);
  }

  return Object.assign({}, ICONS, { get, render });
})();

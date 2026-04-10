# TODOS

## Design Debt

### Utworz DESIGN.md — formalny design system
- **What:** Utworz DESIGN.md dokumentujacy design system projektu: tokeny (z variables.css), wzorce komponentow (karty, gauge, modale, sparklines), styl ikon SVG, wytyczne a11y.
- **Why:** Bez formalnego design system kazdy developer zgaduje styl. variables.css definiuje tokeny ale nie dokumentuje kiedy i jak ich uzywac.
- **Pros:** Spojnosc wizualna, szybsze onboardowanie, mniej review comments o stylu.
- **Cons:** Wymaga utrzymania dokumentu przy zmianach.
- **Context:** Decyzja z /plan-design-review 2026-03-31. Plan node detail dashboard redesign dodal wiele nowych decyzji (sparkline spec, SVG icons, a11y labels) ktore powinny byc w jednym miejscu.
- **Depends on:** Nic. Mozna uruchomic `/design-consultation` w dowolnym momencie.

## Meeting Bot

### Streaming STT — partial transcripts for real-time interjection
- **What:** Send audio chunks to STT incrementally instead of waiting for VAD Transition (end of utterance). Get partial transcripts every 200-500ms.
- **Why:** Current approach buffers entire utterance (could be 10-30 seconds), then sends as one batch. For an active meeting participant that needs to decide when to interject, you want real-time transcript, not a 2-second delay after the speaker stops.
- **Pros:** Lower latency for interjection decisions, better UX for transcript display, enables streaming transcript to dashboard/event bus.
- **Cons:** Requires streaming Whisper support on the STT backend. RouterClient::transcribe() currently does request/response, not streaming. QUIC ModelStreamChunk exists in protocol but STT backend doesn't implement streaming mode yet.
- **Context:** Decision from /plan-eng-review 2026-04-09. The QUIC channel supports streaming (ModelStreamChunk in tentaflow-protocol). The STT whisper.rs would need a streaming wrapper around whisper.cpp's progressive decode API.
- **Depends on:** Phase 1 audio pipeline fix (parec/pacat) + Phase 2 brain loop (STT → LLM → TTS wiring).

# Agents

Autonomiczne agenty — boty spotkan, browser agents, asystenci email.

## Struktura

- `_services/*.toml` — manifesty silnikow (deklaratywny opis: warianty, GPU, deployment)
- `docker/<engine>/` — kontenery Docker (Teams meeting bot, browser agents)

## Obslugiwane silniki (planowane)

- Teams Meeting Bot (docker — Chromium + Silero VAD + audio)
- Slack Bot (docker — webhook agent)
- Browser Agent (docker — Playwright + LLM)
- Email Assistant (docker — IMAP/SMTP + LLM)
- Discord Bot (docker — webhook agent)

## Jak dodac nowy silnik

1. Utworz `_services/<engine-id>.toml` zgodnie z `tentaflow-containers/_schema/SCHEMA.md`
2. Dla wariantu `docker`: dodaj `docker/<engine-id>/` z Dockerfile + entrypoint.sh + config.default.toml + build.sh
3. `cargo build` w tentaflow-core/ zwaliduje TOML i wygeneruje wpisy w GUI

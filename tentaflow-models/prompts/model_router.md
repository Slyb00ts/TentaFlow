# PROMPT GENERATORA: MODEL ROUTER

KRYTYCZNE: Twój JEDYNY output to surowe linie JSONL. ZERO komentarzy, ZERO markdown, ZERO tabel. Pierwszym znakiem outputu musi być `{`. Cokolwiek innego = porażka.

Generujesz dane treningowe JSONL do modelu AI, który uczy się wybierać odpowiedni model LLM dla danego zadania. Model dostaje listę dostępnych modeli (filtrowaną per user) i wybiera najlepszy.

## ZADANIE
Wygeneruj **30 linii JSONL** (12 PL, 10 EN, 8 Inne: DE, FR, ES, RU, ZH, JA).
Format: `{"input": "<|model|>\nMODELS:\n...\nTASK: ...", "output": "wybór modelu lub #UNAVAILABLE"}`

## FORMAT

### Input
```
<|model|>
MODELS:
  default|category=chat|desc=General purpose assistant for conversation
  code-gen|category=code|desc=Code generation and review, Rust Python TypeScript Go C#
TASK: User wants to write a Python script for CSV parsing
```

### Output — model dostępny
```
code-gen|task=generate_code|lang=python|context=CSV parsing script
```

### Output — model niedostępny
```
#UNAVAILABLE|reason=No image generation model available. You have access to: chat, code models.
```

## REGUŁY

1. Wybieraj model z NAJLEPSZĄ kategorią dla zadania
2. Jeśli żaden model nie pasuje idealnie → `default` (bezpieczny fallback)
3. Jeśli zadanie WYMAGA modelu którego NIE MA na liście → `#UNAVAILABLE` z opisem co brakuje
4. Output: `alias|task=krótki_opis|parametry_kontekstowe`
5. `#UNAVAILABLE` MUSI zawierać `reason=` z wyjaśnieniem PO POLSKU/ANGIELSKU (w języku taska)
6. Przy #UNAVAILABLE sugeruj co user MA dostępne jako alternatywę

## KATEGORIE MODELI

| Kategoria | Kiedy | Przykład zadania |
|-----------|-------|-----------------|
| chat | Rozmowa, pytania ogólne, wyjaśnienia | "Wyjaśnij czym jest REST API" |
| code | Pisanie kodu, review, debug, refactoring | "Napisz funkcję sortującą" |
| image | Generowanie obrazów, grafik, logo | "Wygeneruj logo firmy" |
| audio | Transkrypcja, text-to-speech, analiza audio | "Przepisz to nagranie" |
| medical | Wiedza medyczna, diagnostyka, leki | "Jakie są objawy cukrzycy" |
| legal | Analiza prawna, umowy, regulacje | "Czy ta klauzula jest zgodna z RODO" |
| embedding | Generowanie embeddingów tekstu | "Zrób embedding tego dokumentu" |
| vision | Analiza obrazów, OCR, rozpoznawanie | "Co jest na tym zdjęciu" |
| translation | Tłumaczenia specjalistyczne | "Przetłumacz umowę na niemiecki" |

## DOSTĘPNE KOMBINACJE MODELI (wylosuj 3-5 per rekord)

Każdy rekord ma INNY zestaw modeli — symulacja różnych userów z różnymi uprawnieniami:

**Zestaw pełny (admin):** default, code-gen, vision, medical, legal, image-gen, audio-stt
**Zestaw deweloper:** default, code-gen
**Zestaw biznes:** default, image-gen, translation
**Zestaw minimalny:** default
**Zestaw medyczny:** default, medical
**Zestaw kreatywny:** default, image-gen, audio-stt, vision

## PROPORCJE
- 50% model dostępny — prawidłowy wybór
- 20% model niedostępny — `#UNAVAILABLE` z reason i sugestią
- 15% nieoczywisty wybór — np. "przeanalizuj kod" → code-gen (nie default)
- 15% fallback do default — zadanie ogólne, żaden specjalny model nie pasuje lepiej

## RÓŻNORODNOŚĆ ZADAŃ (wylosuj per rekord)

- Programowanie: pisanie kodu, debug, code review, refactoring, testy, architektura
- Obraz: logo, diagram, mockup, zdjęcie produktu, avatar, baner
- Audio: transkrypcja spotkania, podcast, TTS, rozpoznawanie mówcy
- Medycyna: objawy, diagnostyka, interakcje leków, zalecenia
- Prawo: analiza umowy, RODO, compliance, klauzule
- Tłumaczenie: umowa EN→PL, dokumentacja techniczna, lokalizacja
- Ogólne: rozmowa, wyjaśnienie, small talk, podsumowanie

## PRZYKŁADY

{"input": "<|model|>\nMODELS:\n  default|category=chat|desc=General purpose assistant\n  code-gen|category=code|desc=Code generation, Rust Python TypeScript\nTASK: Write a Rust function that validates email addresses", "output": "code-gen|task=generate_code|lang=rust|context=email validation function"}
{"input": "<|model|>\nMODELS:\n  default|category=chat|desc=General purpose assistant\nTASK: Generate a logo for our startup", "output": "#UNAVAILABLE|reason=No image generation model available. You have access to: chat. Consider describing what you need and I can help with text-based alternatives."}
{"input": "<|model|>\nMODELS:\n  default|category=chat|desc=Ogólny asystent\n  code-gen|category=code|desc=Generowanie kodu\n  medical|category=medical|desc=Wiedza medyczna i diagnostyka\nTASK: Jakie mogą być przyczyny bólu w klatce piersiowej?", "output": "medical|task=symptom_analysis|context=chest pain differential diagnosis"}
{"input": "<|model|>\nMODELS:\n  default|category=chat|desc=General assistant\nTASK: Explain quantum computing", "output": "default|task=explanation|context=quantum computing basics"}

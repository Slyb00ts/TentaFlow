# TentaFlow Tool Calling — Architektura Pipeline

## Problem

System ma ~800 addonów z ~20,000 tooli. Model 0.8B ma kontekst 4-8k tokenów.
Jeden tool z opisem i parametrami = ~50-100 tokenów. 20,000 tooli = ~1-2M tokenów — nie zmieści się.
Nawet 100 tooli to za dużo dla małego modelu.

## Rozwiązanie: 3-etapowy pipeline

```
User prompt
    │
    ▼
┌─────────────────────────┐
│ ETAP 1: Czy potrzeba    │  ← Qwen3.5-0.8B (klasyfikacja)
│ toola w ogóle?           │     Input: sam prompt
│ TAK / NIE               │     Output: #TOOL / #TEXT
└────────┬────────────────┘
         │ TAK
         ▼
┌─────────────────────────┐
│ ETAP 2: Który tool?     │  ← Embedding search (cosine similarity)
│ 20,000 → top 5-10       │     Prekomputowane embeddingi tooli
│ Semantic retrieval       │     Szybkie: <5ms
└────────┬────────────────┘
         │ top 5-10 kandydatów
         ▼
┌─────────────────────────┐
│ ETAP 3: Wywołaj tool    │  ← Qwen3.5-0.8B (generacja TOON)
│ z parametrami            │     Input: prompt + 5-10 tooli
│                          │     Output: @addon.func|param=val
└─────────────────────────┘
```

## Etap 1 — Klasyfikacja intencji

Szybka decyzja: czy prompt wymaga narzędzia, czy to pytanie o wiedzę / small talk.

**Input:** sam prompt użytkownika (bez listy tooli — oszczędność tokenów)
```
<|intent|>
Wyślij Kasi maila że spotkanie przesunięte na 15:00
```

**Output:** jeden token
```
#TOOL
```

Lub:
```
#TEXT
```

**Trening:** proste pary (prompt → #TOOL/#TEXT). Alternatywnie: osobny BERT classifier zamiast Qwen.

**Czas:** <10ms (kilka tokenów outputu)

### Kiedy #TOOL, kiedy #TEXT

| Prompt | Decyzja |
|--------|---------|
| "Wyślij maila do Kasi" | #TOOL |
| "Sprawdź kalendarz na jutro" | #TOOL |
| "Znajdź raport w SharePoint" | #TOOL |
| "Jaka jest stolica Francji?" | #TEXT |
| "Wyjaśnij mi czym jest REST API" | #TEXT |
| "Dzięki, to wszystko" | #TEXT |
| "Podsumuj naszą rozmowę" | #TEXT |
| "Zaplanuj spotkanie z Markiem" | #TOOL |

## Etap 2 — Semantic Retrieval (szukanie toola)

Każdy z 20,000 tooli ma **prekomputowany embedding** wygenerowany z tekstu opisowego.

### Budowanie indeksu (jednorazowo, przy instalacji addona)

Dla każdego toola generujemy tekst do embedowania:
```
outlook.send_email: Wysyła email do odbiorcy.
Parametry: to (adres email odbiorcy), subject (temat wiadomości),
body (treść wiadomości), cc (kopia do), is_html (czy format HTML)
```

Embedding tego tekstu (np. 384 lub 768 wymiarów) trafia do **indeksu wektorowego** (FAISS, qdrant, lub własna implementacja w Rust).

### Wyszukiwanie (przy każdym query)

1. Zrób embedding zapytania użytkownika
2. Szukaj top-10 najbliższych tooli (cosine similarity)

**Przykład:** "Wyślij Kasi maila że spotkanie przesunięte na 15:00"

```
1. outlook.send_email          (0.94)  ← najlepszy match
2. teams.send_message           (0.87)
3. outlook.reply_email          (0.82)
4. teams.list_messages           (0.61)
5. outlook.list_emails           (0.58)
```

### Modele embeddingowe (off-the-shelf, bez treningu)

| Model | Wymiary | Rozmiar | Uwagi |
|-------|---------|---------|-------|
| nomic-embed-text | 768 | 137M | Dobry, wielojęzyczny |
| BGE-M3 | 1024 | 568M | Najlepszy multilingual |
| E5-small-v2 | 384 | 33M | Najmniejszy, angielski |
| mxbai-embed-large | 1024 | 335M | Dobra jakość |

### Skalowanie

| Tooli | Czas retrieval | RAM na indeks |
|-------|---------------|---------------|
| 30 | <1ms | <1MB |
| 1,000 | <2ms | ~3MB |
| 20,000 | <5ms | ~60MB |
| 100,000 | <10ms | ~300MB |

Model ZAWSZE widzi max 5-10 tooli — reszta to retrieval.

### Aktualizacja indeksu

Indeks jest aktualizowany automatycznie:
- Przy instalacji addona → dodaj embeddingi jego tooli
- Przy odinstalowaniu → usuń
- Przy aktualizacji → zastąp
- Dynamiczna rejestracja (`register_tool()` w `on_start()`) → dodaj

## Etap 3 — Tool Call z parametrami (Qwen3.5-0.8B)

Model dostaje TYLKO top 5-10 kandydatów z etapu 2 w kompaktowym formacie.

**Input:**
```
<|tools|>
outlook: send_email(to*,subject*,body*,cc?,is_html?) reply_email(message_id*,body*)
teams: send_message(to*,message*,channel_id?) list_messages(chat_id*)
<|query|>
Wyślij Kasi maila że spotkanie przesunięte na 15:00
```

**Output (format TOON):**
```
@outlook.send_email|to=kasia|subject=Zmiana terminu spotkania|body=Cześć, spotkanie zostało przesunięte na 15:00.
```

### Format TOON — reguły

```
# Single tool call
@addon_id.function_name|param1=wartość1|param2=wartość2

# Multi tool chain (osobne linie)
@addon1.func1|param=val
@addon2.func2|param=val

# Brak toola (nie powinno się zdarzyć po etapie 1, ale safety fallback)
#TEXT
Odpowiedź tekstowa.

# Brak wymaganych parametrów
#MISSING@addon.func|known_param=wartość|missing_param=?
```

Reguły:
- `@` = początek tool call
- `|` = separator między parametrami
- `=` = separator klucz-wartość
- `?` = brakujący required parametr
- `*` w definicji = required, `?` = optional
- Brak cudzysłowów — mniej tokenów, mniej błędów
- Każdy tool call na osobnej linii

### Czas: <50ms (mały model, krótki output)

## Pełne przykłady end-to-end

### Przykład 1: Proste wysłanie maila

**User:** "Wyślij Kasi maila że spotkanie przesunięte na 15:00"

| Etap | Input | Output | Czas |
|------|-------|--------|------|
| 1. Intent | prompt | `#TOOL` | 5ms |
| 2. Retrieval | embedding query | top-5 tooli | 3ms |
| 3. Tool call | prompt + 5 tooli | `@outlook.send_email\|to=kasia\|subject=Zmiana terminu\|body=...` | 40ms |

**Łączny czas: ~48ms**

### Przykład 2: Chain — szukaj i wyślij

**User:** "Znajdź raport kwartalny i wyślij go Markowi na Teams"

| Etap | Input | Output | Czas |
|------|-------|--------|------|
| 1. Intent | prompt | `#TOOL` | 5ms |
| 2. Retrieval | embedding query | top-8 tooli | 3ms |
| 3. Tool call | prompt + 8 tooli | `@sharepoint-rag.search_files\|query=raport kwartalny`<br>`@teams.send_message\|to=marek\|message=Przesyłam raport kwartalny` | 60ms |

### Przykład 3: Brakujące parametry

**User:** "Wyślij coś do Kasi"

| Etap | Input | Output | Czas |
|------|-------|--------|------|
| 1. Intent | prompt | `#TOOL` | 5ms |
| 2. Retrieval | embedding query | top-5 tooli | 3ms |
| 3. Tool call | prompt + 5 tooli | `#MISSING@outlook.send_email\|to=kasia\|subject=?\|body=?` | 30ms |

**System do usera:** "Jaki temat i treść maila do Kasi?"

### Przykład 4: Nie wymaga toola

**User:** "Jaka jest stolica Francji?"

| Etap | Input | Output | Czas |
|------|-------|--------|------|
| 1. Intent | prompt | `#TEXT` | 5ms |

**Koniec — przekaż do głównego LLM.**

### Przykład 5: Niejednoznaczne — Teams czy mail?

**User:** "Napisz do Ani"

| Etap | Input | Output | Czas |
|------|-------|--------|------|
| 1. Intent | prompt | `#TOOL` | 5ms |
| 2. Retrieval | embedding query | `outlook.send_email (0.83)`, `teams.send_message (0.81)` | 3ms |
| 3. Tool call | prompt + top-5 | `#MISSING@outlook.send_email\|to=ania\|subject=?\|body=?` | 30ms |

Model domyślnie wybiera email (wyższy score) i pyta o brakujące dane.

### Przykład 6: Tool z addona którego user nie zna

**User:** "Zrób embeddingi tego tekstu: machine learning is fun"

| Etap | Input | Output | Czas |
|------|-------|--------|------|
| 1. Intent | prompt | `#TOOL` | 5ms |
| 2. Retrieval | embedding query | `embeddings-chunker.embed_text (0.95)` | 3ms |
| 3. Tool call | prompt + top-3 | `@embeddings-chunker.embed_text\|text=machine learning is fun` | 25ms |

## Co trzeba wytrenować / zbudować

| Komponent | Technologia | Trening? | Priorytet |
|-----------|-------------|----------|-----------|
| Etap 1: Intent classifier | Qwen3.5-0.8B fine-tuned | TAK | Wysoki |
| Etap 2: Tool embeddings | nomic-embed / BGE-M3 | NIE (off-the-shelf) | Wysoki |
| Etap 2: Indeks wektorowy | FAISS / własny Rust | NIE (kod) | Wysoki |
| Etap 3: Tool caller | Qwen3.5-0.8B fine-tuned | TAK | Wysoki |
| Parser TOON | Deterministyczny parser | NIE (kod) | Średni |
| Auto-indeksowanie | Hook na install/uninstall addona | NIE (kod) | Średni |

### Uwaga o etapie 1 i 3

Etap 1 (intent) i etap 3 (tool call) mogą używać **tego samego modelu** Qwen3.5-0.8B — fine-tuned na oba zadania jednocześnie. Model rozróżnia task po special tokenach:
- `<|intent|>` → klasyfikacja #TOOL/#TEXT
- `<|tools|>...<|query|>` → generacja TOON

Jeden model, dwa tryby — oszczędność pamięci GPU.

## Dane treningowe — co generować

### Dla etapu 1 (intent)
```jsonl
{"input": "<|intent|>\nWyślij Kasi maila", "output": "#TOOL"}
{"input": "<|intent|>\nJaka jest stolica Francji?", "output": "#TEXT"}
```
~2000 rekordów, proporcja 50/50.

### Dla etapu 3 (tool calling)
```jsonl
{"input": "<|tools|>\noutlook: send_email(to*,subject*,body*,cc?) ...\n<|query|>\nWyślij Kasi maila o spotkaniu", "output": "@outlook.send_email|to=kasia|subject=Spotkanie|body=Cześć, informuję o spotkaniu."}
```
~5000 rekordów, proporcje: 40% single / 15% missing / 15% chain / 15% #TEXT fallback / 5% niedostępny / 10% niejednoznaczny.

### Łącznie: ~7000 rekordów treningowych

## Skalowalność na 20,000 tooli

Model NIE musi znać wszystkich tooli. Model umie:
1. Rozpoznać intencję (etap 1) — bez wiedzy o toolach
2. Wybrać z **małej listy kandydatów** (etap 3) — lista pochodzi z retrieval (etap 2)
3. Wygenerować poprawny format TOON z parametrami

Wiedzę o tym JAKIE toole istnieją ma **indeks wektorowy** — budowany automatycznie z manifestów addonów przy ich instalacji. Model jest agnostyczny wobec liczby tooli w systemie.

Dodanie nowego addona z 25 toolami:
1. Zainstaluj addon → manifest.toml parsowany automatycznie
2. Wygeneruj embeddingi dla 25 tooli → dodaj do indeksu wektorowego
3. Gotowe — model od razu je "widzi" przez retrieval

**Nie trzeba retrenować modelu przy dodawaniu nowych addonów.**

## Ekosystem addonów — architektura danych

### Podział odpowiedzialności

```
manifest.toml    = CO addon potrafi (dane maszynowe, parsowalne)
                   → keywords, disambiguation, parameters schema, category
                   → zapisywane do DB przy instalacji
                   → używane przez retrieval i model

SKILL.md         = JAK używać addona (instrukcja dla LLM, dokumentacja)
                   → czysty markdown, EN, bez frontmatter
                   → TOON examples, scenariusze, uwagi
                   → zapisywany do DB, dostępny przez API

addon.wasm       = KOD addona (runtime)
```

### Język

- **manifest.toml** — angielski (keywords, opisy, disambiguation)
- **SKILL.md** — angielski (uniwersalny, cross-language embedding)
- **Wielojęzyczność** — rozwiązana na poziomie modelu:
  - Model embeddingowy jest multilingual — "wyślij" ≈ "send" w przestrzeni wektorowej
  - Dane treningowe generowane w wielu językach (PL, EN, DE, FR, ES...) przez pipeline
  - Twórca addona pisze TYLKO po angielsku — zero dodatkowej pracy

### manifest.toml — format z keywords i disambiguation

```toml
[addon]
id = "outlook"
name = "Microsoft Outlook"
category = "communication"
keywords = ["mail", "email", "inbox", "message", "send", "reply", "attachment"]

[[addon.disambiguation]]
trigger = ["send email", "write email", "compose mail"]
prefer = "outlook.send_email"
over = "teams.send_message"
when = "formal context, with subject line or attachment"

[tools.send_email]
description = "Send an email message"
keywords = ["send", "email", "compose", "write", "message"]
[tools.send_email.parameters]
type = "object"
required = ["to", "subject", "body"]
```

### SKILL.md — czysty EN markdown

```markdown
# Outlook

Microsoft Outlook addon provides email access via Microsoft Graph API.

## Tools

### outlook.send_email
Send a new email message.

When to use:
- User wants to send, compose, or write an email
- User mentions subject line, CC, attachments

TOON examples:
- `@outlook.send_email|to=jan@company.com|subject=Report|body=Quarterly report attached.`
- `#MISSING@outlook.send_email|to=kate|subject=?|body=?`

## Scenarios

### Check inbox and reply
\`\`\`toon
@outlook.list_emails|filter=isRead eq false
@outlook.read_email|message_id={result}
@outlook.reply_email|message_id={result}|body=Thanks, confirmed.
\`\`\`
```

### Co idzie do indeksu wektorowego (etap 2)

Embedding per tool generowany z połączenia:
```
{addon_id}.{tool_name}: {description}
Keywords: {keywords z manifest.toml}
{fragment SKILL.md dla tego toola — sekcja "When to use"}
```

Przykład:
```
outlook.send_email: Send an email message
Keywords: send, email, compose, write, message
When to use: User wants to send, compose, or write an email. User mentions subject line, CC, attachments.
```

### Co idzie do modelu (etap 3)

Kompaktowa lista tooli z keywords jako hinty:
```
<|tools|>
outlook: send_email(to*,subject*,body*,cc?,is_html?) [send,email,compose]
outlook: search_emails(query*,folder?,from_date?,has_attachment?) [search,find,filter]
teams: send_message(to*,message*,channel_id?) [chat,message,teams]
<|query|>
Send Kate an email about the deadline change
```

### Disambiguation flow

Gdy retrieval zwróci 2+ toole z podobnym score:
1. Sprawdź `disambiguation` rules z manifest.toml
2. Jeśli prompt matchuje trigger → użyj prefer
3. Jeśli brak matcha → model decyduje na podstawie kontekstu

## Analiza thirdparty (OpenFang, OpenClaw, ZeroClaw)

### Jak robią tool calling

Żaden z trzech projektów **nie trenuje własnego modelu** do tool calling. Wszystkie polegają na dużych LLM-ach (GPT-4, Claude, Gemini) i przekazują definicje tooli w prompcie jako JSON Schema.

| Aspekt | OpenFang (Rust) | OpenClaw (TypeScript) | ZeroClaw (Rust) |
|--------|----------------|----------------------|-----------------|
| Tool definition | JSON Schema (serde_json) | TypeBox runtime types | JSON Schema (serde_json) |
| Tool routing | Policy-based (glob patterns) | Profile + per-channel | Keyword-based groups |
| Filtrowanie tooli | deny-wins policy, named groups | Deferred loading, tool_search | `filter_tool_specs_for_turn()` |
| Built-in tooli | 53+ | 50+ (84 extensions) | 106+ |
| Schema normalization | Per-provider (Anthropic, Gemini, Groq) | Per-extension | 4 strategies (Gemini najrestrykcyjniejszy) |
| Max iterations | 50 | Configurable | 10 |
| Security | WASM sandbox + taint tracking | Docker sandbox + policy | Autonomy levels + allowlisting |

### Kluczowe wnioski

1. **Brak fine-tuningu** — wszystkie projekty zakładają że LLM z pudełka ogarnia tool calling. My robimy coś czego oni nie mają: mały, szybki, dedykowany model.

2. **Schema normalization jest krytyczna** — Gemini odrzuca połowę JSON Schema keywords ($ref, $defs, additionalProperties, pattern, format, minLength, maxLength...). ZeroClaw ma 4 strategie czyszczenia. To potwierdza słuszność formatu TOON — omijamy problem normalizacji.

3. **Filtrowanie tooli** — ZeroClaw filtruje toole per-turn po keywordach w wiadomości usera. To jest prymitywna wersja naszego etapu 2 (semantic retrieval). Nasz jest lepszy bo używa embeddingów zamiast keyword match.

4. **Loop detection** — OpenFang wykrywa pętle tool-call przez SHA256 hashowanie. Warto zapamiętać dla przyszłej integracji.

## Status implementacji (zrealizowane)

### Zmiany w tentaflow-core

| Komponent | Zmiana | Status |
|-----------|--------|--------|
| `mod.rs` | `ManifestTool.keywords`, `ToolDefinition.keywords`, `AddonManifest.keywords/category/disambiguation`, `DisambiguationRule` struct | Done |
| `migrations.rs` | Migracja 25 (keywords_json, skill_md, category) + 26 (disambiguation_json) | Done |
| `lifecycle.rs` | Parsowanie keywords/category/disambiguation z manifestu, zapis SKILL.md do DB | Done |
| `tool_dispatch.rs` | `get_tools_for_llm()` zwraca keywords | Done |
| `api_addon_system.rs` | skill_md z DB zamiast None | Done |
| `host_functions/mod.rs` | keywords_json w INSERT INTO addon_tools | Done |

### Zaktualizowane addony

| Addon | manifest.toml | SKILL.md | Język |
|-------|--------------|----------|-------|
| outlook | keywords EN, category, disambiguation | Czysty EN markdown + TOON | EN |
| teams | keywords EN, category, disambiguation | Czysty EN markdown + TOON | EN |
| sharepoint-rag | keywords EN, category | Czysty EN markdown + TOON | EN |
| embeddings-chunker | keywords EN, category | Czysty EN markdown + TOON (nowy) | EN |
| template (SDK) | keywords EN, category | Czysty EN markdown + TOON | EN |

## Następne kroki

1. **Generowanie danych treningowych** — GENDATA_PROMPT.md + pipeline w tentaflow-models/toolcalling/
2. **Trening Qwen3.5-0.8B** — QLoRA na RTX 4090 z danymi TOON
3. **Indeks wektorowy** — budowanie embeddingów z manifest.toml + SKILL.md
4. **Integracja z tentaflow-core** — parser TOON w Rust, podłączenie do ToolDispatcher

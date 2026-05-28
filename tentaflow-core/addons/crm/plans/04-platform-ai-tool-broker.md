# 04 · Platform — AI Tool Broker

**Mockup:** [C11 AI chat + confirm](../../../../../.gstack/projects/Slyb00ts-TentaFlow/designs/crm-v1/c11-ai-chat.html), [P3 AI Tools registry](../../../../../.gstack/projects/Slyb00ts-TentaFlow/designs/crm-v1/p03-ai-tools.html)

## Cel

Globalna pula narzędzi AI (LLM tools) z konkretnych addonów, kontrolowana centralnie. LLM **nigdy** nie woła host fn addona bezpośrednio. Woła **Broker**, który:

1. Sprawdza grants użytkownika i addona
2. Pokazuje **confirm dialog z diffem** dla mutacji (przed wykonaniem)
3. Loguje każde wywołanie do audit log
4. Routuje do właściwego addona

Bez Brokera nie ma bezpiecznej integracji AI z biznesowymi addonami.

## Domeny i encje

### `ai_tools` (rejestr — zasilany z manifestów addonów)

| Pole | Typ | Opis |
|---|---|---|
| `id` | UUID PK | |
| `name` | TEXT UNIQUE | Np. `crm.create_deal`, `email.draft_followup` |
| `addon_instance` | TEXT | Np. `crm/main` |
| `description` | TEXT | Co robi — dla LLM-a (musi być jasny, krótki) |
| `input_schema` | JSONB | JSON Schema dla argumentów |
| `output_schema` | JSONB | JSON Schema dla wyniku |
| `risk` | ENUM | `read | draft | act` |
| `required_grants` | TEXT[] | Lista grant'ów (np. `["crm.deal.write", "contacts.read_basic"]`) |
| `confirmation` | ENUM | `none | one_click | required` |
| `audit_level` | ENUM | `summary | full` |
| `rate_limit_per_user_per_min` | INT | |
| `available_in_contexts` | TEXT[] | Gdzie tool jest widoczny dla LLM (np. `["chat.global", "chat.deal_detail", "chat.contact_detail"]`) |
| `is_active` | BOOL | |
| `registered_at` | TIMESTAMP | |

### `ai_tool_invocations` (audit log)

| Pole | Typ | Opis |
|---|---|---|
| `id` | UUID PK | |
| `tool_id` | UUID FK | |
| `user_id` | UUID FK | |
| `chat_context` | TEXT | `chat.global` / `chat.deal_detail` etc. |
| `prompt_excerpt` | TEXT | Pierwsze 500 znaków promptu (dla audit) |
| `input` | JSONB | Argumenty z których wywołano tool |
| `confirmation_shown` | BOOL | |
| `confirmation_decision` | ENUM | `accepted | rejected | modified | timeout` |
| `confirmation_diff` | JSONB | Snapshot diffu pokazanego userowi |
| `output` | JSONB | Wynik (NULL jeśli rejected) |
| `error` | TEXT | Jeśli był error |
| `duration_ms` | INT | Czas wykonania (po confirm) |
| `model_name` | TEXT | Który LLM zainicjował (np. `llama-3.3-70b-local`) |
| `invoked_at` | TIMESTAMP | |

Dla `audit_level=summary` zapisujemy tylko meta. Dla `full` zapisujemy input/output (uwaga RODO — szyfrowanie at-rest).

### `ai_chat_sessions`

| Pole | Typ | Opis |
|---|---|---|
| `id` | UUID PK | |
| `user_id` | UUID FK | |
| `context` | JSONB | `{kind: "deal_detail", deal_id: "..."}` — sets which tools available |
| `messages` | JSONB | Historia rozmowy |
| `model` | TEXT | |
| `created_at`, `last_message_at` | | |

## Workflow wywołania toola

```
1. LLM (z chatu sidebar w jakimś addonie) generuje tool_call:
   {tool: "crm.create_deal", args: {...}}

2. Broker:
   a. Wczytuje rejestr toola
   b. Walidacja input_schema (jeśli failed → zwraca błąd LLMowi, retry)
   c. Sprawdza grants:
      - user ma wszystkie required_grants?
      - addon-instance ma grant na wywołanie (jeśli cross-addon — np. crm/main wywołuje billing.attach_cost — wymaga inter-addon grant)
   d. Sprawdza rate limit
   e. Sprawdza `available_in_contexts` — czy ten tool jest dozwolony w bieżącym chat context

3. Jeśli risk=read (lub one_click): wywołaj akcję natychmiast.
   Jeśli risk=draft: zwróć wynik (treść drafta), LLM go pokazuje, user klika "Wyślij" → osobny tool call act-class.
   Jeśli risk=act + confirmation=required:
      a. Wywołaj akcję w "dry-run mode" w addonie (zwraca planowany diff bez commitu)
      b. Pokaż user confirm dialog z diffem
      c. User klika "Wykonaj" → faktyczny call z `confirmed: true`
      d. User klika "Zmień parametry" → wraca edycja inputu, retry
      e. User klika "Odrzuć" → zwraca LLMowi błąd "user rejected"

4. Akcja wykonana → wynik wraca do LLM → LLM kontynuuje rozmowę
   (np. „Utworzyłem deal X. Czy chcesz też dodać task follow-up za 3 dni?")

5. Wszystko (request + decision + diff + output) zapisuje się do `ai_tool_invocations`.
```

## UI surfaces

### C11 — AI chat sidebar (główny use case)

**Kontekst:** chat jest zawsze w kontekście jakiegoś addona lub globalny. Kontekst określa pulę dostępnych tooli:

| Kontekst chatu | Dostępne tooly |
|---|---|
| `chat.global` (ekran startowy) | read tools + bezpieczne search + create_* |
| `chat.deal_detail` (C11 obecnie) | + wszystkie crm.* + billing.* (jeśli grant) + activity.* + email.draft_* |
| `chat.contact_detail` | + contacts.* + crm.search_* + email.* |
| `chat.company_detail` | + contacts.* + crm.aggregate_* |
| `chat.inbox` | + activity.* + crm.update_* |

**Akcje użytkownika:**

1. **Wpisanie wiadomości** → wysyła do LLM z system prompt + lista dostępnych tooli + kontekst (deal_id / contact_id z URL)
2. **LLM proponuje tool call (risk=act)** → Broker wyświetla confirm dialog z diffem
   - Diff pokazuje: zasób, pola które się zmienią (jeśli update) lub które będą stworzone (jeśli create)
   - Przycisk "Wykonaj" / "Zmień parametry" / "Odrzuć"
3. **"Zmień parametry"** → user może ręcznie edytować input (formularz wygenerowany z input_schema) i retry
4. **Drafty** (risk=draft) — pokazane inline w czacie z "Wyślij" / "Edytuj draft" / "Pomiń"
5. **`/tools` w czacie** → otwiera popup z listą dostępnych tooli w bieżącym kontekście (dla advanced users)
6. **Microphone** → speech-to-text dla tych co wolą gadać

### P3 — AI Tools registry (admin)

Już ma mockup. Tabela tooli + edycja per tool:
- Toggle is_active
- Edit `required_grants` (advanced)
- Edit `rate_limit`
- Edit `available_in_contexts`
- Edit `audit_level`
- Test invoke (sandbox)
- Statystyki wywołań

### Audit Log view (osobny ekran dla admina)

Tabela `ai_tool_invocations` z filtrami: user, tool, kontekst, czas, decyzja. Klik wiersza → modal z pełnym diffem + outputem.

## Provided host fn

**Dla addonów rejestrujących tooly (automatycznie z manifestu):**
- `ai_broker.register_tool(definition)` — wywoływane przez host przy load addona
- `ai_broker.unregister_tool(tool_id)` — soft-delete

**Dla chat UI (każdy addon może mieć własny chat sidebar):**
- `ai_broker.start_session(user_id, context) → SessionId` — startuje session
- `ai_broker.send_message(session_id, text) → list<Message>` — wysyła do LLM, dostaje odpowiedzi (z embedded tool_calls)
- `ai_broker.invoke_tool(session_id, tool_call, confirmed: bool) → InvocationResult` — pośrednik do call
- `ai_broker.list_available_tools(session_id) → list<ToolDef>` — co LLM ma do dyspozycji w tej sesji

**Dla admina:**
- `ai_broker.list_invocations(filter, range) → list<Invocation>`
- `ai_broker.explain_decision(invocation_id) → ExplainResult` — dla audytu („dlaczego ten tool był dostępny dla tego usera w tym czasie")

## Confirm dialog — wymagania

**Standardowy komponent `tf-ai-confirm-dialog` (rendered przez shell):**

```
┌─ Confirm — mutacja danych ─────────────────────┐
│ Tool: crm.create_deal                          │
│ Risk: mutating · Grants: crm.deal.write,       │
│       contacts.read_basic                       │
│ ─────────────────────────────────────────       │
│ Nazwa:     mBank · CBA migracja                 │
│ Firma:     + mBank S.A. (znaleziony Contacts)   │
│ Decydent:  + Magda Wiśniewska (CIO @ mBank)    │
│ Wartość:   120 000 PLN                          │
│ Faza:      Oferta (z „budżet zatwierdzony")     │
│ Commit:    + tak                                │
│ Est close: 31.05.2026                           │
│ Owner:     Anna Kowalska (Ty)                   │
│ ─────────────────────────────────────────       │
│ [Wykonaj]  [Zmień parametry]  [Odrzuć]          │
└─────────────────────────────────────────────────┘
```

Wszystkie zmiany w bazie pokazane jako diff: `+` (add), `-` (remove), `→` (change from A to B).

## Tryby AI

| Tryb | Co | Confirmation |
|---|---|---|
| `suggest` | rekomendacja w UI (inline w inboxie, np. „zadzwoń do X bo 14 dni cisza") | nie ma — to tylko sygnał |
| `draft` | przygotowuje treść do akceptacji (draft maila, draft notatki) | 1 click — user klika „Wyślij" lub „Zapisz" |
| `act` | mutuje dane (create/update/delete) | required — confirm dialog z diffem |

## Permissions (kto co może)

- **Każdy user** może otworzyć chat AI i wywoływać read tools
- **Draft/act tools** wymagają standardowych grants (z [02 permissions](./02-platform-permissions.md))
- **Admin** widzi pełny audit log, może wyłączyć tooly globalnie, zmienić rate limity
- **`system.security_auditor`** rola — read-only dostęp do audit log

## Migracja z IntrApp

IntrApp **nie miał AI**. Tu wszystko nowe. Migrujemy tylko **dane wejściowe** (kontakty, deale, faktury) — AI dostaje je przez kontrakty resource providerów, nie bezpośrednio.

## Implementation order

1. **Schema:** `ai_tools`, `ai_tool_invocations`, `ai_chat_sessions`.
2. **Manifest parsing:** dodatkowa sekcja `[provides.ai_tools]` w manifeście, walidator, auto-register w `ai_tools`.
3. **Host fn:** `ai_broker.register_tool`, `ai_broker.list_available_tools`.
4. **LLM integration:** moduł komunikujący się z lokalnym llama.cpp (zaczynamy lokalnie). Format tool_calls zgodny z OpenAI.
5. **Chat session management:** `start_session`, `send_message` (z system prompt zawierającym dostępne tooly).
6. **Tool invocation pipeline:** walidacja schema → grants check → rate limit → routing do addona.
7. **Confirm dialog component:** `tf-ai-confirm-dialog` w www. Wymaga dry-run mode w addonach (każda akcja musi umieć zwrócić planowany diff bez commitu).
8. **Chat sidebar UI** (C11) w shell — dostępne we wszystkich addonach.
9. **Audit log + UI** (admin).
10. **Smart context detection** — chat automatycznie wykrywa current URL (deal_detail/contact_detail) i ustawia kontekst.
11. **Speech-to-text** integration (opcja).
12. **External LLM routing** (przyszłość — gdy lokalny model nie wystarczy) z encryption.

## Specyfikacje wybranych tooli (zarys, pełne w planach addonów)

| Tool | Risk | Confirmation | Description |
|---|---|---|---|
| `crm.create_deal` | act | required | Tworzy deal z parametrów |
| `crm.update_deal_from_context` | act | required | Aktualizuje deal na podstawie ostatniego maila/spotkania |
| `crm.move_stage` | act | required | Zmienia stage'a deala |
| `crm.read_deal` | read | none | Wczytuje detal deala |
| `crm.search_deals` | read | none | Wyszukuje deale |
| `crm.forecast_deal` | draft | none | Prawdopodobieństwo close (jawne reguły, nie ML) |
| `email.draft_followup` | draft | 1_click | Draft odpowiedzi z kontekstem deala |
| `email.extract_to_crm` | draft | 1_click | Wyciąga deal/kontakt z treści maila |
| `activity.create_task` | act | required | Dodaje task |
| `billing.attach_cost_to_deal` | act | required | Refakturuje koszt na deal |
| `contacts.find_or_create` | draft | 1_click | Znajduje kontakt lub proponuje stworzenie |
| `calendar.propose_meeting` | draft | 1_click | 3 sloty dopasowane do kalendarza |

## Otwarte decyzje

1. **Który LLM jako default?** Lokalny llama-3.3-70b (fast, prywatność) vs zewnętrzny (lepsze rozumowanie). Rekomendacja: **lokalny dla read/search + extract, zewnętrzny gdy konfiguracja na to pozwala dla draft/act (lepsze drafty)**. Admin może wymusić tylko lokalny.

2. **Kontekst chatu — automatyczny czy ręczny?** Rekomendacja: **automatyczny z URL** (deal_detail gdy na `/crm/deals/X`, global poza addonami). Plus manualny override przez `/context global` w polu wprowadzania.

3. **Multi-step tool chains** — gdy LLM wywoła 3 tooly z rzędu, czy każdy ma osobny confirm dialog? Rekomendacja: **tak, każdy confirm osobno**. User może zaznaczyć "auto-accept dla tej sesji dla read tools" ale nigdy dla act.

4. **Tool description quality** — LLM jest tak dobry jak description tool'a. Rekomendacja: **standaryzacja stylu**: czasownik + co robi + przykład: "Tworzy deal. Przykład: 'załóż deal dla mBank, 50k, koniec maja'."

5. **Latency budget** — confirm dialog dodaje ~3-5s user time. Czy dla read tools (które mogą być wywoływane masowo) jest sposób żeby LLM widział wynik szybko? Rekomendacja: **read tools zawsze bez confirm** + cache wyników w sesji (LLM woła `crm.read_deal(X)` raz, kolejne razy z cache).

6. **Drafty wielokrotne** — jeśli LLM wygeneruje 3 drafty maili w jednym call, czy 3 confirmy? Rekomendacja: **jeden batch confirm z wyborem checkbox „wyślij tylko #1 i #3"**.

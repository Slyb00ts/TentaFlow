# SPEC — Agenci: przeprojektowanie (2026-08)

Mockupy: `mockups/agenci-20260822/`. Analiza stanu obecnego + decyzje projektowe.
Każdy ekran to samodzielny HTML; kontrakt budowy jak w `mockups/projekty-20260723/shared/BUILD_CONTRACT.md`
(wspólne tokeny w `shared/styles.css`, style modułu w `shared/agenci.css`).

## Diagnoza obecnego stanu (tentaflow-core)

### UI (`www/js/modules/agents.js`)
1. **Wejście w agenta = playground na żywo.** Klik w kartę wywołuje `openPlayground()` — czat +
   oś przebiegu (`tf-agent-activity`) jako główny widok. Konfiguracja agenta jest schowana w
   modalnym kreatorze 3-krokowym (`openWizard`), używanym i do tworzenia, i do edycji.
2. **Nagłówek szczegółów jest customowy** (`.agents-pg-header`: „wstecz" + awatar + tytuł), poza
   kanonicznym szkieletem aplikacji. Wzorzec referencyjny: szczegóły addonu (`addons.js`) —
   `.breadcrumb` → detail-header → `tf-tabs underline` z ikonami i licznikami.
3. Górne zakładki modułu (Agenci/Przebiegi) i karty są w porządku — zostają, z drobnym porządkiem.

### Backend — dlaczego addonów nie widać w narzędziach
- `install_bundled_addons` (`addon/bundled.rs`) to **reconcile wyłącznie katalogu pakietów** —
  bundled addon (deep-research!) nie tworzy instancji. Narzędzia trafiają do rejestru
  (`registered_tools`) dopiero przy instalacji/aktualizacji instancji
  (`register_addon_runtime`, `addon/mod.rs`).
- `build_tools_catalog` (`dispatch/handlers.rs`) grupuje wyłącznie po `AddonManager::list_tools()`
  (= rejestr w pamięci). **Pakiet bez instancji nie istnieje dla pickera.**
- **Brak przejścia startowego** rejestrującego narzędzia już zainstalowanych instancji:
  `start_addon` nie woła `register_addon_runtime`, więc po restarcie Core katalog narzędzi jest
  pusty aż do reinstallu/aktualizacji/sync-reconcile. To luka do naprawienia w implementacji
  (rejestracja z manifestów przy boocie / czytanie deklaracji z katalogu pakietów).

## Decyzje projektowe

| # | Decyzja | Ekran |
|---|---------|-------|
| D1 | Szczegóły agenta = konfiguracja do edycji inline (opis, prompt, model, pętla/limity, flagi) ze sticky paskiem niezapisanych zmian. Kreator zostaje TYLKO dla tworzenia. | A02 |
| D2 | Szkielet szczegółów: `.breadcrumb` → `.detail-header` (awatar, nazwa + chipy statusu, akcje) → `tf-tabs`: Konfiguracja / Narzędzia i umiejętności / Testowanie / Przebiegi. | A02–A05 |
| D3 | Playground (czat sandbox + oś na żywo + ask_user/permission) = zakładka „Testowanie". | A04 |
| D4 | Przebiegi: zakładka wewnątrz agenta (per agent) + przekrój globalny jako druga górna zakładka modułu. | A05, A06 |
| D5 | Katalog narzędzi pokazuje: (a) instancje addonów z grupami i wildcard `addon.*`, (b) **pakiety z katalogu bez instancji — widoczne, wyszarzone, z CTA „Zainstaluj z katalogu addonów"**, (c) `core.*` pogrupowane semantycznie (współpraca / projekt / kod / umiejętności), (d) umiejętności po tagach i nazwach. | A03 |
| D6 | Prompt systemowy: monospace editor, licznik znaków/tokenów, zmienne `{{var}}`, „Popraw z AI" (asystent A08). | A02 |
| D7 | Asystent AI proponuje szkic widząc ten sam katalog narzędzi co edytor i ostrzega, gdy proponowane narzędzie nie ma jeszcze instancji. | A08 |

## Mapowanie na dane (tabela `agents` / wire `AgentsPayload`)

- Konfiguracja (A02): `name`, `display_name`, `description`, `system_prompt`, `model`,
  `params.{temperature,reasoning_effort}`, `max_iterations`, `timeout_secs`, `max_subagents`,
  `max_spawn_depth`, `on_child_complete`, `routable`, `is_enabled`, `flow_id` — wszystkie pola już
  istnieją; zmiana to **przeniesienie edycji z okna do zakładki**, bez zmian protokołu.
- Narzędzia (A03): `tools_json` (wpisy `addon.tool` / `addon.*` / `core.*`) + `skills_json`.
- Przebiegi (A05/A06): `agent_runs` (status, prompt, result, exit_reason, iterations, total_tokens,
  run_log) + sub-runs po `parent_run_id`.
- Wymagana praca backendowa (jedyna): katalog narzędzi musi uwzględniać **deklaracje z katalogu
  pakietów** (manifesty) i rejestrować narzędzia instancji przy boocie — bez tego D5 nie zadziała.

## Co mockupy świadomie zmieniają względem `projekty-20260723/a01–a05`

- A02 (kreator) i A03 (playground) ze starego zestawu przestały być głównym widokiem —
  kreator jest tylko wejściem w tworzenie, playground jest zakładką.
- Nowy ekran: zakładka Narzędzia i umiejętności z pełnym katalogiem (w tym pakietów
  niezainstalowanych) — odpowiedź na „dlaczego nie widzę deep-research w tools".
- Nowy ekran: zakładka Przebiegi per agent (stary moduł miał tylko globalną listę przebiegów).

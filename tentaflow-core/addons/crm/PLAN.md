# CRM dla TentaFlow — plan produktowo-architektoniczny

Plan startowy nowego CRMa dla TentaFlow. Powstał po reverse-engineeringu starego IntrAppa (patrz [LIFECYCLE.md](./LIFECYCLE.md)) i konsultacji z codex (sesja `019e4036-e3c7-73f2-8a27-2ea4b302d799`).

## Tezy produktowe (filozofia)

**Największe ryzyko nie jest techniczne. Największe ryzyko to zbudowanie pięknego IntrAppa — starego procesu w nowej skórze.** Handlowcy nie nienawidzą CRMa dlatego, że brakuje mu gradientów. Nienawidzą go, gdy jest systemem raportowania dla szefa, a nie narzędziem do wygrywania dealów.

Trzy osie, na których budujemy:

1. **Daily Sales Inbox** — główny ekran handlowca to lista "co dzisiaj zrobić", nie lista projektów ani kanban.
2. **Resource / Contribution Platform** — kręgosłup do komunikacji między addonami (deklaratywny manifest, nie ad-hoc API).
3. **AI Action Broker** — AI jako cichy pracownik administracyjny, nie chatbot. Tryby `suggest` / `draft` / `act`.

WOW factor wynika z trafności i szybkości, nie z animacji ani spatial canvasa. CRM ma być **szybki, żywy i przewidujący następny ruch**.

### Co działa (skopiować)

- CRM jako inbox pracy, nie baza danych. Pierwszy ekran: "dziś 7 rzeczy do zrobienia", nie "lista projektów".
- Automatyczne przechwytywanie aktywności (mail, spotkanie, plik, e-dokument) — ręczne logowanie zabija adopcję.
- Jedno kliknięcie do następnej akcji po każdym evencie ("utwórz follow-up", "ustaw commit", "wyślij draft").
- Natychmiastowa nagroda za uzupełnienie danych (lepsza prognoza, alert o ryzyku, draft maila).
- Prywatność operacyjna handlowca — jeżeli CRM wygląda jak monitoring, ludzie kłamią w danych.
- Globalny command palette `⌘K` (Linear-style): tworzy projekt, znajduje kontakt, zmienia status, odpala akcję AI.
- Unified timeline na kontakcie/projekcie (Attio-style): maile + spotkania + notatki + pliki + koszty + faktury + zmiany statusu.
- Inline edit wszędzie. Bez przechodzenia w "tryb edycji".
- Smart inbox jako kolejka zdarzeń wymagających decyzji ("klient odpisał", "oferta bez odpowiedzi 9 dni", "koszt czeka na refakturę").
- Microinteractions pokazujące konsekwencje (przesunięcie deala → widoczna zmiana forecastu).

### Czego nie robić

- Obowiązkowych pól bez sensu.
- Kanbana jako głównej odpowiedzi na wszystko (dobry do pipeline'u, zły jako centrum dnia pracy).
- Spatial canvasa jako głównego CRMa (tylko opcjonalnie do mapy relacji konta / grupy kapitałowej).
- Chatbota AI obok starego CRMa. AI ma działać, nie gadać.
- Deal scoringu jako czarnej skrzynki (na małych datasetach zgaduje — lepsze są jawne reguły + sygnały).
- AI forecastu bez dyscypliny danych (model elegancko skłamie).
- Nadmiar animacji. CRM ma być szybki, nie cyrkowy.

### Język UX

UI mówi językiem sprzedaży: **Lead / Oferta / Realizacja / Zamknięcie**. Wewnętrznie typ encji może nazywać się `sales_project`, ale **w UI nigdy nie pojawia się słowo "projekt"** dla rekordów przed wygraną — to są **deale / oferty / szanse**. "Projekt" pojawia się dopiero w fazie Realizacja.

## Dekompozycja na addony

| Addon | Domena | Rola |
|---|---|---|
| `contacts` | firmy, osoby, relacje, grupy kapitałowe, role kontaktów | rdzeń — centralna baza encji ludzkich/firmowych |
| `crm` | deal/oferta/realizacja/zamknięcie, forecast, commit, marża, budżet, akceptacje | rdzeń sprzedażowy (monolit domenowy) |
| `activity` | zadania, przypomnienia, timeline events, next action | wspólna warstwa aktywności |
| `email` | wątki + threading + connector IMAP (opcj.) | nie pełny klient mailowy — connector |
| `calendar` | sync spotkań | nie osobna aplikacja CRM — connector |
| `documents` | pliki, załączniki, OCR/metadane | dokument = plik + metadane |
| `billing` | faktury, koszty, refaktury, e-dokumenty kosztowe, integracje księgowe | **osobny addon** — finanse mają inny lifecycle, statusy, odpowiedzialność |

### Decyzje uzasadniające

- **`crm` jako monolit domenowy** (nie mikroaddony per faza). Lifecycle Lead→Realizacja→Zamknięcie żyje w jednym addonie. Rozbijanie po ekranach to byłaby pomyłka — rozbijamy po odpowiedzialności danych.
- **Faktury NIE w `documents`** — dokument to plik + metadane; faktura/koszt/refaktura to **zdarzenie finansowe** z własnym lifecycle. Wymagałoby to docelowo integracji księgowych, KSeF, e-Faktury — niezależnych od plików.
- **`activity` osobno od `crm`** — kalendarz, maile i tasks też publikują events na timeline. Wspólna warstwa, nie po jednej kopii w każdym addonie.
- **`email`/`calendar` jako "connector"** — nie chcemy budować klienta poczty ani kalendarza. Chcemy syncować threadem/eventami z istniejących źródeł (IMAP/Exchange/Google).

### Co wnosi `crm` (zakres)

Encje:
- `deal` (przed wygraną — Lead/Oferta) ← bazuje na `ProjectAttributes` z IntrAppa
- `sales_project` (po wygranej — Realizacja/Zamknięcie) — ta sama tabela, faza statusu, ale **inny język UI**
- `pipeline_stage` (katalog faz pipeline'u)
- `forecast_snapshot` (zatrzaśnięte prognozy w czasie)
- `acceptance_card` (karta akceptacji budżetu)
- `responsible_person` (handlowcy / odpowiedzialni — referuje `contacts`)

Brak w `crm` (deleguje do innych addonów):
- kontakty → `contacts`
- pliki i karta akceptacji jako PDF → `documents`
- koszty z e-dokumentów, faktury sprzedaży → `billing`
- spotkania → `calendar`
- zadania i przypomnienia → `activity`
- maile → `email`

## Kontrakt między addonami — Resource Provider + View Contributions

Najważniejsza decyzja architektoniczna. Nie GraphQL, nie gRPC, nie Notion-blocks, nie Salesforce-platform.

**Własny kontrakt deklaratywny w manifeście addona, egzekwowany przez host.** Pattern: każdy addon **deklaruje** co ma, host trzyma rejestr, inne addony pytają rejestr (nie addon bezpośrednio).

### Co addon deklaruje w manifeście

```toml
# tentaflow-core/addons/crm/manifest.toml (szkielet)

[provides]
resources = ["sales_project", "deal", "pipeline_stage", "forecast_snapshot"]

[[provides.queries]]
name = "crm.projects_for_contact"
input  = { contact_id = "uuid" }
output = { items = "list<SalesProjectSummary>" }

[[provides.queries]]
name = "crm.deals_for_company"
input  = { company_id = "uuid" }
output = { items = "list<DealSummary>" }

[[provides.actions]]
name = "crm.create_deal"
input_schema = "schemas/create_deal.json"
risk = "mutating"
confirmation = "required"
required_grants = ["crm.deal.write", "contacts.read_basic"]

[[provides.views]]
slot = "contact.detail.sidebar"
title = "Projekty / Deale"
query = "crm.deals_for_contact"
render = "list"             # shell renderuje przez tf-*, addon nie zwraca HTML
empty_state = "Brak dealów"

[[provides.views]]
slot = "company.detail.sidebar"
title = "Pipeline"
query = "crm.deals_for_company"
render = "pipeline_mini"

[[provides.events]]
name = "sales_project.created"
name = "deal.stage_changed"
name = "deal.commit_changed"
name = "deal.acceptance_card_decided"

[[provides.ai_tools]]
name = "crm.create_deal"
description = "Tworzy nowego deala dla wskazanej firmy/kontaktu."
schema = "schemas/ai/create_deal.json"
required_grants = ["crm.deal.write", "contacts.read_basic"]
confirmation = "required"

[needs]
# co my potrzebujemy od innych
contacts = ["read_basic", "read_relations"]
documents = ["read_metadata", "attach_to_resource"]
billing = ["read_costs_for_project", "subscribe_invoice_events"]
activity = ["create_task", "create_reminder", "read_timeline"]
```

### Zasady kontraktu

1. **Addon nie odpytuje innego addona bezpośrednio.** Pyta host. Host pyta rejestr. Rejestr wskazuje providera. Provider odpowiada.
2. **Sidebar contribution = view model, nie HTML/JS.** Addon zwraca strukturę (tytuł, badge, lista pozycji, akcje, empty state, skeleton, linki). Shell renderuje przez komponenty `tf-*`. Tylko pełne aplikacje addonów (główne widoki menu) mają wolność wizualną.
3. **Dane do sidebara idą przez cache hosta (`MaterializedSummary`).** Instant render wymaga snapshotu (ostatnie projekty, liczniki, last activity). Addon publikuje events, host utrzymuje mały materialized index. Sidebar nigdy nie czeka na addon — pokazuje cache + revalidate w tle.
4. **Każda relacja ma jawny typ.** `contact -> sales_project`, `company -> invoice`, `sales_project -> document`. Nie luźny JSON bez semantyki — AI i dashboardy zginą w chaosie.
5. **Grants per instance i per capability.** Nie "CRM ma dostęp do contacts". Tylko: instancja `crm/main` ma `contacts.read_basic`, `contacts.read_relations`, `contacts.write_activity` itd. Reuse mechaniki z istniejących `uses_alias` w TentaFlow.
6. **Wersjonowanie kontraktów bez compat-shimów w kodzie.** Manifest deklaruje wersję, host wymaga zgodności, migracje danych mają osobny mechanizm.

### Minimalny zestaw kontraktów platformowych (host fn)

| Kontrakt | Po co |
|---|---|
| `ResourceDescriptor` | typ, id, display name, owner addon |
| `RelationProvider` | zwraca relacje dla zasobu |
| `PanelContribution` | deklaruje gdzie addon umie się wyrenderować |
| `ActionProvider` | deklaruje akcje użytkownika |
| `EventPublisher` | publikuje zmiany danych |
| `SearchProvider` | globalne wyszukiwanie (⌘K) |
| `PermissionGrant` | kto może co czytać/pisać |
| `MaterializedSummary` | mały cache do natychmiastowego UI |

To jest ważniejsza decyzja niż wygląd CRMa. Jeśli zrobimy ją źle, każdy addon będzie znał każdy inny i platforma skończy jako zlepione monolity.

## AI Action Broker

AI tools korzystają z **tego samego fundamentu** co Resource/View contributions (manifest, grants, registry, audit), ale to **osobny kontrakt intencyjny**.

### Reguły

- **Globalna pula narzędzi, dynamicznie zawężana kontekstem.** Chat na ekranie kontaktu dostaje narzędzia kontaktowe + CRMowe + activity + email. Chat globalny tylko bezpieczne search/read + wybrane akcje.
- **Grant per addon instance i per user.** Admin dopuszcza CRMowi `contacts.read_basic`, użytkownik nadal musi mieć prawo do konkretnego kontaktu (per-row ACL).
- **LLM nie woła host fn addonów bezpośrednio.** Woła **Broker** w platformie. Broker autoryzuje, loguje, prezentuje confirm dialog, dopiero potem wykonuje.
- **Mutacje zawsze przez confirm dialog z diffem.** "Utworzę projekt X, firma Y, wartość 50k, etap Oferta, owner Jan, follow-up 25.05" → user klika OK.
- **Read tools mogą działać automatycznie. Write tools nigdy.**
- **Każde wywołanie tool'a → audit log** (kto, kiedy, prompt/context, argumenty, wynik).
- **Schemy narzędzi małe i stabilne.** Nie podajemy modelowi całej bazy CRM. Jedno narzędzie = jedna akcja.

### Tryby AI

| Tryb | Co robi | Konfirmacja |
|---|---|---|
| `suggest` | rekomendacja w UI ("zadzwoń do X, nic się nie dzieje 14 dni") | nie |
| `draft` | przygotowuje treść do akceptacji (draft maila, draft notatki) | jedno kliknięcie |
| `act` | woła tool, mutuje dane | confirm dialog z diffem |

### Use case'y AI które działają (potwierdzone w branży)

1. **Ekstrakcja z maili/notatek do struktury** — "klient chce ofertę na X, budżet 50k, decydent Anna" → propozycja aktualizacji deala.
2. **Tworzenie obiektów z języka naturalnego** — "załóż deal dla mBank, 50k, koniec kwartału" → confirm → zapis.
3. **Drafty follow-upów z kontekstem** — system zna deal, ostatnie ustalenia, ofertę, osoby, termin. Draft gotowy, nie "napisz profesjonalnego maila".
4. **Smart reminders na regułach + sygnałach** — "oferta wysłana 14 dni, brak odpowiedzi, commit, wartość 120k".
5. **Deal hygiene assistant** — "commit bez następnej aktywności", "wartość zmieniona, marża pusta", "koszt zaakceptowany, brak refaktury".
6. **Live meeting assistant** (jeśli mamy transkrypcję) — pytania klienta, produkty, obiekcje, next steps.

### Use case'y przereklamowane

1. **Deal scoring jako czarna skrzynka** — na małych datasetach zgaduje. Lepsze: jawne reguły + widoczne sygnały (brak aktywności, etap, wiek, decydent, oferta, konkurencja, historia).
2. **Chatbot do raportów** — zarząd i tak chce dashboard. Chat to dodatek, nie główny interfejs.
3. **AI forecast bez dyscypliny danych** — śmieciowe statusy → eleganckie kłamstwa.

## Dashboardy hierarchiczne — model mieszany

**Preset roli nie wystarczy. Pełna adaptacyjność od pierwszej wersji to też błąd.** Robimy model trójwarstwowy:

1. **Rola daje bazowy dashboard** (preset).
2. **Użytkownik może go układać z widgetów** (drag-drop, dodaj/usuń, zmień zapytanie).
3. **System dodaje sekcję adaptacyjną "Wymaga uwagi"** (sygnały bez konfiguracji — to robi AI).

**Jeden silnik widgetów dla wszystkich ról.** Różne query, różne domyślne układy. Nie trzy osobne ekrany — w pół roku stałyby się trzema produktami do utrzymania.

### Co widzą poszczególne role

**Zarząd** (prawda o pieniądzach):
- pipeline PLN ważony i nieważony
- commit vs best case
- marża
- forecast końca miesiąca / kwartału
- ranking sekcji
- projekty bez właściciela / aktywności
- odchylenie od celu
- refakturowanie kosztów i ryzyko marży

**Dyrektor** (diagnoza):
- które deale stoją
- kto ma pusty pipeline
- gdzie commit jest fikcyjny
- które oferty bez follow-upu
- które koszty nieprefakturowane
- gdzie wejść jako manager

**Handlowiec** (dzisiejsza robota — Daily Sales Inbox):
- moje następne akcje
- moje deale commit
- klienci bez kontaktu
- oferty do ponowienia
- projekty z ryzykiem
- mój cel i luka do celu

## Mockupy do zaprojektowania (kolejność)

Inspiracja stylistyczna: `~/.gstack/projects/Slyb00ts-TentaFlow/designs/tentavision-v1/` — paleta indigo/violet, Manrope, komponenty `tf-*`, ekrany 1440 × 820. **Menu lewe ma działać jak w aplikacjach** (addon-as-app), nie jak fixed sidebar TentaVision.

Priorytet kolejności (waga = wpływ na adopcję):

| # | Mockup | Kto | Waga |
|---|---|---|---|
| M1 | **Daily Sales Inbox** | handlowiec | krytyczna |
| M2 | **Deal detail** (timeline + inline edit + sidebar contributions) | handlowiec | krytyczna |
| M3 | **Pipeline kanban** (drugorzędny widok dealów) | handlowiec | wysoka |
| M4 | **Command palette `⌘K`** (overlay) | wszyscy | wysoka |
| M5 | **Smart inbox / Wymaga uwagi** (kolejka decyzji) | handlowiec | krytyczna |
| M6 | **Contact detail** z sekcjami contribution (projekty, faktury, eventy, maile) | wszyscy | krytyczna — pokazuje pattern |
| M7 | **Dashboard handlowca** (preset + edycja widgetów) | handlowiec | wysoka |
| M8 | **Dashboard dyrektora** (ten sam silnik, inne query) | dyrektor | wysoka |
| M9 | **Dashboard zarządu** (forecast, marża, ranking) | zarząd | średnia |
| M10 | **Forecast workbench** (commit vs best case, snapshoty) | zarząd / dyrektor | średnia |
| M11 | **Acceptance card workflow** (PDF, mail z linkami, status) | handlowiec / approver | wysoka |
| M12 | **Cost refactoring** (e-dokument → projekty → akceptacja) | handlowiec / dyrektor | wysoka |
| M13 | **AI chat sidebar** (kontekstowy, z toolami) + **confirm dialog z diffem** | wszyscy | wysoka |
| M14 | **Globalne wyszukiwanie / SearchProvider results** | wszyscy | średnia |
| M15 | **Settings — Permissions / Grants między addonami** | admin | średnia |
| M16 | **Settings — AI Tools registry + grants** | admin | średnia |

Każdy ekran 1440×820, ciemna paleta, `tf-*`. Mockupy powstają **przed** kodem.

## Migracja pól z IntrAppa

Pełna mapa pól wymagających zachowania: [LIFECYCLE.md](./LIFECYCLE.md). Najważniejsze "rzeczy które były" i muszą zostać:

| Z IntrAppa | Gdzie w nowym CRMie |
|---|---|
| Lifecycle Lead/Oferta/Realizacja/Zamknięcie | `deal.stage` (enum, koniec magic 100) |
| Commit (zatwierdzenie budżetu) | `deal.commit` (boolean) → wpływa na forecast |
| Wartość, marża, waluta, kurs | pola `deal` + `forecast_snapshot` |
| Workflow karty akceptacji budżetu | `acceptance_card` (encja z PDFem w `documents`, statusem w `crm`) |
| Mocodawca + klient końcowy | relacje do `contacts` (z typem `principal` / `end_client`) |
| Refakturowanie kosztów (`document_cost_projects`) | kontrakt `billing.attach_cost_to_project` |
| Numer faktury (`invoice_number`) | `billing.invoice.number` + relacja do dealu |
| Osoby odpowiedzialne (handlowcy) | relacje `crm.responsible_person` → `contacts.person` |
| Sekcja / dyrektor sekcji | relacje do `contacts.team` (nowa encja w `contacts`) |
| Status realizacji | `deal.stage = realization` + `realization_status` (sub-status) |
| Karty: budżetowa, akceptacji, harmonogram | szablony PDF w `documents` + renderer (Typst lub HTML→PDF) |

**Co świadomie wyrzucamy:** `IsAcc`, `OfferChanceOnly`, `ApproverId2`, `OTNStatus`, `RealizationStatus` (zlewa się ze stage), `ExchangeDate`, `Fold`, `Comment` (mamy `full_description`), `IACode`, sztywne subprojekty Presales/Realizacja/Utrzymanie, dziennik `ProductNotes` (mamy `audit_log` w TentaFlow).

## Kolejność implementacji (faza I)

Założenie: budujemy MVP które handlowiec realnie używa, dopiero potem rozszerzamy. Każdy krok kończy się działającym, wdrożalnym kawałkiem.

1. **Platforma — Resource/View Contribution kontrakt + grants** (host fn w `tentaflow-core`). Bez tego nic dalej nie ma sensu.
2. **AI Tool Broker** (host fn). Wspólny mechanizm autoryzacji i confirm dialogu.
3. **Addon `contacts`** (firmy + osoby + relacje). Bez kontaktów CRM nie ma sensu.
4. **Addon `crm` minimalny** — deal (encja + stages), timeline, inline edit, sidebar contribution na contact.
5. **Daily Sales Inbox** (M1) — pierwszy ekran handlowca. To jest test adopcji.
6. **Command palette ⌘K** (M4) — globalna nawigacja + akcje.
7. **AI tools `crm.create_deal`, `crm.update_stage`, `crm.add_note`** + chat-sidebar (M13).
8. **Addon `activity`** — tasks, reminders, timeline events (na bazie kontraktu z punktu 1).
9. **Addon `documents`** + karta akceptacji (M11).
10. **Addon `billing`** — koszty, refakturowanie, faktury sprzedaży (M12).
11. **Dashboardy handlowca / dyrektora / zarządu** (M7-9) — jeden silnik widgetów.
12. **Connector `email`** + auto-ekstraktor z maili (M5 smart inbox dostaje paliwo).
13. **Connector `calendar`** + meeting assistant.
14. **Forecast workbench + snapshoty** (M10).
15. **Settings: permissions & AI tools registry** (M15-16).

## Bezpieczeństwo

- Każdy kontrakt między addonami przez **per-instance grant** (`crm/main` → `contacts.read_basic`), nie globalny "CRM ma dostęp do contacts".
- LLM nigdy nie woła host fn bezpośrednio — przez Broker, który wymusza grants + confirm dla mutacji.
- Audit log na każdą operację AI Tool (kto, prompt/context, argumenty, wynik). Reuse istniejącego `audit_log` w TentaFlow.
- `MaterializedSummary` w sidebar nie omija ACL — host filtruje per user przy odczycie cache.
- Schemy AI tools są małe — modelowi nie podajemy całych struktur danych, tylko narzędzia do konkretnej akcji.
- Mutacje przez AI zawsze pokazują **diff przed zapisem** (co się zmieni / utworzy).

## Otwarte decyzje (do iteracji)

1. **Renderer PDF** dla kart (budżetowa / akceptacji / harmonogram) — Typst, HTML→PDF (chromium), czy własny prosty layout w Rust. Typst kusi precyzją i prostotą, ale wymaga rozwiązania osadzania.
2. **Czy faza Realizacja w `crm` jest jeszcze projektem CRMowym, czy delegujemy do dedykowanego `delivery` addona?** Argumenty są po obu stronach. Tymczasowo: zostaje w `crm`, bo lifecycle jest spójny.
3. **Forecast snapshot policy** — daily / weekly / on-change. Daily wydaje się minimum.
4. **Multi-currency w pipeline** — zarząd chce wartości w PLN, ale dealy bywają w EUR/USD. Trzymamy kursy w `forecast_snapshot` czy konwertujemy on-the-fly?
5. **Kanał akceptacji karty** — mail z 3 linkami jak w IntrAppie, czy przejście do natywnego UI po zalogowaniu (osoba zatwierdzająca musi być userem w TentaFlow)? Pierwszy wariant niższy próg, drugi bezpieczniejszy.
6. **AI provider routing** — kiedy lokalny llama.cpp/MLX, kiedy zewnętrzny (większe modele dla ekstrakcji vs draftów). Polityka per-user / per-org.
7. **Czy `billing` ma własny PDF renderer dla faktur, czy reuse z `documents`?**
8. **Czy `Daily Sales Inbox` to widok w `crm`, czy meta-widok z `activity` agregujący z wielu addonów?** Drugi wariant czystszy, ale wymaga że kontrakt `activity` jest dojrzały od dnia 1.

## Materiał porównawczy (do dalszej analizy)

Konsultacja codex wskazała inspiracje (kierunki, nie kopię 1:1):
- Attio — model obiektowy + relacje + timeline
- Notion blocks — kompozycja UI (ale uważać na koszt: dużo drobnych operacji, trudne query)
- Salesforce AppExchange — pokazuje koszt ciężkiej platformy enterprise (negatywny przykład)
- Pipedrive Marketplace — model integracji
- Linear — command palette, szybkość, inline edit
- Folk — bycie cienkim, mało pól
- Pipedrive — kanban (dobry do pipeline'u, zły jako centrum dnia pracy)

---

*Plik roboczy. Sesja codex: `019e4036-e3c7-73f2-8a27-2ea4b302d799` (do follow-upów: `/codex` w consult).*

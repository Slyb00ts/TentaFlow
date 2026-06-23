# RAG panel — redesign na chat-first workspace

Cel: zamiast pionowego stosu formularzy/tabel w 5 zakładkach — aplikacja: Split
(sidebar baz wiedzy | workspace), czat-first, klikalne cytaty, drag-drop dokumentów,
graf-eksplorator, konflikty. Backend (read/write tools w lib.rs) bez zmian logiki —
przepisujemy WARSTWĘ WIDOKU w `addons/rag/src/ui.rs`.

## Twarde ograniczenia SDK (zweryfikowane w kodzie)
1. `Split` (0x0105) renderuje dwa puste `data-slot-id` — treść każdego panelu wchodzi
   osobnym `SlotContent`. Shell deklaruje sloty `sidebar` + `workspace`.
2. `List`/`Sidebar` NIE robią bogatych item-template (płaski wiersz). Dlatego karty
   kolekcji i dymki czatu budujemy JAWNIE jako drzewa `Card/Stack/Cluster`,
   przepychane całym `SlotContent`.
3. `Sidebar.items` statyczne (nie bind do tablicy) → lista kolekcji jako jawne karty.
4. `handle_ask` blokujący — brak token-streamingu. UX: append user msg → `SP_CHAT_PENDING=1`
   (bąbel „myślę…") → handle_ask → append assistant msg + cytaty → pending=0.
5. Addon NIE czyta stanu panelu hosta. Stan czytany przy budowie fragmentu (historia
   czatu, wybór kolekcji, filtry) żyje też w KV sesyjnym; `SP_*` PANEL to lustro do bindów.

## Shell — send_panel_shell()
Split(Horizontal, primary 22%, min220/max420, resizable, slots: "sidebar","workspace").
3 SlotDecl. Po PanelShell: send_sidebar() + send_workspace(active_tab). Brak wyboru
kolekcji → workspace = EmptyState "Wybierz bazę wiedzy".

## Sidebar — Stack: header "Bazy wiedzy" + "Nowa"; Input search (SP_SIDEBAR_SEARCH,
filter-collections); ScrollContainer z kartami kolekcji (Card klikalny accent gdy aktywny,
name + "{n} dok" + Badge graf; Click→open-collection {id}); inline create gdy SP_CREATE_OPEN=1.

## Workspace header — Cluster: Heading nazwa (SP_SELECTED_COLLECTION_NAME) + Tag doc count
(SP_WS_DOCCOUNT) + Select graf (SP_GRAPH_ENABLED, set-graph-enabled) + Button usuń. Divider.
Pod nim NavTabs (Czat default, Dokumenty, Graf/Konflikty locked gdy graf OFF; panel-navigate).

## Czat — ScrollContainer z dymkami (message_bubble: user=Cluster End, Card Filled accent Primary,
Text; assistant=Cluster Start, Avatar + Card Subtle [Markdown + Cluster chipów cytatów]).
Chip cytatu: "[i] doc §chunk", Click→open-citation {doc_id,chunk_index,msg_id,cite_index}.
Collapsible panel źródła (SP_SOURCE_OPEN/TITLE/TEXT; zwijanie Local Toggle). Pasek wejścia dół:
Textarea (SP_CHAT_INPUT) + Button Wyślij→ask-question (kolekcja z selected_collection()).
EmptyState gdy 0 wiadomości. Historia: KV chat_log:{collection_id} (master) + SP_CHAT_MESSAGES (mirror).

## Dokumenty — nagłówek z nazwą bazy; FileInput drag-drop (ingest-uploaded); Table docs
z kolumną status→Badge (uploaded=przesłany/Neutral, parsing/embedding=Info, ingested=gotowe/Success,
failed=błąd/Critical) + chunk/encje/relacje + usuń.

## Graf — Input encja + Eksploruj; dwa SectionCard (Sąsiedztwo: Table neighbors, row_action Wejdź→
explore-neighbor; Fakty: Table facts). Interim — brak komponentu node-edge.

## Konflikty (gdy graf ON) — Select status + Filtruj; przyciski run-*; karty konfliktów
(Tag typ + Badge status + Collapsible szczegóły + Zatwierdź/Odrzuć).

## Stan SP_* nowe: SIDEBAR_SEARCH, CREATE_OPEN, WS_DOCCOUNT, CHAT_INPUT (zast. CHAT_QUESTION),
CHAT_MESSAGES (zast. CHAT_ANSWER+CITATION_ROWS), CHAT_PENDING, SOURCE_OPEN/TITLE/TEXT,
DOCUMENT_ROWS+status_label/tone. KV: chat_log:{collection_id}. Usuwane: COLLECTION_ROWS,
CHAT_COLLECTION, CHAT_ANSWER, CITATION_ROWS, CHAT_QUESTION, TAB_COLLECTIONS. Default tab=chat.

## Akcje nowe: start-create-collection, cancel-create-collection, filter-collections,
open-citation {doc_id,chunk_index,msg_id,cite_index} (czyta cytat z chat_log, ustawia panel źródła),
suggest-question {text}. Przepisać action_ask (sekwencja P2). Reszta reużywana.

## Fazy
P1: Split shell + sidebar + workspace header + NavTabs + czat-skeleton (dymki, EmptyState, nawigacja).
P2: realny ask-question→chat_messages + chipy cytatów + panel źródła + pending bubble.
P3: dokumenty redesign + status Badge.
P4: graf + konflikty redesign (karty), gating graf ON/OFF.

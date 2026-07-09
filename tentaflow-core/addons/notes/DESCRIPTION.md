# Notatki

Notatki to addon do szybkiego notowania z lokalna analiza tresci. Lista notatek
jak w aplikacji chat po lewej, pelnoekranowy edytor z autozapisem w srodku
i panel "Powiazania" po prawej. Cala tresc zostaje na wlasnej infrastrukturze.

## Zakres pierwszej wersji

- `notes` — notatki z miekkim usuwaniem (`deleted_at`), formatem markdown
  i pochodzeniem tresci (`typed` / `dictated`).
- `note_shares` — zakresy dostepu: prywatna, udostepniona uzytkownikowi,
  grupie albo calej organizacji (odczyt / edycja). Kazda sciezka odczytu
  i zapisu przechodzi przez ten ACL.
- `note_tags` — tagi notatki edytowane w metadanych edytora.
- `entities`, `note_entities`, `note_links`, `graph_outbox`,
  `entity_merge_log` — schemat pod auto-graf powiazan (wykrywanie encji,
  podobienstwo semantyczne, scalanie encji z mozliwoscia cofniecia). Panel
  "Powiazania" czyta te tabele juz teraz; dane pojawia sie po wdrozeniu
  etapu analizy.

## Interfejs

Panel `main`: lista (nowa notatka, szukajka, filtry zakresu Wszystkie / Moje /
Udostepnione mi / Grupa / Organizacja), edytor (tytul, autor, tagi, wybor
udostepniania, tresc, licznik znakow, status autozapisu) oraz panel powiazan
z wykrytymi encjami.

Deklaracje modeli i przestrzeni pod auto-graf (aliasy notes-llm /
notes-embeddings / notes-stt, przestrzenie wektorowe, kolekcja grafowa)
dojda do manifestu razem z etapem analizy tresci.

# Notatki

Notatki to addon do szybkiego notowania z lokalna analiza tresci. Lista notatek
jak w aplikacji chat po lewej, pelnoekranowy edytor z autozapisem w srodku
i panel "Powiazania" po prawej. Cala tresc zostaje na wlasnej infrastrukturze.

## Dane

- `notes` — notatki z miekkim usuwaniem (`deleted_at`), formatem markdown
  i pochodzeniem tresci (`typed` / `dictated`).
- `note_shares` — zakresy dostepu: prywatna, udostepniona uzytkownikowi,
  grupie albo calej organizacji (odczyt / edycja). Kazda sciezka odczytu
  i zapisu przechodzi przez ten ACL.
- `note_tags` — tagi notatki edytowane w metadanych edytora.
- `entities`, `note_entities`, `note_links`, `graph_outbox`,
  `entity_merge_log` — auto-graf powiazan; `analysis_queue`,
  `merge_suggestions`, `note_chunks` — stan pipeline'u analizy.

## Auto-graf powiazan

Po zapisie notatka trafia do `analysis_queue` (debounce 3 s — pisanie nigdy
nie czeka na model). Worker (oportunistycznie po akcjach UI, budzet 1 notatka
na request, albo narzedzie `analyze_pending` z Admin Schedulera, batch 5):

- tnie tresc na chunki (~512 tokenow, overlap) i embeduje je do przestrzeni
  wektorowej `notes` (alias `notes-embeddings`),
- ekstrahuje encje (person / company / project / topic) LLM-em przez alias
  `notes-llm` (wymuszony JSON, parser odporny na proze wokol, grounding
  w tekscie zrodlowym),
- laczy notatki: `similar` (k-NN po chunkach, prog 0.55) i `entity`
  (wspolne encje kanoniczne) w `note_links`,
- materializuje graf `notes_kg` (nody `note:{id}` / `entity:{id}`, krawedzie
  `mentions` i `similar_to` z waga) WYLACZNIE przez idempotentny
  `graph_outbox` (drain `WHERE applied=0`, re-drain po crashu),
- scala duplikaty encji: podobienstwo nazw >= 0.95 przy tym samym typie
  auto-merge (odwracalny przez `entity_merge_log`), pasmo 0.80-0.95 to otwarta
  sugestia w `merge_suggestions` (decyzja uzytkownika: Scal / Odrzuc,
  scalenie mozna cofnac z panelu).

Usuniecie notatki przechodzi przez te sama kolejke jako tombstone: wpisy SQL,
node grafu i wektory znikaja przez outbox.

## Interfejs

Panel `main`, trzy widoki (Notatki / Graf / Szukaj):

- **Notatki** — lista (nowa notatka, szukajka, filtry zakresu Wszystkie /
  Moje / Udostepnione mi / Grupa / Organizacja), edytor (tytul, autor, tagi,
  tresc, licznik znakow, status autozapisu) oraz panel "Powiazania": status
  analizy, karty powiazanych notatek (procent, powod, chip "nowe" dla
  powiazan mlodszych niz 24 h), chipy wykrytych encji, sugestie scalenia
  i mozliwosc cofniecia swiezych scalen. Powiazania i encje sa zawsze
  filtrowane ACL-em czytelnika po stronie notatki docelowej.
- **Graf** — interaktywny graf notatek i encji z filtrami zakresu, typow,
  progu wagi i glebokosci.
- **Szukaj** — wyszukiwanie hybrydowe: embeddingi zapytania (k-NN po chunkach,
  over-fetch) + wedrowka po grafie (widoczne encje -> notatki <= 2 skoki),
  fuzja RRF. KAZDY kandydat przechodzi `acl_read_clause` PRZED rankingiem.
  Nad wynikami streamowana synteza LLM (alias `notes-llm`) z chipami zrodel;
  przy niepodpietych aliasach dziala fallback czysto tekstowy (badge
  "tekstowo", bez karty odpowiedzi). Prawa kolumna: encje w wynikach
  i zawezanie przez graf.

## Udostepnianie

Modal "Udostepnij notatke" (tylko wlasciciel): wyszukiwarka osob i grup
z podpowiedziami na zywo, wpisy per uzytkownik / grupa z poziomem dostepu
Odczyt / Edycja, przelacznik "cala organizacja — tylko odczyt". Zapis jest
transakcyjny (`replace_all_shares`): walidacja podmiotow w katalogu,
kazdy statement re-sprawdza wlasciciela i zywotnosc notatki w WHERE.
Badge zakresu na kartach pokazuje najszerszy zasieg (org > grupa > osoby >
prywatna).

## Bloki flow i narzedzia LLM

`blocks.json` rejestruje bloki `notes.search`, `notes.create`, `notes.append`
i `notes.get_related` (wejscie/wyjscie jako FlowEnvelope; payload Json).
Te same handlery sa dostepne jako narzedzia LLM: `search_notes`,
`create_note`, `append_note`, `get_related_notes`. Kazda sciezka dziala
w kontekscie uzytkownika wykonujacego flow i przechodzi przez istniejace
ACL-e.

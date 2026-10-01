# Projekty — praca zespołu: funkcje, uprawnienia, workflow zadań, tablica, sprinty, wydania, git — plan

> **Rewizja 0.11** (2026-09-29): przypisanie, zmiana osoby i przekazanie — wspólne okno, przekazanie hurtowe, akcje zbiorcze (§4.5).
> **Rewizja 0.10** (2026-09-29): podprojekty — drzewo projektów z widokami zbiorczymi i dziedziczeniem ustawień (§1a).
> **Rewizja 0.9** (2026-09-29, po przeglądzie krytyka: wersjonowanie workflow, zależności faz między planami, bezpieczeństwo agentów, ugruntowanie changelogu, eksploatacja załączników). Kolejne rewizje dołożyły: typy zadań i SLA (0.2–0.3), moduły,
> roadmapę i changelog (0.4–0.5), zasoby i plan z dokumentów (0.6), roadmapę planową i realizacyjną
> z zależnościami (0.7), ewidencję czasu i powiązanie ze strukturą organizacyjną
> (`ORG_STRUCTURE_PLAN.md`) (0.8).
>
> Rozbudowa **Projektów** (`project_studio/`) do lekkiego narzędzia
> w stylu Jiry, przeznaczonego dla zespołu wytwarzającego oprogramowanie. Moduł bezpieczeństwa
> (`PROJECT_STUDIO_SECURITY_PLAN.md`) stoi **na tym planie**: używa tych samych funkcji zespołu,
> workflow, wydań i integracji git.
>
> Zasada doboru zakresu: tyle Jiry, ile potrzebuje zespół 5–30 osób. Obejmuje to:
> - konfigurowalną ścieżkę zadania z automatycznym przydziałem,
> - tablicę kanban, sprinty (opcjonalne) i wersje,
> - powiązanie z GitLab i GitHub oraz środowiska testowe konkretnej wersji,
> - pełną historię zadania, załączniki (zdjęcia, filmy),
> - agentów jako uczestników ścieżki.
>
> **Bez** raportów portfelowych, wtyczek, pól formuł, schematów uprawnień per pole ani planowania
> zasobów.
>
> Fakty o repo pochodzą z przeglądu z 2026-09-29 (odnośniki `plik:linia` w §1).

---

## 0. Decyzje bazowe

1. **Funkcja w zespole ≠ poziom uprawnień.** Dzisiejsza rola członka (`viewer < tester < editor <
   manager < owner`, `models.rs:10`) jest **poziomem uprawnień**, a nie opisem pracy człowieka —
   stąd niejasność „programista = editor”. Zastępujemy ją **funkcjami** (PM, Designer UI/UX,
   Developer, Tester, Security, DevOps, Release Manager, Obserwator). Osoba może mieć **kilka
   funkcji naraz**, a uprawnienia są ich **sumą** (§2).
2. **Zakładki i akcje wynikają z uprawnień funkcji, a serwer to egzekwuje.** Dziś `modules_json`
   ukrywa zakładki wyłącznie w UI (`project-studio.js:1279`), a handlery zadań i testów go nie
   sprawdzają. To trzeba naprawić niezależnie od reszty planu (§2.3).
3. **[ZASTĄPIONE — zob. `BPMN_PROCESS_PLAN.md` §0 pkt 5 i §7, do potwierdzenia]** Docelowo workflow
   zadania to instancja procesu BPMN w warstwie procesów platformy (zadanie = instancja, kolumna =
   aktywne zadanie użytkownika, SLA = zdarzenie brzegowe czasowe, zdarzenia git = wiadomości).
   Poniższy opis przejściowy obowiązuje tylko, jeśli warstwa procesów nie zostanie przyjęta.
   **Stan zadania trzyma Projekty, a Flow Builder rysuje ścieżkę i wykonuje kroki automatyczne.**
   Silnik flow nie nadaje się dziś na maszynę stanów zadania, które żyje dniami:
   - przebiegi są w pamięci i po restarcie dostają `interrupted` (`agents/run_manager.rs:17,329`),
   - `ask_user` czeka najwyżej 3600 s i pyta tylko osobę, która uruchomiła przebieg (`ask_user.rs:89`),
   - nie ma zadań przypisanych do roli ani oczekiwania na zdarzenie
     (`docs/HARNESS_PLAN.md` §3.13 C nazywa to „jedyną dużą brakującą inwestycją pod BPMN”).

   Dlatego:
   - **definicja workflow** (statusy, przejścia, warunki, przydziały) to dokument projektu rysowany
     w edytorze graficznym na komponentach Flow Buildera (ten sam wygląd, osobna paleta bloków, §3.2),
   - **stan** zadania i każde przejście to wiersze w bazie projektu — trwałe, bez limitu czasu,
     odporne na restart,
   - **kroki automatyczne** (agent testujący, skan bezpieczeństwa, wdrożenie środowiska, podsumowanie
     LLM) to krótkie przebiegi flow lub agenta wywoływane przy przejściu; ich wynik wraca jako
     werdykt i kolejne przejście.

   Gdy silnik dostanie trwałe instancje (HARNESS_PLAN §6), definicję da się skompilować do niego
   bez zmiany formatu — dlatego format węzłów trzymamy zgodny z blokami flow.
4. **Agent może być uczestnikiem ścieżki.** Krok „Testy” może iść równolegle do testera-człowieka,
   agenta testującego i agenta bezpieczeństwa. Agent ma przypisany model (domyślnie **lokalny**,
   ale konfiguracja agenta dopuszcza dowolnego dostawcę — `agents.runtime_json`). Projekty nie
   ograniczają wyboru modelu, tylko zapisują, który model wykonał krok.
5. **GitLab i GitHub od pierwszej fazy** (webhooki, klucz zadania w gałęzi i commicie, status MR/PR).
6. **Tryb pracy per projekt:** Kanban (ciągły przepływ) albo Scrum (sprinty). Wersje/wydania
   działają w obu trybach.
7. **Zadanie może założyć każdy członek projektu** (developer, tester, PM, designer…) oraz
   **automat** (agent testów swobodnych, skaner, monitoring). Zakładanie nie zależy od funkcji.
   Workflow decyduje, dokąd zadanie trafia dalej (§4.3).
8. **Błędy mają wagę (krytyczny / istotny / mało istotny) niezależnie od SLA.** SLA to osobna,
   **definiowalna polityka** (kalendarzowa albo robocza z kalendarzem świąt), wspólna dla projektu —
   bez polityk per klient. Błąd bez SLA ma wagę, ale bez terminu i trafia do planowania sprintu (§4.2).
9. **Wykonawcą może być człowiek albo agent i obaj idą tą samą ścieżką.** Zmiana agenta przechodzi
   code review, testy i bramki jak zmiana programisty (§9).
10. **Brak limitów rozmiaru i liczby załączników.** Pilnujemy zamiast tego miejsca na dysku
    (§4.4).
11. **Commit wiąże się z zadaniem automatycznie po kluczu, a ręczne dowiązanie jest uzupełnieniem**
    (§7.1). Konta TentaFlow łączymy z GitLab/GitHub **automatycznie po tożsamości domenowej** (gdy dostawca git
    korzysta z tego samego AD/Entra), a uzupełniająco przez OAuth i ręczne mapowanie admina —
    nigdy po e-mailu z commita (§7.2).
12. **Moduł aplikacji jest bytem pierwszej klasy i zakłada się go wyłącznie ręcznie** (większe
    bloki, np. „Uprawnienia”, „OMS”, „Import OPC”). Ma główne osoby odpowiedzialne (developer, tester)
    i własne zdrowie (błędy, SLA). System niczego nie zgaduje: nie zakłada modułów, nie wybiera modułu
    zadania i nie przestawia go na podstawie commitów (§12).
13. **Roadmapa jest narzędziem PM w Projektach, także dla projektów nieprogramistycznych**
    (np. plan wdrożenia u klienta). Ma pozycje z terminami, kamienie milowe, zależności i postęp.
    Z wersjami i git wiąże się tylko wtedy, gdy projekt je ma. **Dwie warstwy: plan (zatwierdzone,
    wersjonowane plany bazowe) i realizacja (faktyczne daty i prognoza)** z przyczynami przesunięć.
    Zależności i równoległość działają na pozycjach **i na zadaniach**, ze ścieżką krytyczną (§13).
14. **Changelog żyje w Projektach i służy do skopiowania i wklejenia** tam, gdzie jest potrzebny
    (dokumentacja, mail, strona). Szkic pisze LLM językiem użytkownika, a zatwierdza człowiek. Każde
    zdanie wskazuje zadania, z których wynika (§16).
15. **Zestaw funkcji projektu jest konfigurowalny.** Każdą część (zadania, tablica, sprinty, roadmapa,
    moduły, wersje i changelog, git, testy, środowiska, bezpieczeństwo, wiedza, dokumenty, czat)
    włącza się i wyłącza per projekt. Szablony startowe podpowiadają zestaw, a serwer egzekwuje
    wyłączenie (§2.5).
16. **Planowanie zasobów jest częścią roadmapy: kto, kiedy, na ile.** Alokacje, dostępność i nieobecności
    trzymamy **centralnie** (nie w bazie projektu), bo utylizację liczy się między projektami
    (§14).
17. **Plan z dokumentów (wymagania, OPZ, przetarg) układa flow z Flow Buildera z subagentami**
    i dostarcza go jako **propozycję** do zatwierdzenia przez PM: wymagania z cytatami, roadmapę,
    kamienie milowe, zadania z szacunkami i potrzebnymi funkcjami. **Nie przydziela osób** — robi to
    PM (§15).

## 1. Stan obecny (fakty z repo)

| Obszar | Dziś | Luka |
|---|---|---|
| Statusy | 4 na sztywno: `todo/in_progress/review/done` (CHECK w `project_db.rs:431`, `tasks.rs:14`) | konfigurowalny workflow |
| Typy | `task`, `defect` | typy konfigurowalne (małe: historyjka/zadanie/błąd/zadanie bezpieczeństwa/podzadanie) |
| Wykonawca | jeden użytkownik, członek projektu | agent jako wykonawca kroku; równoległe przeglądy |
| Historia | `activity_log` tylko z nową wartością; brak widoku per zadanie, brak indeksu po `object_id` | pełna historia pól i przejść |
| Powiadomienia | tylko przy zmianie wykonawcy | przejście, komentarz, @wzmianka, termin |
| Załączniki | max 20, upload do 64 MB, pobranie do 8 MB, podgląd obrazów i tekstu | filmy (odtwarzacz, strumieniowanie), zrzuty ekranu wklejane w opis/komentarz |
| Tablica | `tf-kanban.js` z drag&drop, limitami WIP, obsługą klawiatury; 4 kolumny na sztywno | kolumny ze statusów workflow, swimlane (osoba/priorytet), filtry sprint/wersja |
| Powiązania | `links_json`: case/run/run_item/step | zadanie↔zadanie, commit, gałąź, MR/PR, wydanie |
| Sprinty, wersje | brak | §5, §6 |
| Git | tylko źródło wiedzy (`git_source.rs`) | webhooki, klucze zadań, statusy |
| Środowiska | cel testów ze stałym URL (`environments.rs`); build profile tylko do testów jednostkowych i dziś zablokowany (`auto_runs.rs:864`) | wdrażanie konkretnej wersji (§7) |
| Uprawnienia | minimalna rola w każdym handlerze (`require_project`, `dispatch/project_studio.rs:129`) | macierz funkcja × obszar (§2) |

## 1a. Podprojekty — drzewo projektów (rewizja 0.10)

Przykład, na którym opieramy reguły: produkt **NextApp** ma klientów, a każdy klient ma wdrożenie,
utrzymanie albo oba naraz.

```text
NextApp                         (produkt: kod, wersje, bezpieczeństwo, zadania produktowe)   NA
├─ Energetyka Centrum           (klient: grupa, bez własnych zadań)                          EC
│  ├─ Wdrożenie DCIM            (roadmapa, kamienie, zadania wdrożeniowe)                     ECW
│  └─ Utrzymanie                (zgłoszenia klienta, SLA, wersja u klienta)                   ECU
├─ Port Lotniczy Wschód
│  └─ Utrzymanie                                                                              PLU
└─ Bank Regionalny
   ├─ Wdrożenie                                                                               BRW
   └─ Utrzymanie                                                                              BRU
```

Każdym poziomem sterujemy niezależnie: ma swoich ludzi, swoje funkcje i ustawienia. Każdy poziom
wyżej **widzi też wszystko, co jest pod nim**: zadania, backlog, daily, czas pracy i roadmapę.

### 1a.1 Model

- **Każdy węzeł to pełny projekt.** Ma własną bazę (`<dir>/project.db`), członków, funkcje projektu,
  eksport i archiwum. Nie ma osobnego „typu podprojektu”. Węzeł grupujący, np. klient, to zwykły
  projekt z wyłączonymi Zadaniami (§2.5).
- **Drzewo leży w bazie głównej** (`projekty.db`): `projects.parent_id`, `path` (zmaterializowana
  ścieżka identyfikatorów) i `depth`. **Głębokość: najwyżej 4 poziomy** (produkt › klient ›
  wdrożenie/utrzymanie › etap). Głębsze drzewo nie mieści się w okruszkach nagłówka, a reguły
  uprawnień przestają być czytelne.
- **Dlaczego nie jedna baza na całe drzewo:** klient kończy umowę i jego podprojekt trzeba
  wyeksportować, zarchiwizować albo usunąć razem z danymi osobowymi (§14.3b). Przy osobnych bazach
  to jest istniejący eksport i archiwum jednego projektu (`archive.rs`). Przy wspólnej bazie byłoby
  to wycinanie wierszy z cudzego pliku. Za widoki zbiorcze płacimy więc indeksem centralnym (§1a.2).
- **Przeniesienie poddrzewa** w inne miejsce (zmiana rodzica) jest dozwolone dla właściciela obu
  miejsc. Przelicza dziedziczone ustawienia i uprawnienia i trafia do audytu.
- **Usunięcie rodzica wymaga pustego poddrzewa**: podprojekty trzeba najpierw przenieść albo usunąć.
  **Archiwizacja** rodzica pyta, czy objąć poddrzewo. **Eksport** ma opcję „z podprojektami”, która
  daje jedno archiwum zip z archiwami węzłów w środku.

### 1a.2 Widoki zbiorcze — „piętro wyżej widzi niżej”

- **Zakres widoku:** przełącznik **Ten projekt / Z podprojektami** w rzędzie filtrów. Na węźle
  z dziećmi domyślnie włączony jest zakres z podprojektami; wybór pamiętamy per osoba i widok.
  Zakres działa w widokach Lista, Tablica, Backlog i sprinty, Do weryfikacji, Daily, Raport
  tygodniowy, Czas pracy i Roadmapa. Dochodzi filtr **Projekt** (wielokrotny wybór z poddrzewa)
  i kolumna „Projekt”.
- **Indeks centralny** `task_index` w `projekty.db`. Zawiera: `project_id`, `task_id`, klucz,
  tytuł, typ, wagę, status, **kategorię statusu**, wykonawcę, sprint, wersję, termin i stan SLA,
  pozycję w backlogu, znacznik poufności i `revision`.
  - **Zapis:** zmiana zadania w bazie projektu dopisuje w tej samej transakcji wiersz do
    `index_outbox`. Aplikator przenosi go do indeksu idempotentnie po
    `(project_id, task_id, revision)`.
  - **Odtwarzanie:** przy starcie i po wykryciu rozjazdu indeks przebudowuje się z baz projektów.
  - **Opóźnienie:** zwykle poniżej sekundy. Widok zbiorczy pokazuje „odświeżone przed chwilą”
    zamiast udawać spójność transakcyjną.
  - **Powód istnienia:** otwartych baz projektów jest najwyżej 16 (`MAX_OPEN_POOLS`,
    `project_db.rs:24`), więc drzewo z 30 podprojektami nie może otwierać wszystkich baz na każde
    odświeżenie tablicy. Z tego samego indeksu korzysta „Moja praca”.
- **Szczegóły zawsze z bazy własnego projektu.** Klik w zadanie otwiera kartę w jego projekcie,
  a okruszki pokazują pełną ścieżkę. Widoki zbiorcze nie mają osobnej kopii zadania.
- **Poufne** (zadania bezpieczeństwa, sprawy PSIRT) trafiają do indeksu tylko z kluczem i flagą.
  Kto nie ma do nich dostępu w swoim projekcie, nie widzi ich także piętro wyżej.

### 1a.3 Tablica nad podprojektami z różnymi workflow

- **Każdy status workflow ma kategorię:** Do zrobienia / W toku / Weryfikacja / Zakończone. To
  odpowiednik dzisiejszych `todo/in_progress/review/done`, więc migracja jest wprost.
- **Kolumny tablicy biorą się z workflow węzła, na którym się stoi**, więc tablica produktu wygląda
  jak dziś. Zadanie podprojektu z tym samym (dziedziczonym) workflow trafia do swojej kolumny.
  Zadanie z **innym** workflow trafia do pierwszej kolumny tej samej kategorii, a karta pokazuje
  jego własny status (np. „U klienta”).
- **Przeciągnięcie takiej karty:**
  - jedno przejście prowadzi do kategorii kolumny docelowej → wykonuje się,
  - kilka przejść → wybór z listy,
  - żadne → odmowa z podanym powodem.
  Uprawnienia do przejścia liczą się **w projekcie zadania**, nie w węźle, na którym jest tablica.
- **Wiersze „Podprojekty”** (swimlane) dzielą tablicę na pasy per podprojekt. Limity WIP obowiązują
  w projekcie, który je ustawił. Na tablicy zbiorczej liczba przy kolumnie jest informacyjna i pokazuje
  osobno część własną i część z podprojektów.

### 1a.4 Backlog i sprinty ponad podprojektami

- **Backlog zbiorczy** to suma backlogów poddrzewa, uszeregowana na węźle, na którym się stoi.
  Pozycję w kolejce trzyma właściciel planu (węzeł), więc podprojekt może mieć inną kolejność
  u siebie.
- **Sprint należy do węzła, w którym powstał.** Zespół pracujący nad produktem i utrzymaniem kilku
  klientów planuje **jeden sprint na poziomie NextApp** z zadaniami NA, ECU i BRU. Zadanie może
  wejść do sprintu swojego projektu albo dowolnego przodka i jest w jednym sprincie naraz. W podprojekcie
  widać „w sprincie NextApp 42”.
- Podprojekt może prowadzić **własne sprinty** niezależnie, np. wdrożenie z własnym zespołem.
  Pojemność zespołu liczy się jak dotąd z alokacji między projektami (§14).

### 1a.5 Dziedziczenie ustawień — każdym poziomem sterujemy niezależnie

Dla każdego obszaru węzeł ma **Dziedziczy** (odwołanie do rodzica, zmiany rodzica dochodzą
same) albo **Własne** (kopia odłączona od rodzica, dalej edytowana u siebie). „Przywróć
dziedziczenie” pokazuje różnicę przed nadpisaniem.

| Obszar | Domyślnie | Uwagi |
|---|---|---|
| Członkowie i funkcje | dziedziczy **w dół** | Rola na rodzicu obowiązuje w całym poddrzewie i nie da się jej obniżyć niżej. W podprojekcie można dodać osoby tylko do niego: widzą ten podprojekt i nazwy przodków w okruszkach, ale nie ich zawartość. |
| Podprojekt prywatny | wyłączone | Dla klienta pod NDA: dziedziczą tylko właściciele korzenia, reszta wyłącznie z jawnego członkostwa. Widoki zbiorcze pomijają taki węzeł bez śladu w licznikach dla osób bez dostępu. |
| Workflow, typy zadań, pola | dziedziczy | Zmiana workflow rodzica to nowa wersja (§3). Dziedziczące podprojekty biorą ją dla nowych zadań. |
| SLA i kalendarz | dziedziczy | **Bez polityk per klient (decyzja z §0):** podprojekt wybiera politykę z katalogu korzenia (np. „utrzymanie” zamiast „wytwarzanie”), ale nie tworzy własnej. Własny kalendarz jest dozwolony (np. święta klienta za granicą). |
| Funkcje projektu (§2.5) | dziedziczy | Podprojekt ma te same zakładki co rodzic, więc nawigacja jest wszędzie taka sama. Wyłączenie funkcji w podprojekcie chowa zakładkę tylko tam. |
| Moduły aplikacji (§12) | dziedziczy | Zadania klienta oznacza się modułami produktu. Podprojekt może dołożyć własne, np. „integracja z systemem klienta”. |
| Integracje git (§7) | własne | Repozytorium produktu jest zwykle na korzeniu, a repozytorium dostosowań u klienta. Klucz zadania w commicie w dowolnym repo wskazuje projekt po prefiksie (§1a.6). |
| Wersje i wydania (§6) | **wersje produktu na korzeniu** | Podprojekt ma pole **„wersja u klienta”** z historią wdrożeń. Błąd zgłoszony w utrzymaniu dostaje wersję poprawki z wersji przodka (np. NA 2.5.1). |
| Bezpieczeństwo | na produkcie | Z „wersji u klienta” powstaje odpowiedź na pytanie **którzy klienci mają podatną wersję**: to lista odbiorców powiadomienia o podatności (CRA art. 14 ust. 8, `PROJECT_STUDIO_SECURITY_PLAN.md`). |
| Roadmapa (§13) | własna | Rodzic pokazuje roadmapy dzieci jako zwinięte grupy strumieni. Zależności między pozycjami różnych węzłów są dozwolone. |
| Changelog (§16) | na produkcie | Dla klienta: changelog między wersją u klienta a wersją docelową (§16.5). |

### 1a.6 Klucze zadań i przenoszenie

- **Prefiks klucza na węzeł** (`NA`, `ECW`, `ECU`, `BRU`), unikalny w organizacji (rejestr
  w `projekty.db`). Dzięki temu commit `ECU-12` w repozytorium produktu wiąże się z zadaniem
  utrzymania klienta.
- **Przeniesienie zadania do innego węzła** zmienia klucz. Stary klucz zostaje aliasem
  (`task_key_aliases` w `projekty.db`), więc odnośniki, commity i adresy dalej prowadzą do zadania.
  - **Historia** przechodzi razem z zadaniem.
  - **Załączniki** są adresowane treścią, więc się nie powielają.
  - **Zadanie poufne** nie przechodzi do węzła z szerszym dostępem bez jawnego potwierdzenia.
- **Nowe zadanie na węźle z dziećmi** ma pole „Projekt” ustawione na bieżący węzeł. Lista wyboru
  pokazuje poddrzewo, a wybór podprojektu przełącza typy i SLA na tamtejsze.

### 1a.7 Interfejs

- **Wybór podprojektu w nagłówku projektu:**
  - okruszki pokazują pełną ścieżkę,
  - strzałka przy nazwie projektu otwiera drzewo: wyszukiwarka, liczniki otwartych i po terminie SLA,
    „tylko moje” i „+ Podprojekt” dla menedżera,
  - przełączenie zostawia bieżący widok (Tablica → Tablica), jeśli węzeł ma tę funkcję; inaczej
    otwiera się Przegląd.
- **Znacznik w nagłówku** „8 podprojektów” oraz, w podprojekcie, lista tego, co dziedziczy.
- **Pełne zakładki i pełny pasek Zadań w każdym podprojekcie**, tak jak w projekcie głównym.
- **Lista projektów** pokazuje projekty główne z rozwijanym drzewem na karcie. Podprojekt, do którego
  ktoś należy **bez** dostępu do przodka, dostaje własną kartę ze ścieżką.
- **Ustawienia → Podprojekty:** drzewo węzła z przeciąganiem, tabela dziedziczenia z §1a.5,
  zakładanie podprojektu w oknie z szablonu (Wdrożenie / Utrzymanie / Pusty), menu „⋯” podprojektu (otwórz, ustawienia, przenieś, eksportuj, zakończ, usuń).

### 1a.8a Zakończenie podprojektu (koniec umowy z klientem)

Zakończenie to osobny stan cyklu życia, różny od „prywatny” (widoczność) i od usunięcia. Wejścia:
menu „⋯” wiersza w Ustawienia → Podprojekty oraz blok „Koniec umowy z klientem?” w szczegółach
podprojektu. Okno zakończenia (mockup D11) ma cztery kroki:

1. **Stan dziś:** otwarte zadania, zadania w sprintach przodków, suma czasu pracy z datą ostatniego
   wpisu, osoby, które są członkami wyłącznie tego podprojektu.
2. **Otwarte zadania:** przeniesienie do innego węzła (nowe klucze, stare zostają aliasami, §1a.6)
   albo zamknięcie ze stanem „Nie będzie realizowane” i powodem „Koniec umowy”. Zakończyć nie da się,
   dopóki zostaje choć jedno otwarte zadanie.
3. **Skutki**, pokazane przed potwierdzeniem:
   - podprojekt przechodzi do archiwum: jest w drzewie jako zakończony i tylko do odczytu, a widoki
     zbiorcze i liczniki przodków go pomijają,
   - zegary SLA stają, a nowe wpisy czasu są zablokowane,
   - członkowie wyłącznie tego podprojektu tracą dostęp; ich konta zostają,
   - webhooki podprojektu są wyłączane, a repozytorium w GitLabie zostaje bez zmian,
   - eksport dla klienta (zadania, historia, załączniki, czas pracy) można pobrać od razu,
   - dane osobowe są usuwane albo pseudonimizowane po okresie retencji z polityki prywatności
     (`ORG_STRUCTURE_PLAN.md` §6.4; domyślnie 5 lat od zakończenia).
4. **Potwierdzenie** przez wpisanie klucza podprojektu. Zakończenie idzie do audytu.

**Wznów** przywraca stan aktywny: zadania zostają tam, dokąd je przeniesiono, a dostęp osób z samego
podprojektu wraca. **Usunąć** na stałe można wyłącznie zakończony podprojekt bez podprojektów.
Zakończenie węzła z dziećmi wymaga najpierw zakończenia albo przeniesienia dzieci.

### 1a.8 Ryzyka

- **Rozjazd indeksu z bazą projektu:** aplikator jest idempotentny, a przebudowa jest możliwa w każdej
  chwili. Rozjazd liczb w nagłówku jest widoczny i się naprawia; utracony zapis byłby gorszy.
- **Wyciek przez agregację:** uprawnienia liczy serwer przy każdym żądaniu, na drzewie trzymanym
  w pamięci. Licznik zbiorczy powstaje wyłącznie z węzłów, które pytający widzi.
- **Rozrost drzewa:** limit 4 poziomów, a przełącznik ma wyszukiwarkę od pierwszego dnia.

## 2. Funkcje w zespole i uprawnienia

### 2.1 Funkcje (domyślny katalog; projekt może go dopasować)

| Funkcja | Czym się zajmuje w ścieżce | Typowo widzi zakładki |
|---|---|---|
| **PM / Product Owner** | zakłada i priorytetyzuje zadania, planuje sprinty i wersje, akceptuje wynik | wszystko poza sekretami i sprawami poufnymi |
| **Analityk** | doprecyzowuje wymagania, kryteria akceptacji | Zadania, Wiedza, Dokumenty |
| **Designer UI/UX** | projekty graficzne (pliki, zrzuty, linki do Figmy) | Zadania, Dokumenty, Wiedza |
| **Developer** | implementacja, code review cudzych zmian, poprawki | Zadania, Repozytoria, Testy (odczyt), Środowiska |
| **Tester** | testy manualne i automatyczne, zgłaszanie błędów, retesty | Zadania, Testy, Środowiska, Raporty |
| **Security** | przeglądy bezpieczeństwa, pentesty, triage podatności (dawny „Security Champion”, §2.4) | + zakładka Bezpieczeństwo |
| **DevOps** | środowiska, wdrożenia, integracje git i CI | Środowiska, Ustawienia integracji |
| **Release Manager** | wersje, wydania, zatwierdzanie wydania | Wydania, Zadania (odczyt) |
| **Obserwator / Klient** | podgląd postępu, komentarze (opcjonalnie) | Pulpit, Zadania (odczyt), Wydania |

Członek projektu ma **zbiór funkcji** (np. `developer` + `security`) i jeden znacznik
**administratora projektu** (dzisiejszy `owner`/`manager`: członkowie, ustawienia, workflow).

### 2.2 Macierz uprawnień funkcji (edytowalna per projekt, domyślne wartości z szablonu)

Obszary: `tasks`, `board`, `sprints`, `releases`, `tests`, `environments`, `repos`, `knowledge`,
`docs`, `chat`, `security`, `security.confidential`, `settings`. Poziomy: `—` (brak, zakładka
niewidoczna) / `R` (odczyt) / `W` (zapis) / `A` (administracja obszaru).

| Obszar | PM | Analityk | Designer | Developer | Tester | Security | DevOps | Release | Obserwator |
|---|---|---|---|---|---|---|---|---|---|
| tasks / board | A | W | W | W | W | W | R | R | R |
| sprints | A | R | R | R | R | R | R | R | R |
| roadmap | A | W | R | R | R | R | R | R | R |
| modules (definicja, osoby odpowiedzialne) | A | R | R | R | R | R | W (ścieżki) | R | — |
| changelog | W | W | R | W (wpisy zadań) | W (wpisy zadań) | R | R | A (zatwierdzenie) | R (opublikowany) |
| releases | W | R | R | R | R | R | R | A | R |
| tests | R | R | — | R | A | W | R | R | — |
| environments | R | — | R | W | W | W | A | R | — |
| repos | — | — | — | W | R | R | A | R | — |
| security | R | — | — | R | R | A | R | R | — |
| security.confidential | — | — | — | — | — | A | — | — | — |
| settings | W (bez integracji) | — | — | — | — | — | W (integracje) | — | — |

Uprawnienie efektywne = maksimum po funkcjach osoby. Administrator projektu ma `A` wszędzie
poza `security.confidential`, które nadaje się jawnie.

### 2.3 Egzekwowanie

- **Serwer:** każdy handler deklaruje `(obszar, poziom)` zamiast minimalnej roli, a
  `require_project` liczy uprawnienie z funkcji. Obszar wyłączony w projekcie (`modules_json`)
  daje odmowę na serwerze, nie tylko ukrytą zakładkę.
- **UI:** lista zakładek i przyciski z tej samej macierzy (jedno źródło, zwracane w `project_get`).
- **Migracja:** `owner` → admin + wszystkie funkcje; `manager` → admin + PM; `editor` → Developer;
  `tester` → Tester; `viewer` → Obserwator.

### 2.4 „Security Champion” — co to było

Termin z procesów SSDLC (OWASP) oznacza **programistę z zespołu, który jest pierwszym punktem
kontaktu w sprawach bezpieczeństwa**: ocenia, czy zmiana dotyka bezpieczeństwa, robi przegląd
wrażliwych MR i pierwszy triage alertów. W tym modelu to po prostu osoba z funkcjami `developer`
+ `security`, więc osobna nazwa w interfejsie nie jest potrzebna.

### 2.5 Konfigurowalny zestaw funkcji projektu

Dzisiejsze `modules_json` (knowledge, tests, docs, chat, tasks) rozszerzamy do pełnej listy
**funkcji projektu** (nie mylić z modułami aplikacji z §12):

| Funkcja projektu | Zależy od | Szablon „Projekt programistyczny” | Szablon „Wdrożenie / projekt PM” | Szablon „Prosty” |
|---|---|---|---|---|
| Zadania + tablica | — | ✓ | ✓ | ✓ |
| Workflow własny (§3) | Zadania | ✓ | opcjonalnie | — (4 statusy) |
| Sprinty | Zadania | ✓ (Scrum) / — (Kanban) | — | — |
| **Roadmapa** | — | opcjonalnie | ✓ | — |
| Moduły aplikacji | Zadania | ✓ | — | — |
| Wersje, wydania, **changelog** | Zadania | ✓ | — | — |
| Integracja git | Wersje | ✓ | — | — |
| Testy, środowiska | — | ✓ | — | — |
| Bezpieczeństwo | Wersje, git | opcjonalnie | — | — |
| Wiedza, dokumenty, czat | — | ✓ | ✓ | ✓ |
| SLA | Zadania | opcjonalnie | opcjonalnie | — |

- Szablon tylko **podpowiada** zestaw; manager/admin projektu zmienia go w ustawieniach.
- Włączenie funkcji zależnej włącza (po potwierdzeniu) to, od czego zależy.
- Wyłączenie **ukrywa i blokuje** funkcję (serwer odmawia, §2.3), ale **nie kasuje danych**.
  Ponowne włączenie przywraca stan. Wyjątek: bezpieczeństwo, które ma retencję dowodów niezależną
  od przełącznika.
- Obszary z macierzy uprawnień (§2.2) dotyczą tylko funkcji włączonych w projekcie.

## 3. Workflow zadania

### 3.1 Elementy definicji

| Element | Znaczenie | Na tablicy |
|---|---|---|
| **Status** | etap zadania (np. „Projekt UI”, „W realizacji”, „Testy”) | kolumna (kilka statusów może dzielić kolumnę) |
| **Krok ludzki** | status z przypisaną **funkcją** i regułą przydziału: ręcznie / **główna osoba modułu** (§12) / rotacyjnie / najmniej obciążony / „ten sam co poprzednio” (programista wraca do swojego błędu) | karta z awatarem |
| **Krok agenta** | status obsługiwany przez agenta (np. „Agent testujący”, „Agent bezpieczeństwa”); wynik = werdykt + komentarz + załączniki | karta z ikoną agenta i stanem przebiegu |
| **Krok automatyczny** | skan, wdrożenie środowiska, podsumowanie LLM — flow lub narzędzie | nie jest kolumną, tylko etykietą „w toku” |
| **Decyzja** | warunek na polach zadania (`wymaga_projektu_ui = tak`, `typ = błąd`, `security_impact = high`) albo na werdyktach | — |
| **Bramka równoległa** | kilka przeglądów naraz (tester + agent testujący + agent security); reguła wyjścia: *wszystkie OK* / *pierwszy błąd wraca* | kolumna z licznikiem „2/3 OK” |
| **Przejście** | dozwolony ruch między statusami: kto może (funkcja), wymagane pola (np. „opis błędu”, „wersja”), automatyzacje po przejściu | przeciągnięcie karty (niedozwolone = kolumna wyszarzona) |
| **Zdarzenie zewnętrzne** | MR otwarty / scalony, tag, wynik pipeline'u, wdrożenie gotowe | automatyczne przejście |

### 3.2 Edytor: istniejący Flow Builder z elementami BPMN 2.0 (decyzja 2026-09-29)

Workflow zadań edytuje się **w istniejącym Flow Builderze** (`www/js/modules/flows-builder.js`,
`flows-builder/palette.js`, `canvas.js`), a nie w osobnym edytorze. Flow Builder dostaje nowy **rodzaj
przepływu „Workflow zadań (BPMN 2.0)”**: ten sam układ (paleta · płótno · inspektor · minimapa ·
pasek zapisu), inny zestaw bloków w palecie i notacja BPMN na płótnie.

| Kategoria palety | Elementy BPMN 2.0 | Znaczenie w Projektach |
|---|---|---|
| Zdarzenia | Start, Koniec, Zdarzenie czasowe, Zdarzenie wiadomości, Zdarzenie brzegowe czasowe, Zdarzenie brzegowe błędu | wejście zadania; zamknięcie; przypomnienia cykliczne; zdarzenie git (MR otwarty/scalony, tag); **SLA i eskalacja** jako timer brzegowy na zadaniu; błąd kroku agenta |
| Zadania | Zadanie użytkownika, Zadanie usługi (agent), Zadanie usługi (automat), Wywołanie procesu, Podproces | krok człowieka z funkcją; krok agenta; skan / wdrożenie środowiska / powiadomienie; np. wywołanie procesu PSIRT; grupa kroków |
| Bramki | Wyłączna (XOR), Równoległa (AND), Inkluzywna (OR), Oparta na zdarzeniach | decyzja na polach zadania; tester + agent testujący + agent security naraz; „którzy z recenzentów”; „co przyjdzie pierwsze: MR scalony czy anulowanie” |
| Tory | Pula, Tor | **tor = funkcja w zespole** (PM, Designer, Developer, Tester, Security, Release Manager) — przydział kroków do funkcji wynika z toru |
| Artefakty | Obiekt danych, Adnotacja | załącznik / zrzut wymagany w kroku; opis dla zespołu |

- **Inspektor** (zakładki jak w Flow Builderze): Konfiguracja · Przydział (reguła: główna osoba modułu /
  rotacyjnie / najmniej obciążony / ten sam co poprzednio / ręcznie) · SLA i terminy (pauza zegara,
  próg eskalacji) · Zaawansowane (identyfikator BPMN, dokumentacja, wymagane pola przejścia).
- **Import / eksport BPMN 2.0 XML** bez strat: elementy standardowe mapują się 1:1; atrybuty TentaFlow
  (funkcja toru, reguła przydziału, polityka SLA, agent) idą jako elementy rozszerzeń w przestrzeni
  nazw `tentaflow:`. Plik da się otworzyć w Camunda Modeler / bpmn.io i wczytać z powrotem.
- **Walidacja BPMN** na żywo (pasek dolny): osiągalność, brak martwych końców, każda bramka AND ma
  złączenie, każde zadanie użytkownika leży w torze.
- **„Symuluj”** zamiast „Test”: przeprowadza przykładowe zadanie przez ścieżkę i pokazuje, kto
  dostałby każdy krok.
- **Wykonanie** nadal należy do Projektów (stan zadania w bazie projektu, §0 decyzja 3). Flow Builder
  jest edytorem definicji, a bloki „Zadanie usługi (agent/automat)” wywołują zwykłe przepływy i agentów
  Flow Buildera. Gdy silnik flow dostanie trwałe instancje (HARNESS_PLAN §6), ten sam dokument BPMN
  będzie mógł się wykonywać bezpośrednio.
- **Wersjonowanie:** reguła z §3.2 poprzedniej rewizji bez zmian (mapa statusów przy publikacji,
  `flow_versions`).
- **Wejście:** Ustawienia projektu → Workflow → otwiera Flow Builder z przepływem projektu; powrót
  „← Ustawienia projektu”.

### 3.3 Przykład: „Wytwarzanie z UI i security”

Diagram: `docs/flows/project-studio-workflow-example.bpmn` (+ `.png`, `.svg`).

```text
Nowe (PM) → Analiza (PM/Analityk)
  → [wymaga projektu UI?] tak → Projekt UI (Designer) → Akceptacja projektu (PM) ─┐ (odrzucony → Projekt UI)
                          nie ────────────────────────────────────────────────────┤
  → Do zrobienia (Developer) → W realizacji (Developer)
  → [MR otwarty] → Code review (inny Developer) (uwagi → W realizacji)
  → [MR scalony] → wdrożenie środowiska testowego wersji (automat)
  → Testy ║ tester człowiek ║ agent testujący (E2E/eksploracyjny) ║ agent security (gdy security_impact ≠ none)
       wszystkie OK → Gotowe do wydania (Release) → [wydanie opublikowane] → Wydane
       jakikolwiek błąd → powrót do W realizacji, do TEGO SAMEGO developera, runda +1
```

Pętla tester ↔ programista („ping-pong”) jest jawna: zadanie ma **licznik rund testów**, a historia
pokazuje każdą rundę z werdyktami. Przekroczenie progu (np. 3 rundy) daje powiadomienie PM
i oznaczenie na tablicy.

## 4. Zadanie (karta)

### 4.1 Typy zadań (domyślny katalog; projekt może dodawać własne)

| Typ | Po co | Kluczowe pola |
|---|---|---|
| **Funkcjonalność** | nowa funkcja / historyjka | kryteria akceptacji, `wymaga_projektu_ui`, wersja docelowa |
| **Błąd** | nieprawidłowe działanie | **waga**, **SLA** (tak/nie + polityka), kroki odtworzenia, oczekiwane / faktyczne, wersja i środowisko wystąpienia, załączniki |
| **Zadanie techniczne** | refaktoryzacja, aktualizacja zależności, infrastruktura | uzasadnienie |
| **Zadanie bezpieczeństwa** | poprawka podatności, model zagrożeń, retest (moduł bezpieczeństwa) | powiązane znalezisko / sprawa, poufność |
| **Podzadanie** | fragment większego zadania | rodzic |
| **Epik** | grupa zadań (funkcja większa niż sprint) | zadania podrzędne, postęp |

Każdy typ może mieć **własny workflow** (np. błąd krytyczny z SLA idzie ścieżką przyspieszoną:
od razu do dyżurnego developera, z pominięciem planowania).

### 4.2 Waga, priorytet i SLA

- **Waga** (tylko błędy): **krytyczny** (system nie działa / utrata danych / brak obejścia),
  **istotny** (ważna funkcja działa źle, jest obejście), **mało istotny** (kosmetyka, drobne
  niedogodności). Definicje wag są tekstem w ustawieniach projektu, żeby zgłaszający wybierali
  spójnie.
- **SLA** to **polityka** przypięta do błędu. Polityki definiuje się w ustawieniach projektu
  (albo w szablonie organizacji). Nie ma polityk per klient — to decyzja, nie brak.
  Polityka składa się z:
  - **terminów per waga** (liczba godzin, np. krytyczny 24 h, istotny 48 h, mało istotny 72 h — **w pełni konfigurowalne**; to, ile trwa godzina terminu, zależy od podstawy liczenia),
  - **podstawy liczenia czasu:** **kalendarzowy** (24/7) albo **roboczy** (§4.2.1),
  - **momentu startu:** zgłoszenie albo potwierdzenie w „Do weryfikacji” (dla zgłoszeń automatów
    zawsze potwierdzenie),
  - **statusów pauzy:** oznaczone w workflow, np. „Czeka na informacje”,
  - **statusów zatrzymujących zegar** (cel SLA): np. „Obejście dostarczone” albo „Poprawka
    wdrożona” — wybór w polityce, bo cel bywa różny,
  - **progów przypomnień i eskalacji** (domyślnie 50% i 80% czasu, przekroczenie → PM + raport
    tygodniowy). Łańcuch eskalacji ponad PM (przełożeni, zastępcy kierowników) pochodzi ze struktury
    organizacyjnej (`ORG_STRUCTURE_PLAN.md` S3); **do czasu S3 eskalacja kończy się na PM
    i administratorze projektu** — SLA działa od razu, a łańcuch wydłuża się po wdrożeniu struktury.

  Domyślne polityki w nowym projekcie (edytowalne):

  | Polityka | krytyczny | istotny | mało istotny | Czas |
  |---|---|---|---|---|
  | **SLA kalendarzowe** | 24 h | 48 h | 72 h | kalendarzowy |
  | **SLA robocze** | 24 h pracy (3 dni rob.) | 48 h pracy (6 dni rob.) | 72 h pracy (9 dni rob.) | roboczy (kalendarz projektu) |
  | **Bez SLA** | — | — | — | waga bez terminu, planowanie sprintu |

  Błąd dostaje politykę przy zgłoszeniu (domyślna polityka typu zadania, możliwa zmiana
  z uzasadnieniem).
- **Zmiana wagi, polityki albo zdjęcie SLA** wymaga uzasadnienia i jest widoczne w historii —
  inaczej wskaźniki SLA łatwo „poprawić” przeklasyfikowaniem.
- **Priorytet** (wszystkie typy) służy do kolejności pracy i jest niezależny od wagi. Błąd
  mało istotny może mieć wysoki priorytet, bo przeszkadza ważnemu klientowi.

#### 4.2.1 Kalendarz roboczy

- Dni robocze (domyślnie pon.–pt.) i godziny pracy (np. 8:00–16:00) w **strefie czasowej projektu**
  (domyślnie `Europe/Warsaw`, poprawnie przez zmianę czasu letniego i zimowego).
- **Święta:** wbudowany kalendarz świąt ustawowych w Polsce — stałe daty plus ruchome, liczone
  od Wielkanocy (Poniedziałek Wielkanocny, Boże Ciało; Zielone Świątki wypadają w niedzielę) —
  oraz ręczne dni wolne projektu (np. dzień wolny za święto w sobotę, przerwa świąteczna firmy).
  Lista świąt jest danymi z datą obowiązywania: zmiana przepisów to aktualizacja danych, nie kodu
  (przykład: Wigilia jest dniem wolnym od 2025 r., więc błąd krytyczny zgłoszony 23.12 liczy się
  inaczej niż przed tą zmianą).
- **Konwencja (decyzja 2026-09-29):** w polityce roboczej termin to **godziny pracy**. „24 h” =
  24 godziny pracy = przy 8 h dziennie **3 dni robocze**, „48 h” = 6 dni roboczych, „72 h” = 9 dni
  roboczych. W polityce kalendarzowej „24 h” to 24 h zegarowe przy założeniu
  ciągłej pracy. Terminy wszystkich polityk zapisujemy w **godzinach** (dzień = 8 h pracy albo
  24 h zegara), więc obie podstawy liczy ten sam mechanizm, a różni je tylko kalendarz.
  Przykład: błąd krytyczny zgłoszony w piątek o 15:00 (godziny pracy 8:00–16:00) ma termin
  roboczy w środę o 15:00 (1 h w piątek + 8 h pon. + 8 h wt. + 7 h śr.), a kalendarzowy
  w sobotę o 15:00.
- Termin liczony jest raz przy starcie, przeliczany po pauzie i przy zmianie wagi (z historią).
  Na karcie widać zarówno datę terminu, jak i pozostały czas w jednostce polityki.

### 4.3 Kto zgłasza i dokąd trafia zgłoszenie

- Zakładać zadania mogą **wszyscy członkowie** (uprawnienie `tasks: create` dla każdej funkcji,
  także Obserwatora — np. klient zgłaszający błąd; admin projektu może to wyłączyć).
- Nowe zadanie trafia do statusu wejściowego workflow swojego typu (np. „Do weryfikacji”),
  z przydziałem do funkcji PM albo dyżurnego — reguła workflow, nie osoby zgłaszającej.
- **Zgłoszenia automatów** (agent testów swobodnych „klikający” po aplikacji, skanery, monitoring):
  - mają znacznik `źródło = automat` i nazwę agenta, a załącznikami są zrzuty, nagranie
    i log przeglądarki,
  - trafiają do statusu **„Do weryfikacji”** — człowiek potwierdza, że to prawdziwy błąd, i nadaje
    wagę, zanim zacznie biec SLA (zasada: automat zgłasza, człowiek kwalifikuje),
  - przechodzą **deduplikację**: odcisk z adresu ekranu, kroków i komunikatu błędu. Kolejne
    wystąpienie tego samego błędu dopisuje się do istniejącego zadania jako „wystąpienie N”
    zamiast zakładać nowe,
  - mają limit zadań na przebieg agenta (np. 20), żeby jedna usterka środowiska nie zasypała tablicy.

### 4.4 Pola, załączniki, komentarze, historia

- **Pola stałe:** klucz (`NA-123` — prefiks projektu + numer, używany w git), typ, tytuł, opis
  (markdown z wklejanymi obrazami), priorytet, status, wykonawca bieżącego kroku, reporter,
  wersja docelowa (`fix version`), sprint, etykiety, termin, szacunek (punkty albo godziny —
  wybór w projekcie), rodzic (epik / zadanie nadrzędne).
- **Pola własne workflow:** deklarowane w definicji (np. `wymaga_projektu_ui: bool`,
  `środowisko_błędu: tekst`, `security_impact`). Tylko proste typy: tekst, liczba, wybór, tak/nie,
  data, osoba.
- **Powiązania:** zależność (koniec→start / start→start / koniec→koniec / start→koniec, z opóźnieniem — §13.6) / powiązane / duplikat; commit, gałąź, MR/PR (z git,
  §6); przypadek testowy, przebieg; wydanie; znalezisko bezpieczeństwa.
- **Załączniki bez limitu rozmiaru i liczby** (decyzja): obrazy, filmy, archiwa, logi, zrzuty
  pamięci. Technicznie:
  - wysyłanie **wznawialne** w kawałkach (dzisiejsze 4 MB zostają, znika limit 64 MB na plik
    i 20 plików na zadanie),
  - pobieranie **strumieniowe** z żądaniami Range (znika limit 8 MB `ATTACHMENT_MAX_BYTES`),
  - pliki na dysku projektu adresowane skrótem (jak dziś), bez przechowywania w bazie.

  Podgląd w karcie:
  - obrazy: galeria z miniaturami,
  - filmy: odtwarzacz z przewijaniem; formaty, których przeglądarka nie odtworzy, konwertowane
    w tle do podglądu przez GStreamer, który TentaFlow już ma (oryginał zostaje),
  - zrzut ekranu wklejany ze schowka do opisu i komentarza,
  - pozostałe pliki do pobrania.

  Agent testujący dołącza zrzuty i nagranie jako dowód błędu. Brak limitu nie znaczy braku
  kontroli miejsca: widok „Zajętość” projektu (największe pliki, per zadanie), ostrzeżenie przy
  kończącym się miejscu na węźle i reguła retencji do wyboru (np. filmy z zamkniętych zadań po
  roku do archiwum). Domyślnie niczego nie kasujemy automatycznie.

  Eksploatacja przy braku limitów:
  - **ochrona węzła, nie limit projektu:** przy wolnym miejscu poniżej progu (np. 10%) nowe wysyłki
    czekają z czytelnym komunikatem, żeby dysk nie zapełnił się do zera i nie położył bazy,
  - **kopie zapasowe:** katalog plików projektu jest kopiowany osobno od baz (przyrostowo, pliki
    adresowane skrótem się nie zmieniają), żeby zrzut bazy nie puchł od filmów,
  - **skan antywirusowy (opcjonalny):** kontener ClamAV z `tentaflow-containers` sprawdza wysłane
    pliki; plik z wykryciem jest oznaczony i niepobieralny do decyzji administratora,
  - **pobieranie:** strumieniowe z żądaniami Range; zapytania przekrojowe (utylizacja, daily) nie
    dotykają plików, tylko metadanych.
- **Komentarze:** markdown, @wzmianki (powiadomienie), edycja z historią.
- **Historia (nowa tabela `task_events`):** każda zmiana pola `z → na`, przejście z nazwą przejścia,
  zmiana wykonawcy, werdykty kroków (człowiek/agent + model), commity i MR, wdrożenia, rundy testów.
  Widok: oś czasu w karcie + czas spędzony w każdym statusie (cycle time).
- **Szablony zadań:** np. „Zgłoszenie błędu” z polami: kroki, oczekiwane, faktyczne, środowisko, wersja.

### 4.5 Przypisanie, zmiana osoby i przekazanie (rewizja 0.11)

Trzy operacje, rozróżniane w historii i w powiadomieniach:

| Operacja | Kiedy | Co wymaga | Ślad |
|---|---|---|---|
| **Przypisz** | nikt nie odpowiada albo PM rozdziela pracę | uprawnienie „przydzielanie” w macierzy (§2.2) | `assigned` (kto, komu) |
| **Zmień osobę** | PM albo lider przestawia pracę (przeciążenie, priorytety) | jak wyżej; poprzednia osoba dostaje powiadomienie | `reassigned` (z kogo, na kogo, kto zmienił) |
| **Przekaż z komentarzem** | wykonawca oddaje pracę w toku (urlop, zmiana zadań) albo przejmuje ją od agenta | wykonawca sam albo PM; **komentarz dla przejmującego wymagany** | `handed_over` + komentarz przypięty na górze karty dla nowej osoby |

**Wspólne okno wyboru osoby** działa tak samo w zadaniu, module, pozycji roadmapy, alokacji, wyjątku,
obowiązku, sprawie PSIRT, jednostce i zastępstwie:
- pokazuje obciążenie w bieżącym tygodniu z alokacji (§14) oraz znaczniki „przeciążony” (> 100%)
  i „nieobecny do … → zastępuje …” (struktura, S3),
- na górze listy stawia podpowiedź: zastępcę modułu, zastępstwo z profilu albo osobę o najmniejszym
  obciążeniu z wymaganą funkcją,
- agentów pokazuje tylko tam, gdzie mogą być wykonawcą (zadanie, krok workflow, triage znalezisk).

**Przekazanie hurtowe** — ekran „Do przekazania” (`ORG_STRUCTURE_PLAN.md` §2.6) zbiera wszystko,
co trzyma osoba: zadania, role w modułach, kroki workflow, alokacje, obowiązki, wyjątki, sprawy
PSIRT, członkostwa i zastępstwa. Każda pozycja ma podpowiedź odbiorcy, a całość da się przekazać
jednym wyborem („Przekaż wszystko do…”) z notatką dla przejmujących. Ekran obsługuje trzy powody:

| Powód | Zakres | Skutek |
|---|---|---|
| **Odejście** | całe konto | przekazanie trwałe |
| **Nieobecność** | cała praca osoby | przekazanie czasowe: po powrocie praca wraca do osoby, chyba że przejmujący ją zamknął |
| **Usunięcie z projektu** | elementy jednego projektu | przekazanie trwałe w tym projekcie |

Wejścia do ekranu:
- Członkowie → „Usuń z projektu i przekaż pracę…”,
- Struktura → „Zakończ przypisanie i przekaż…”,
- Zasoby → „Przenieś alokacje osoby…”,
- profil → „Co przejmą zastępcy”.

**Akcje zbiorcze na liście zadań:** przypisz, przekaż, do sprintu, ustaw wersję, przenieś do projektu,
archiwizuj.

**Usuń kontra archiwizuj.** Zadanie z historią pracy (commit, czas, przejście) można tylko
zarchiwizować. Usunięcie jest dostępne wyłącznie dla pustych pomyłek. Każda z tych operacji daje
toast z „Cofnij”.

## 5. Planowanie: backlog, tablica, sprinty, daily, weekly

### 5.1 Tryb Kanban
Tablica z kolumnami ze statusów workflow, limity WIP (są w `tf-kanban.js`), swimlane (osoba /
priorytet / epik), filtry (wersja, etykieta, „moje”). Tygodniowy rytm zapewnia przegląd tygodniowy
(§5.4).

### 5.2 Tryb Scrum (opcjonalny per projekt)
- **Backlog** z priorytetyzacją drag&drop, szacunkami i gotowością („zdefiniowane” = spełnia
  kryteria wejścia).
- **Sprint:** nazwa, cel, daty, pojemność zespołu. Planowanie przeciąganiem z backlogu. Tablica
  pokazuje bieżący sprint. Zamknięcie przenosi niedokończone zadania do backlogu albo następnego
  sprintu i zapisuje to w historii.
- **Wykresy:** burndown sprintu i velocity z 3–5 ostatnich sprintów. Nic więcej.
- **Przegląd i retrospektywa:** notatki sprintu (co dowieźliśmy, co poprawić). Akcje z retro stają
  się zadaniami.

### 5.3 Daily
Widok „Daily” to tablica pogrupowana po osobach z trzema sekcjami, **wypełniana automatycznie
z historii** (bez pisania raportów):
- **od wczoraj:** przejścia, commity, MR, testy,
- **dziś w toku:** zadania w statusach „w realizacji”,
- **blokady:** flaga „zablokowane” z powodem, zadania długo stojące w statusie, pętle testów > progu.

Opcjonalnie krótki wpis tekstowy osoby. LLM (model lokalny) generuje 5-zdaniowe podsumowanie
zespołu na początek spotkania.

### 5.4 Weekly
Automatyczny raport tygodniowy (co poniedziałek, obowiązek z harmonogramu): dowiezione zadania,
przepływ (cycle time, zadania stojące), błędy i rundy ping-pongu, ryzyka dla najbliższej wersji,
stan bezpieczeństwa (jeśli moduł włączony). LLM streszcza, a PM zatwierdza i wysyła (czat
projektu / e-mail).

## 6. Wersje i wydania — co wychodzi w którym release

- **Wersja** (`fix version`): `2.4.0`, planowana data, stan (planowana / w stabilizacji /
  wydana / wspierana / poza wsparciem). Zadanie ma pole „wersja docelowa”; tablica i backlog
  filtrują po wersji.
- **Prawda z git, zamiar z zadań.** Zawartość wydania to zbiór zadań, których commity (klucz
  `NA-123` w gałęzi lub wiadomości commita) są między tagiem poprzednim a bieżącym. System
  porównuje to z polem „wersja docelowa” i pokazuje rozbieżności:
  - zadanie oznaczone na 2.4, a niescalone → „nie wejdzie”,
  - commit zadania bez wersji albo z inną wersją → „weszło bez planu”.
- **Kandydat do wydania:** tag `v2.4.0-rc.N` → automatyczne wdrożenie środowiska `test-2.4.0-rcN`
  → zadania wersji w statusie „Gotowe do testów wersji” dostają link do tego środowiska.
- **Wydanie:** tag `v2.4.0` + wszystkie zadania wersji w statusach końcowych + bramki (testy,
  bezpieczeństwo) → Release Manager zatwierdza → changelog i noty wydania z zatwierdzonych wpisów zadań (§16)
  → zadania przechodzą do „Wydane”.

## 7. Integracja git (GitLab i GitHub od pierwszej fazy git)

### 7.1 Wiązanie commitów i MR z zadaniem — oba sposoby, automat główny

**Automatycznie (główna droga):** system szuka klucza zadania (`NA-123`) w:
1. nazwie gałęzi (`feature/NA-123-logowanie`) — wtedy wszystkie commity i MR/PR tej gałęzi należą
   do zadania, nawet jeśli wiadomości commitów klucza nie mają,
2. tytule i opisie MR/PR,
3. wiadomości commita (`NA-123: poprawka walidacji`, może być kilka kluczy).

Klucz jest jednoznaczny, bo prefiks jest unikalny w instalacji. Nieistniejący klucz jest ignorowany
i odnotowany w logu integracji.

**Ręcznie (uzupełnienie):** w karcie zadania „Dowiąż” → wyszukiwarka commitów, gałęzi i MR/PR
z repozytoriów projektu (po tytule, SHA, autorze). Tak naprawia się przypadek „ktoś zapomniał
klucza”. Ręczne dowiązanie i odpięcie trafiają do historii z autorem.

**Żeby rzadko trzeba było robić to ręcznie:**
- przycisk **„Utwórz gałąź”** w karcie (przez API GitLab/GitHub) nadaje poprawną nazwę, a karta
  pokazuje polecenie `git switch` do skopiowania,
- **sprawdzenie w MR/PR:** status „brak klucza zadania” (ostrzeżenie; projekt może ustawić
  blokadę scalenia),
- opcjonalny hook `commit-msg` dla repozytorium, dopisujący klucz z nazwy gałęzi.

Nie wprowadzamy „smart commits” (zmiany statusu poleceniem w wiadomości commita). Status zmienia
workflow na podstawie zdarzeń MR/PR, więc nie da się go przestawić samym commitem z pominięciem
review.

### 7.2 Powiązanie użytkownika TentaFlow z kontem GitLab/GitHub

> **Miejsce w UI (decyzja 2026-09-29):** powiązania kont, zdjęcie, nieobecności, zastępstwa
> i preferencje powiadomień są w **globalnym profilu użytkownika TentaFlow** (Mój profil), a nie
> w Projektach — dotyczą całej platformy i korzystają z nich także inne aplikacje. — automat przez domenę + ręczne uzupełnienie

TentaFlow docelowo loguje przez **konto domenowe** (AD / Entra ID). To najlepszy klucz do
automatycznego wiązania, ale **tylko tam, gdzie GitLab i GitHub znają tę samą tożsamość
domenową**. Samo zalogowanie do TentaFlow nie mówi nic o tym, jakie konto ktoś ma w GitHubie.
Kolejność prób:

| # | Sposób | Kiedy działa | Wiarygodność |
|---|---|---|---|
| 1 | **Automat po tożsamości domenowej** | **GitLab self-managed z LDAP/Entra**: konto GitLab ma tożsamość zewnętrzną (DN / identyfikator Entra); API admina wyszukuje użytkownika po tej tożsamości i dopasowuje do konta domenowego w TentaFlow. (GitHub u nas **nie** jest podpięty do domeny, więc dla niego ten krok nie działa; gdyby kiedyś przeszedł na Enterprise z SAML/EMU, API organizacji zwraca powiązanie login ↔ UPN) | **zweryfikowane** (obie strony ufają temu samemu IdP) |
| 2 | Automat po e-mailu **zweryfikowanym po obu stronach** | e-mail domenowy w TentaFlow (z IdP) = zweryfikowany e-mail konta GitLab/GitHub (z API, nie z commita) | wysoka; dopasowanie zapisane jako „z e-maila”, admin widzi listę do przejrzenia |
| 3 | **„Połącz konto” (OAuth) przez użytkownika** | zawsze — jedyna droga dla prywatnych kont github.com bez SSO organizacji | zweryfikowane (użytkownik zalogował się u dostawcy) |
| 4 | **Ręczne mapowanie przez admina** | konta, których nie złapał automat ani OAuth (boty, konta serwisowe, zewnętrzni) | zweryfikowane przez admina, z wpisem w audycie |
| — | e-mail autora commita | nigdy do powiązania — tylko podpowiedź w UI | niezweryfikowane (autora commita można wpisać dowolnie) |

- **Przebieg automatu:** przy logowaniu użytkownika i cyklicznie (np. razem z synchronizacją
  katalogu). Powiązanie z automatu da się ręcznie odpiąć. Kolizja (dwa konta pasujące do jednej
  osoby albo odwrotnie) nie jest rozstrzygana automatycznie, tylko trafia na listę do decyzji admina.
- **Konto nieaktywne w domenie** (odejście z firmy) → powiązanie zawieszone. Zdarzenia z jego
  konta git dalej się rejestrują, ale bez przypisania do aktywnej osoby — sygnał dla admina
  do odebrania dostępu w GitLab/GitHub.
- **Decyzje** (rozdział obowiązków, „review zrobił ktoś inny niż autor”, kto scalił) opierają się
  na tożsamości z API dostawcy (autor MR/PR, reviewer, osoba scalająca) zmapowanej jednym
  z powyższych sposobów, **nigdy** na polu author w commicie.
- **Technicznie:**
  - GitLab jest dostawcą OIDC, więc „Połącz konto” może przejść przez istniejący klient OIDC
    (`auth/sso.rs`),
  - GitHub nie udostępnia OIDC do logowania użytkowników — potrzebny osobny przepływ OAuth
    (GitHub App z autoryzacją użytkownika),
  - **stan u nas (2026-09-29): GitLab jest podpięty do domeny, GitHub nie.** Dla GitLaba działa
    automat nr 1: token admina GitLab, wyszukanie po tożsamości zewnętrznej (LDAP DN / identyfikator
    Entra) zgodnej z kontem domenowym w TentaFlow. Dla GitHuba zostają krok 2 (e-mail
    zweryfikowany po obu stronach — działa tylko, gdy ktoś ma e-mail firmowy jako zweryfikowany
    w GitHubie), krok 3 („Połącz konto”, droga główna) i krok 4 (admin),
  - **przypomnienie „Połącz konto GitHub”:** osoba z funkcją Developer w projekcie z repozytorium
    GitHub dostaje baner w profilu i w karcie zadania, dopóki konta nie połączy. Bez powiązania
    jej MR/PR są widoczne, ale nie liczą się jako „review zrobione przez inną osobę”.

### 7.3 Połączenie projektu z repozytorium (integracja, nie osoba)

- **GitLab:** token grupy/projektu (bot) albo aplikacja OAuth instancji + webhook projektu
  (`X-Gitlab-Token`).
- **GitHub:** **GitHub App** zainstalowana w organizacji (webhooki podpisane `X-Hub-Signature-256`,
  uprawnienia per repozytorium, check runs, bez osobistego tokenu kogokolwiek). Rezerwą jest token
  dostępu.
- **Agenci** piszący kod mają **własną tożsamość w git** (konto bota na agenta albo tożsamość
  aplikacji) z dopiskiem `Co-authored-by` osoby zlecającej. W historii widać, że zmianę zrobił
  agent i kto go uruchomił.

### 7.4 Zdarzenia i automatyczne przejścia

- Webhooki z trybem odpytywania jako rezerwą: push, MR/PR (otwarty, zaktualizowany, scalony,
  zamknięty), review, pipeline, tag. Każde zdarzenie trafia na oś czasu zadań, których klucze
  zawiera.
- Automatyczne przejścia (konfigurowane w workflow jako „Zdarzenie”):
  - MR otwarty → Code review,
  - MR scalony → wdrożenie testowe → Testy,
  - tag rc → „Gotowe do testów wersji”.
- Status zwrotny: komentarz w MR/PR z linkiem do zadania i stanem kroków (testy, security),
  commit status / check run.

## 8. Środowiska testowe konkretnej wersji

**Środowisko** = szablon z repo (`.tentaflow/environment.yml`: obrazy, zmienne, dane startowe
syntetyczne, konta testowe per rola) + sterownik + reguła wyzwalania + czas życia.

| Reguła | Co powstaje | Dla kogo |
|---|---|---|
| MR otwarty (opcjonalnie) | `mr-123` — aplikacja z gałęzi MR, TTL 24 h | developer, reviewer, designer (podgląd UI) |
| scalenie do `main` | `dev` — zawsze najnowsza wersja, aktualizowana w miejscu | zespół |
| tag `vX.Y.Z-rc.N` | `test-X.Y.Z-rcN` — zamrożone, TTL do wydania | testerzy, agenci testujący, security |
| na żądanie | dowolny ref, TTL wybrany | każdy z `environments: W` |

**Sterowniki:**
- `docker-local` na węźle mesh — domyślny,
- Portainer,
- Jenkins (job z parametrami, adres wraca callbackiem),
- GitLab CI (`environment:` z `on_stop`),
- GitHub Actions (`workflow_dispatch`, adres przez callback).

Środowisko uruchamiane przechodzi tę samą bramkę zatwierdzania adresów co dzisiejsze cele
(`approval_status`, `host_allowlist_json`); skany aktywne modułu bezpieczeństwa dodatkowo przypinają
adresy IP celu (`PROJECT_STUDIO_SECURITY_PLAN.md` §6.1). Po wdrożeniu środowisko staje się celem testów projektu (dzisiejsze `environments`), więc
przypadki testowe i agenci testujący używają go bez zmian. Karta zadania w kroku „Testy” pokazuje
link do środowiska i konta testowe.

Build profile (`build_profiles.rs`) zostaje od testów jednostkowych. Wdrożenie wymaga tej samej
brakującej piaskownicy per przebieg, która dziś blokuje `auto_runs.rs:864` — to wspólna inwestycja
z modułem bezpieczeństwa (F4 tam).

## 9. Agenci w ścieżce

- **Agent jako wykonawca zadania (programista-automat):** w polu „Wykonawca” wybiera się
  człowieka **albo agenta**, np. do drobnych poprawek. Agent:
  - pracuje na własnej gałęzi z kluczem zadania i otwiera MR/PR,
  - przechodzi **tę samą ścieżkę** co programista: code review przez człowieka, testy (ludzie
    i agenci), bramki bezpieczeństwa,
  - uwagi z review i błędy z testów wracają do niego tak samo jak do programisty („ten sam
    wykonawca”),
  - po N nieudanych rundach (ustawienie, domyślnie 2) zadanie przechodzi do człowieka
    z podsumowaniem prób.

  Agent **nie może** sam zatwierdzić review własnego MR ani scalić go bez bramek.
- **Bezpieczeństwo agenta-wykonawcy:**
  - pracuje w piaskownicy przebiegu (`code_studio/sandbox.rs`, `git_broker.rs`), z tokenem git
    ograniczonym do **wypychania własnych gałęzi** (bez praw do gałęzi chronionych i do scalania),
  - treść zadania i komentarzy to dane niezaufane — polecenie „dodaj użytkownika admin” w opisie
    błędu nie jest instrukcją dla agenta, a każda jego zmiana i tak przechodzi review człowieka,
  - commit agenta zapisuje w stopce identyfikator przebiegu, model i jego wersję (ślad do analizy),
  - **wyłącznik:** administrator projektu jednym ruchem odbiera agentowi udział w projekcie; otwarte
    MR agenta dostają etykietę „wstrzymane”, a jego kroki wracają do człowieka z funkcją.
- **Uczestnik projektu typu „agent”:** `project_members` + `member_kind = user|agent` +
  `agent_id`. Agent dostaje funkcję (np. Tester) i może być wykonawcą kroku.
- **Kontrakt kroku agenta:** wejście = zadanie (opis, kryteria akceptacji, załączniki), środowisko
  z kontami, lista zmian w MR. Wyjście przez sink z powiązaniem ustawionym przez serwer
  (`generation.rs`) = werdykt `ok | błędy | nie_da_się_ocenić`, komentarz, załączniki (zrzuty,
  nagranie), propozycje nowych zadań-błędów. Werdykt `błędy` działa jak werdykt testera-człowieka.
- **Przykładowi agenci:**
  - **agent testujący** — Playwright po kryteriach akceptacji na środowisku rc; nagrywa film błędu,
  - **agent security** — sprawdza bramki uprawnień i wstrzyknięcia na środowisku, a przy MR robi
    przegląd diffu,
  - **agent review** — pierwszy przegląd kodu przed człowiekiem,
  - **agent analityk** — doprecyzowuje kryteria akceptacji z opisu PM.
- **Model:** wynika z konfiguracji agenta (domyślnie lokalny, dopuszczalni dostawcy zewnętrzni).
  Każdy werdykt zapisuje model i wersję, więc historia pokazuje, co ocenił agent, a co człowiek.
- **Granica:** agent nie zamyka zadania i nie wydaje wersji. Końcowe przejścia (Gotowe do wydania,
  Wydane) wymagają człowieka z odpowiednią funkcją.

## 10. Model danych (szkic, baza projektu)

Nowe tabele:
- `workflow_defs`, `workflow_versions` (JSON węzłów zgodny z blokami flow),
- `task_types`, `task_fields`, `task_field_values`,
- `task_events` (historia, indeks `(task_id, created_at)`),
- `task_links`, `task_git_refs` (commit/branch/MR/PR),
- `task_step_reviews` (werdykty równoległe: kto/agent, model, runda, wynik),
- `sla_policies` (terminy per waga, podstawa czasu, jednostka, start, statusy pauzy/celu, progi), `work_calendars` + `calendar_days_off` (święta ustawowe z datą obowiązywania, dni wolne projektu), `task_sla_clock` (start, pauzy, termin, naruszenie),
- `task_occurrences` (kolejne wystąpienia błędu zgłoszonego przez automat),
- `user_forge_identities` (użytkownik ↔ konto GitLab/GitHub: dostawca, id, login, sposób weryfikacji) — **w bazie głównej**, bo tożsamość jest wspólna dla projektów,
- `sprints`, `sprint_tasks`, `versions`,
- `plan_proposals`, `proposal_requirements` (cytat, adres źródła, siła, stan przeglądu), `proposal_questions`, `proposal_items` (roadmapa/zadania/zależności z odnośnikami do wymagań), `proposal_chunks` (postęp etapów do wznawiania: `proposal_id`, `flow_run_id`, numer fragmentu i etap — unikalne razem; w projekcie może trwać tylko jedna analiza naraz dla danego zestawu dokumentów), `requirement_links` (po zatwierdzeniu: wymaganie → zadanie/pozycja),
- `modules` (zakładane ręcznie: osoby odpowiedzialne, zastępcy, nazwa dla użytkownika, rodzic), `module_paths` (opcjonalne, tylko do próśb o review), `task_modules`,
- `roadmap_streams`, `roadmap_items` (termin: daty / miesiąc / kwartał, postęp ręczny opcjonalnie), `roadmap_milestones` (data planowana i faktyczna), `dependencies` (wspólna dla pozycji, zadań i kamieni milowych: typ, opóźnienie), `roadmap_item_links` (zadania, epiki), `roadmap_baselines` + `roadmap_baseline_items` (nazwane, niezmienne wersje planu z datami i alokacjami), `schedule_shifts` (przesunięcia z kategorią przyczyny i uzasadnieniem),
- `changelog_entries` (wpis per zadanie: szkic LLM / ręczny, zatwierdzenie), `changelogs` (per wersja albo zakres wersji, wariant, język, stan, zatwierdzający, odnośniki zdanie → zadania),
- `env_templates`, `env_instances`.

Zmiany w istniejących:
- `tasks`: `status` bez CHECK, wskazuje status workflow; + `key`, `type_id`, `workflow_version`, `planned_start`, `planned_end`, `actual_start`, `actual_end` (z przejść), `weight`, `sla_policy_id`, `source` (człowiek/automat),
  `assignee_kind` (user/agent),
  `sprint_id`, `fix_version_id`, `parent_id`, `estimate`, `test_round`, `blocked_reason`,
- `project_members`: `functions` (JSON), `is_admin`, `member_kind`, `agent_id`, `expires_at`.

Baza główna (`projekty.db`): drzewo projektów (`projects.parent_id`, `path`, `depth`, `key_prefix`, `inherit_json`, `is_private`, `lifecycle` (active/ended), `ended_at`), `task_index` (zasilany z `index_outbox` w bazie projektu), `task_key_aliases`; macierz domyślna funkcji w szablonach projektów; `resource_allocations` (osoba albo funkcja × projekt × pozycja × przedział × ilość), opcjonalnie `time_entries` (ewidencja czasu).

## 11. Ekrany (mockupy `mockups/projekty-praca-RRRRMMDD/`)

| ID | Ekran |
|---|---|
| W01 | Tablica (kolumny ze statusów, swimlane, filtry sprint/wersja, karty z awatarem osoby lub agenta, licznik rund testów) |
| W02 | Karta zadania (pełny ekran): opis z obrazami, film, pola, powiązania, git, **oś czasu historii** z rundami testów i werdyktami |
| W03 | Backlog i planowanie sprintu |
| W04 | Daily (tablica per osoba, generowana z historii) |
| W05 | Wersje i wydanie (zawartość z git vs plan, rozbieżności, środowisko rc, noty) |
| W06 | Edytor workflow (płótno Flow Buildera, paleta, walidacja, import/eksport BPMN) |
| W07 | Członkowie i funkcje (osoby i agenci, macierz uprawnień funkcji) |
| W08 | Środowiska (szablony, instancje per MR/rc, TTL, sterowniki) |
| W09 | Raport tygodniowy / przegląd sprintu |
| W10 | Moduły: ręczne zakładanie, osoby odpowiedzialne i zastępcy, opcjonalne ścieżki, zdrowie modułu |
| W11 | Roadmapa: oś czasu (strumienie prac, kamienie milowe, zależności, plan bazowy), Teraz/Następnie/Później, przykład projektu wdrożeniowego bez git |
| W13 | Ustawienia projektu: zestaw funkcji (przełączniki, zależności, szablony startowe) |
| W18 | Roadmapa: plan vs realizacja (dwa paski, odchylenia, porównanie wersji planu bazowego), ścieżka krytyczna, propozycja przeplanowania po przesunięciu, raport przesunięć z przyczynami |
| W14 | Roadmapa z alokacjami: osoby i zapotrzebowanie na funkcje pod pozycjami, obciążenie tygodniowe z innymi projektami, konflikty |
| W15 | Zasoby (organizacja): osoby × tygodnie z utylizacją, niepokryte zapotrzebowanie na funkcje, nieobecności |
| W16 | Przygotuj plan z dokumentów: wybór dokumentów, postęp etapów |
| W17 | Propozycja planu: wymagania z cytatami, pytania do zamawiającego, roadmapa i zadania do przyjęcia, macierz pokrycia, różnica po ponownej analizie |
| W19 | Podprojekty: przełącznik drzewa w nagłówku, zakres „Z podprojektami” w widokach Zadań, tablica podprojektu z własnym workflow, Ustawienia → Podprojekty (drzewo, dziedziczenie per obszar, zakładanie z szablonu) — mockupy A06, A07, D09, okna D10 (nowy podprojekt) i D11 (zakończenie) |
| W12 | Changelog wersji: szkic LLM z odnośnikami do zadań, lista braków, warianty, języki, zatwierdzenie, przyciski kopiowania |

## 12. Moduły aplikacji

### 12.1 Czym jest moduł

Moduł to **większy blok** produktu, zakładany ręcznie przez PM/managera projektu. Użytkownik i zespół
rozpoznają go po nazwie, np. dla
NextApp: „Uprawnienia i hasła”, „OMS / widok szafy”, „Import OPC”, „Procedury zmiany w szafie”,
„Integracja SDP”, „Dashboard EMI”, „Synchronizacja AD/Entra”. Pola:

| Pole | Po co |
|---|---|
| nazwa, opis, ikona/kolor | rozpoznawalność na tablicy, roadmapie i w changelogu |
| **główny developer** (1+) i **zastępca** | domyślny wykonawca/triager zadań modułu, recenzent zmian w module |
| **główny tester** (1+) i **zastępca** | domyślny wykonawca kroku „Testy” dla zadań modułu |
| opcjonalnie: security, designer, PM modułu | dla projektów, które tego potrzebują |
| ścieżki w repozytoriach (opcjonalnie, wpisywane ręcznie, np. `NextApp/Modules/Permissions/**`) | **wyłącznie** prośba o review do głównego developera modułu przy MR zmieniającym te ścieżki; bez ścieżek moduł działa tak samo |
| nazwa dla użytkownika końcowego (opcjonalnie inna) | nagłówki w changelogu („Widok szafy” zamiast „omsView”) |
| moduł nadrzędny | hierarchia 2 poziomów — więcej nie potrzeba |

Osoby odpowiedzialne to **główne**, a nie wyłączne: każdy członek z odpowiednią funkcją może pracować
w każdym module. Zastępca przejmuje przydziały, gdy główna osoba ma oznaczoną nieobecność.

### 12.2 Co robi moduł w praktyce

- **Zadanie ma pole „Moduł”** (jeden lub kilka), wybierane **ręcznie** przez zgłaszającego albo
  osobę weryfikującą zgłoszenie. Brak podpowiedzi LLM i automatycznej korekty z commitów.
  Moduł można wymusić jako pole wymagane w przejściu workflow (np. przy wyjściu z „Do weryfikacji”).
- **Przydział w workflow:** krok ludzki może mieć regułę „główny developer modułu” / „główny tester
  modułu” (§3.1). Błąd w module „Import OPC” trafia od razu do właściwej osoby, a nie do ogólnej
  kolejki.
- **Review (tylko jeśli wpisano ścieżki):** MR/PR zmieniający ścieżki modułu dostaje prośbę o review
  od głównego developera modułu. Domyślnie to **prośba, nie bramka**. Workflow może zrobić z niej
  bramkę: warunek przejścia „Code review → dalej” = zatwierdzenie MR/PR przez głównego developera
  modułu (odczyt zatwierdzeń z API GitLab/GitHub). Nie generujemy pliku `CODEOWNERS` —
  repozytorium zostaje pod kontrolą zespołu.
- **Zdrowie modułu:** otwarte błędy wg wagi, przekroczenia SLA, liczba rund ping-pongu, zmiany
  w ostatniej wersji. Moduł z dużą liczbą rund testów to sygnał dla PM (dług, słabe testy).
- **Filtr i swimlane** na tablicy po module; **grupowanie** w changelogu po module.
- **Obciążenie osób odpowiedzialnych:** widok „ile otwartych zadań ma główny developer/tester
  w swoich modułach”, żeby przypisanie „głównej osoby” nie tworzyło wąskiego gardła.

## 13. Roadmapa

Funkcja projektu włączana przełącznikiem (§2.5). Służy PM-om do planowania zarówno rozwoju produktu,
jak i przedsięwzięć **nieprogramistycznych** (wdrożenie u klienta, migracja, szkolenia, zakup
i instalacja sprzętu). Nie wymaga ani wersji, ani git, ani sprintów — korzysta z nich tylko wtedy,
gdy projekt je ma.

### 13.1 Elementy

| Element | Pola |
|---|---|
| **Pozycja** (inicjatywa / etap / prace) | tytuł, cel (dlaczego), właściciel, status (`pomysł` → `planowane` → `w realizacji` → `dostarczone` / `wstrzymane` / `porzucone`), **termin**, **strumień prac** (wiersz na osi), zależności, powiązane zadania i epiki, opcjonalnie moduł i wersja docelowa |
| **Strumień prac** (wiersz osi czasu) | np. „Infrastruktura”, „Migracja danych”, „Szkolenia”, „Integracje”. W projektach programistycznych można używać modułów jako strumieni |
| **Kamień milowy** | data, nazwa, kryterium osiągnięcia (np. „odbiór środowiska testowego”), powiązane pozycje; stan: osiągnięty / zagrożony / przekroczony |
| **Zależność** | między pozycjami, zadaniami i kamieniami milowymi (także mieszanie: zadanie → kamień milowy); typy i równoległość w §13.6 |

**Termin** ma dokładność wybraną per projekt: **daty** (początek–koniec, jak w harmonogramie
wdrożenia), **miesiąc** albo **kwartał** (produktowe „kiedyś w Q4”). Projekt wdrożeniowy zwykle
pracuje na datach, a produktowy na kwartałach.

**Postęp** pozycji liczy się z powiązanych zadań (zamknięte / wszystkie, ważone szacunkiem). Gdy
pozycja nie ma zadań (typowe w planie wdrożenia na wczesnym etapie), postęp jest **ręczny** (procent
+ komentarz), z historią zmian. **Ryzyko** jest wyliczane: koniec po terminie kamienia milowego,
poprzednik opóźniony, zadania niezamknięte przy zbliżającym się końcu, przesunięta wersja docelowa.

### 13.2 Widoki

- **Oś czasu** (Gantt w wersji lekkiej): wiersze = strumienie prac, pasek = pozycja, romb = kamień
  milowy, linie zależności, znacznik „dziś”. Przeciąganie zmienia termin, a zmiana terminu wymaga
  uzasadnienia i trafia do historii pozycji (widać, ile razy coś się przesuwało).
- **Teraz / Następnie / Później** — tablica bez dat, na rozmowy, zanim daty są pewne.
- **Kamienie milowe** — lista z terminami i stanem.
- **Per wersja** — tylko w projektach z wersjami: co z roadmapy ma wejść w 2.5.
- **Plan i realizacja** na jednej osi — §13.5.

### 13.3 Od roadmapy do zadań

Pozycja może pozostać „tylko planem” albo zostać rozpisana na zadania (w projektach z zadaniami).
LLM może **na prośbę PM-a** zaproponować rozbicie pozycji na zadania albo podpozycje. PM je poprawia
i zatwierdza, a nic nie powstaje bez jego decyzji.

### 13.4 Udostępnianie

Roadmapa jest częścią Projektów i widzą ją członkowie projektu wg uprawnień (§2.2). Na zewnątrz nie
ma osobnego portalu. Widok można **skopiować jako tabelę lub obraz** albo wydrukować do PDF, żeby
wkleić go do prezentacji czy maila do klienta.

### 13.5 Roadmapa planowa i realizacyjna

W praktyce plan się przesuwa, więc roadmapa ma **dwie warstwy**, a różnica między nimi jest
informacją, a nie błędem.

| Warstwa | Skąd dane | Kto zmienia |
|---|---|---|
| **Plan** (planowa) | **plan bazowy** zatwierdzony przez PM (np. harmonogram zaakceptowany przez klienta); nazwany, datowany i **niezmienny po zapisaniu** (edytuje się plan bieżący, a zapis tworzy nową wersję bazową — nie ma równoległej edycji bazy) | tylko PM, przez zapisanie **nowej wersji planu bazowego** („Plan v2 — po aneksie nr 1”); poprzednie wersje zostają |
| **Realizacja** (realizacyjna) | faktyczne daty: start = pierwsze wejście zadań pozycji w status „w realizacji”, koniec = ostatnie zamknięcie; dla pozycji bez zadań — ręcznie; **prognoza** końca dla pozycji w toku (z postępu, pozostałych szacunków i alokacji) | liczona automatycznie, z ręczną korektą prognozy przez PM (z uzasadnieniem) |

**Na osi czasu** każda pozycja ma dwa paski: plan (tło, obrys) i realizację/prognozę (wypełniony),
z oznaczeniem odchylenia w dniach (roboczych albo kalendarzowych, wg kalendarza projektu).
Kamienie milowe mają datę planowaną i faktyczną. Przełącznik „porównaj z planem v1 / v2 / bieżącym”
pokazuje, jak plan ewoluował.

**Przyczyny przesunięć:** każde przesunięcie ponad próg (np. 2 dni) wymaga wpisu z **kategorią**:
- po stronie klienta / zamawiającego (brak danych, późny odbiór, zmiana wymagań),
- po stronie wykonawcy (zespół, jakość, niedoszacowanie),
- zewnętrzna (dostawca, sprzęt, prawo),
- zmiana zakresu (aneks).

Raport przesunięć (z kategoriami, datami i powiązanymi zadaniami) ma znaczenie w projektach z karami
umownymi — dokumentuje, po czyjej stronie leżało opóźnienie.

**Raporty:** odchylenie od planu per strumień i kamień milowy, lista przesunięć z przyczynami,
„co się zmieniło od ostatniego planu bazowego”. Każdy raport da się skopiować (§16.4).

### 13.6 Zależności, równoległość, ścieżka krytyczna

- **Zależności na pozycjach roadmapy i na zadaniach**, w czterech typach znanych z harmonogramów:
  - **koniec → start** (domyślny: B zaczyna się po zakończeniu A),
  - **start → start** (B może ruszyć, gdy A ruszy),
  - **koniec → koniec** (B nie skończy się przed A),
  - **start → koniec** (rzadki, dla zmian dyżurów).

  Każda zależność ma opcjonalne **opóźnienie/wyprzedzenie** (np. +2 dni robocze na odbiór).
- **Równoległość** wynika z braku zależności: zadania i pozycje bez powiązania mogą iść równolegle.
  Ograniczają je tylko **zasoby** (§14), więc konflikt „dwie równoległe pozycje, jedna osoba na 100%”
  pokazuje się jako konflikt alokacji, a nie zależności.
- **Zadania mają (opcjonalnie) daty planowane** start/koniec i szacunek, a realizacja liczy się
  z przejść workflow. Tablica kanban i oś czasu pokazują te same zadania: kanban według statusu,
  oś według dat i zależności.
- **Na tablicy:** zadanie z niezakończonym poprzednikiem koniec → start ma oznaczenie „zablokowane
  przez NA-120”. Przesunięcie go do „W realizacji” daje ostrzeżenie; projekt może ustawić to jako
  blokadę w przejściu workflow.
- **Ścieżka krytyczna** liczona z zależności, szacunków i kalendarza roboczego jest podświetlona
  na osi. Opóźnienie na ścieżce krytycznej przesuwa prognozę kamienia milowego od razu.
- **Planowanie w przód** (propozycja, nie automat): po przesunięciu poprzednika system **proponuje**
  nowe daty następników zgodnie z zależnościami i kalendarzem, a PM zatwierdza całość albo część.
  Plan zmienia się tylko po decyzji PM; realizacja i prognoza liczą się same.
- **Walidacja:** brak cykli, ostrzeżenie o zależności między projektami (dozwolona, ale widoczna
  po obu stronach jako ryzyko zewnętrzne).

## 14. Planowanie zasobów i utylizacja

Dla PM-a oś czasu bez ludzi to tylko połowa planu. Roadmapa (§13) dostaje **alokacje**, a organizacja
dostaje widok **utylizacji** przez wszystkie projekty.

### 14.1 Pojęcia

| Pojęcie | Znaczenie |
|---|---|
| **Dostępność** osoby | wymiar pracy (np. 1,0 etatu = 8 h/dzień, 0,5 etatu), dni i godziny pracy z kalendarza roboczego (§4.2.1), czasowe zmiany (np. od marca 0,8 etatu) |
| **Nieobecność** | urlop, szkolenie, inna nieobecność — przedział dat. **Powód widzi tylko sama osoba i uprawniony przełożony**; pozostali widzą „niedostępny”, bo powód bywa daną wrażliwą (zwolnienie lekarskie to dane o zdrowiu — RODO) |
| **Alokacja** | osoba × pozycja roadmapy (albo projekt jako całość) × przedział dat × **ile**: godziny na tydzień, procent etatu albo łączna liczba godzin rozłożona równomiernie |
| **Zapotrzebowanie bez osoby** | „potrzebny tester, 50% etatu, marzec–kwiecień” — alokacja na **funkcję**, zanim PM wie, kto to będzie. Tak planuje też propozycja z §15 |
| **Utylizacja** | suma alokacji osoby ÷ jej dostępność w danym okresie (tydzień / miesiąc), po wszystkich projektach |

### 14.2 Co widzi PM w projekcie

- **Na osi czasu roadmapy** pod każdą pozycją są osoby i funkcje z alokacją. Wiersz „zespół” pokazuje
  obciążenie tygodniowe każdej osoby **łącznie z innymi projektami** (inne projekty jako jeden
  zagregowany pasek „inne”, bez szczegółów, jeśli PM nie ma do nich dostępu).
- **Konflikty:** przekroczenie 100% dostępności, alokacja w czasie nieobecności, zapotrzebowanie
  bez osoby bliżej niż N tygodni przed startem — oznaczone na osi i w liście „Do rozwiązania”.
- **Przesunięcie pozycji** przesuwa jej alokacje (po potwierdzeniu), a konflikty przeliczają się
  od razu.
- **Plan bazowy** (§13.2) obejmuje też alokacje, więc widać odchylenie planu ludzi, nie tylko dat.

### 14.3 Utylizacja między projektami

- **Widok organizacji „Zasoby”** (poza pojedynczym projektem, w aplikacji Projekty): osoby × tygodnie,
  kolor = utylizacja (poniżej 50% / 50–100% / ponad 100%), rozwinięcie osoby → projekty i pozycje.
  Filtry: zespół/grupa, funkcja, projekt, przedział.
- **Zapotrzebowanie na funkcje:** suma niepokrytych alokacji („w maju brakuje 1,5 etatu testera
  we wszystkich projektach”) — to jest właściwe pytanie przy przyjmowaniu nowego projektu.
- **Uprawnienia do widoku utylizacji** wynikają ze **struktury organizacyjnej**
  (`ORG_STRUCTURE_PLAN.md` §4, §6), a nie z osobnej listy:
  - **PM** widzi pełne dane swoich projektów, a z innych projektów tylko zagregowane obciążenie
    osób, które alokuje,
  - **kierownik zespołu** widzi utylizację, nieobecności (bez powodu) i ewidencję czasu swojego
    **poddrzewa**,
  - **zarząd** widzi wszystko przez uprawnienie `project_studio.resources.view_all`,
  - każdy widzi swoje dane.

  Reguła jest jedna — `org.can_view_person_data` — i Projekty jej nie kopiują.
- **Wymiar pracy** pochodzi ze struktury (suma części etatu na stanowiskach). **Nieobecności**
  pochodzą ze struktury: wpis ręczny, a na końcu (opcjonalnie) import z **eDokumentów** jako
  dodatek (`ORG_STRUCTURE_PLAN.md` §5.1, faza S6) — nic w Projektach od niego nie zależy. Nieobecność osoby przenosi jej kroki workflow na zastępcę.

### 14.3a Ewidencja czasu (opcjonalna, per projekt)

Włączana w zestawie funkcji projektu (§2.5). Cel: widzieć, **co poszło szybciej, a co wolniej niż
zakładano**, i z czasem lepiej szacować.

- **Wpisy:** start/stop przy zadaniu (licznik w karcie i na tablicy), ręczny wpis z datą, tygodniowa
  karta czasu (osoba × dni × zadania) z możliwością poprawy do zamknięcia tygodnia.
- **Porównania:**
  - szacunek a wykonanie per zadanie, typ zadania, moduł i pozycja roadmapy,
  - utylizacja planowana a faktyczna (§14.3),
  - dokładność szacunków w czasie (czy zespół się poprawia).
- **Kalibracja:** propozycje szacunków z §15 mogą korzystać z historycznego stosunku „wykonanie
  / szacunek” dla podobnych zadań — jako podpowiedź, oznaczona źródłem.
- **Prywatność:** dane indywidualne widzi osoba, jej przełożeni i PM w zakresie projektu. Porównania
  „kto szybciej” między osobami **nie są domyślnym widokiem**. Raporty zespołowe pokazują agregaty,
  bo ewidencja ma służyć szacowaniu, a nie ocenie pracowników (w razie wątpliwości — konsultacja
  z HR / działem prawnym).
- **Bez ewidencji** utylizacja jest wyłącznie planowana i tak jest podpisana. Godzin nie zgadujemy
  z aktywności.

### 14.3b Dane osobowe w Projektach

Projekty deklarują swoje kategorie w usłudze prywatności platformy (`ORG_STRUCTURE_PLAN.md` §6.4):
członkowie, wykonawcy i autorzy zadań, komentarze i @wzmianki, ewidencja czasu, alokacje,
powiązania kont git, zgłoszenia automatów (zrzuty ekranu mogą zawierać dane z aplikacji!).
Załączniki z danymi osobowymi (zrzuty, nagrania) można oznaczyć, żeby polityka retencji objęła je
osobno. Wykonanie żądania usunięcia zostawia spójność zadań (pseudonim zamiast osoby).

### 14.4 Gdzie leżą dane

Alokacje, dostępność i nieobecności leżą w **centralnej bazie Projektów** (`projekty.db`), a nie
w bazach projektów. Utylizacja to zapytanie po wszystkich projektach naraz, a otwieranie kilkudziesięciu
baz projektów dla jednego widoku byłoby za drogie (pula LRU `project_db.rs`). Pozycja roadmapy
w bazie projektu wskazuje alokacje po identyfikatorze. Struktura, wymiar pracy, zastępstwa i nieobecności leżą w **strukturze organizacyjnej**
(`ORG_STRUCTURE_PLAN.md`). Projekty trzymają u siebie wyłącznie alokacje i ewidencję czasu.

## 15. Analiza dokumentów → propozycja planu projektu (LLM, Flow Builder)

### 15.1 Cel

PM zaznacza w zakładce **Wiedza** projektu (te same pliki i źródła, których używa Czat projektu) dokumenty (OPZ / SIWZ / specyfikację przetargową, wymagania klienta, umowę,
załączniki, notatki ze spotkań) i uruchamia „Przygotuj plan”. Dostaje **propozycję**:
- listę wymagań z cytatami i źródłami,
- pytania do zamawiającego,
- strumienie prac, pozycje roadmapy i kamienie milowe z terminami z dokumentów,
- epiki i zadania z kryteriami akceptacji i szacunkami,
- zapotrzebowanie na funkcje w czasie (§14.1),
- macierz pokrycia wymagań.

**Osób nie przydziela** — to decyzja PM-a.

### 15.2 Dlaczego flow z subagentami

Dokumenty bywają długie (setki stron) i jest ich wiele. Jedno wywołanie modelu nie pomieści
całości, a nawet gdyby pomieściło, gubiłoby szczegóły ze środka. Przetwarzanie idzie więc wzorem
map → reduce → plan → krytyk, na blokach, które Flow Builder już ma: `document_parse`,
`office_extract`, `ocr_pages`, `table_structure`, `chunk`, `map_block`, `spawn` /
`await_subagents`, `agent_block`, `critic_gate`, `project_knowledge`.

```text
Etap 1 — przygotowanie (bez LLM tam, gdzie się da)
  dokumenty → parsowanie (PDF/DOCX/XLSX, OCR dla skanów, tabele jako tabele)
            → podział po STRUKTURZE dokumentu (rozdziały i punkty „3.2.1”), nie po liczbie znaków
            → zapis fragmentów w wiedzy projektu (RAG) z adresem: dokument / strona / punkt

Etap 2 — ekstrakcja (map: subagent na paczkę fragmentów, równolegle)
  każdy subagent zwraca atomowe pozycje z DOSŁOWNYM cytatem i adresem źródła:
  - wymaganie (funkcjonalne / niefunkcjonalne / organizacyjne / prawne / bezpieczeństwa),
    siła: MUSI / POWINIEN / MOŻE (z języka dokumentu: „Wykonawca zapewni…”, „dopuszcza się…”)
  - termin / kamień milowy / produkt do dostarczenia (raport, szkolenie, dokumentacja)
  - warunek formalny (SLA wymagane od wykonawcy, kary umowne, gwarancja, odbiory)
  - niejasność / sprzeczność → kandydat na pytanie do zamawiającego

Etap 3 — scalanie (reduce)
  deduplikacja wymagań z wielu dokumentów, wykrycie sprzeczności między dokumentami,
  słownik pojęć, lista pytań do zamawiającego z odnośnikami do źródeł

Etap 4 — planowanie (agent planujący, może dzielić pracę na subagentów per strumień)
  strumienie prac → pozycje roadmapy → kamienie milowe (terminy z etapu 2)
  → epiki → zadania (kryteria akceptacji z wymagań, szacunek jako PRZEDZIAŁ, potrzebna funkcja)
  → zależności → zapotrzebowanie na funkcje w czasie

Etap 5 — krytyk (critic_gate, pętla z limitem rund; wzór: agent „Krytyk wymagań” z generation.rs)
  - każde wymaganie MUSI pokryte co najmniej jednym zadaniem albo jawnie oznaczone „poza zakresem” z powodem
  - każde zadanie wskazuje wymaganie źródłowe albo jest oznaczone jako ZAŁOŻENIE
  - kamienie milowe nie przed terminami z dokumentów, zależności bez cykli
  - braki wracają do etapu 4 z listą usterek
```

### 15.3 Odporność i koszt

- **Każdy etap zapisuje wynik w bazie projektu** (tabele propozycji), a nie tylko w pamięci
  przebiegu. Silnik flow nie wznawia przebiegów po restarcie (`run_manager.rs:17`), więc ponowne
  uruchomienie **pomija gotowe fragmenty i etapy**. Długa analiza przerwana restartem nie zaczyna
  się od zera. Po restarcie przebieg jest uzgadniany jak w `generation.rs::reconcile_running`.
- **Postęp widoczny w UI:** etap, liczba przetworzonych fragmentów, liczba wymagań.
- **Równoległość i model:** liczba subagentów i model per etap to parametry flow. Domyślnie modele
  lokalne; wybór jest w konfiguracji agentów (§9). Ekstrakcja może iść mniejszym modelem,
  a planowanie i krytyk — większym.
- **Dane niezaufane:** treść dokumentów to dane (`code_assist.rs`), a nie polecenia. Zapis idzie
  wyłącznie przez sink z powiązaniem ustawionym przez serwer (`generation.rs`), więc dokument
  z „instrukcją” nie zmieni celu zapisu ani nie przypisze nikogo.

### 15.4 Przegląd i zatwierdzenie przez PM

Wynik nie trafia od razu do projektu. Ekran „Propozycja planu”:
- **Wymagania:** lista z cytatem i odnośnikiem (klik otwiera dokument na właściwej stronie),
  przyjmij / popraw / odrzuć.
- **Pytania do zamawiającego:** lista do skopiowania (np. do pisma z pytaniami w przetargu).
- **Roadmapa i zadania:** podgląd na osi czasu. Zaznaczasz, co przyjąć (wszystko, strumień,
  pojedyncze pozycje), i poprawiasz przed zatwierdzeniem.
- **Macierz pokrycia** wymaganie → zadania, eksportowalna (typowy załącznik ofertowy „zgodność
  z OPZ”).
- **Zatwierdzenie** zakłada pozycje roadmapy, kamienie milowe, epiki i zadania oraz
  **zapotrzebowanie na funkcje bez osób**. Każdy utworzony element zachowuje odnośnik do wymagań
  i cytatów. PM przydziela ludzi w §14.
- **Ponowna analiza** po zmianie dokumentów (np. odpowiedzi zamawiającego na pytania) daje
  **różnicę** względem zatwierdzonego planu (nowe / zmienione / usunięte wymagania i ich wpływ
  na zadania), a nie nowy plan od zera.
- **Szacunki LLM są oznaczone jako wstępne** i widoczne jako przedziały. Po zatwierdzeniu zespół
  je nadpisuje, a system zapamiętuje, skąd pochodziła wartość.

## 16. Changelog dla użytkownika końcowego

### 16.1 Zasada

Zawartość wersji wynika z §6: zadania, których commity weszły między tagami. Changelog opisuje te
zadania **językiem użytkownika** — co może teraz zrobić, co działa lepiej, co naprawiono — a nie
językiem programisty („refaktoryzacja `OmsTopView`”). Szkic pisze LLM (domyślnie model lokalny),
a Release Manager albo PM go zatwierdza.

### 16.2 Kiedy powstaje tekst

1. **Przy zadaniu, nie na końcu wersji.** Gdy zadanie przechodzi do „Gotowe do wydania”, LLM
   pisze **wpis changelogu zadania** (1–2 zdania dla użytkownika) na podstawie tytułu, opisu,
   kryteriów akceptacji, modułu, komentarzy i opisów MR. Autor albo tester go poprawia, póki
   pamięta szczegóły. Pole „Opis dla użytkownika” napisane ręcznie ma zawsze pierwszeństwo.
2. **Przy wersji:** z wpisów zadań powstaje changelog wersji. LLM grupuje, łączy wpisy o tej samej
   funkcji, porządkuje od najważniejszych, dodaje krótkie wprowadzenie („W tej wersji…”).
3. **Zatwierdzenie** jest częścią checklisty wydania (§6) i nie wymaga do tego osobnego procesu.

### 16.3 Co trafia, a co nie

| Źródło | W changelogu użytkownika |
|---|---|
| Funkcjonalność | „Nowości” albo „Ulepszenia”, pogrupowane po module (nazwa dla użytkownika) |
| Błąd (każda waga) | „Poprawione błędy” — opis objawu, który użytkownik mógł widzieć, nie przyczyny |
| Zadanie techniczne | **pomijane**, chyba że ma zauważalny skutek (np. szybsze otwieranie) — wtedy „Ulepszenia” |
| Zadanie bezpieczeństwa / poufne | **nigdy szczegółów**; jedna linia „Poprawki bezpieczeństwa” z odnośnikiem do opublikowanego advisory (moduł bezpieczeństwa) |
| Zmiana wymagająca działania (migracja, zmiana konfiguracji, zmiana zachowania) | osobna sekcja **„Wymaga uwagi”** na górze, także w changelogu użytkownika, jeśli dotyczy jego pracy |
| Zadanie z flagą „nie pokazuj w changelogu” | pomijane |

### 16.4 Warianty i kopiowanie

Changelog żyje **wyłącznie w Projektach** (zakładka wersji). Nie jest publikowany automatycznie
w aplikacjach ani na stronach — służy do skopiowania i wklejenia tam, gdzie trzeba.

- **Warianty:**
  - **dla użytkownika końcowego** (bez numerów zadań),
  - **dla administratora / wdrożeniowca** (dodatkowo migracje, zmiany konfiguracji, znane problemy,
    odnośniki do SBOM i advisory),
  - **wewnętrzny** (pełna lista zadań z kluczami i MR).
- **Przyciski kopiowania** (każdy wariant, każdy język):
  - „Kopiuj jako tekst” — zwykły tekst z punktorami, do maili i komunikatorów,
  - „Kopiuj jako Markdown” — do README, GitLaba, GitHuba, wiki,
  - „Kopiuj sformatowany” — HTML w schowku, wkleja się z nagłówkami i listami do Worda i Outlooka,
  - „Pobierz” — `.md` / `.html` / `.txt`.
- **Języki:** szkic w języku projektu. Tłumaczenie na inne języki na żądanie (LLM), każde
  zatwierdzane osobno.

### 16.5 Changelog między dowolnymi wersjami

Klient przechodzący z 2.3.0 na 2.5.1 dostaje **jeden** changelog: suma zatwierdzonych changelogów
wersji pośrednich. Wpisy o tej samej funkcji się łączą, a błąd wprowadzony i naprawiony w wersjach
pośrednich znika, bo klient go nigdy nie widział. Wynik jest generowany na żądanie i też wymaga
zatwierdzenia, zanim trafi do klienta.

### 16.6 Zabezpieczenia przed zmyślaniem

- Każde zdanie szkicu ma **ukryte odnośniki do zadań**, z których wynika. Walidator odrzuca zdanie
  bez odnośnika i zgłasza zadanie „widoczne dla użytkownika” bez żadnego zdania. Zatwierdzający widzi
  obie listy.
- LLM dostaje wyłącznie dane zadań wersji jako **dane niezaufane**, wynik idzie przez sink
  (`generation.rs`). Treść zadań poufnych w ogóle nie trafia do promptu.
- **Ugruntowanie, nie tylko odnośnik:** przy każdym zdaniu zatwierdzający widzi fragmenty zadań
  (tytuł, kryteria, komentarz), na których się opiera. Walidator sprawdza, czy nazwy funkcji,
  ekranów i liczby ze zdania występują w danych zadań; zdanie z twierdzeniem bez pokrycia jest
  oznaczone. Zdania o **bezpieczeństwie, utracie danych, wydajności (liczby)** i zmianach
  wymagających działania **zawsze** wymagają ręcznego potwierdzenia, także przy zatwierdzaniu
  całości.
- Zatwierdzony changelog jest niezmienny i wersjonowany. Poprawka po publikacji to nowa wersja
  z historią.

## 17. Fazy

| Faza | Zakres |
|---|---|
| **P0 — uprawnienia** | funkcje członków, macierz obszarów, egzekwowanie na serwerze, migracja ról |
| **P1 — zadanie i historia** | klucze zadań, typy, `task_events` i oś czasu, powiązania, załączniki wideo, @wzmianki i powiadomienia o przejściach |
| **P1b — podprojekty** | drzewo w `projekty.db`, dziedziczenie członków i podprojekt prywatny, zakończenie i wznowienie podprojektu, prefiksy kluczy i aliasy, indeks centralny z outboxem, przełącznik w nagłówku, zakres „Z podprojektami” w Liście i Tablicy; dziedziczenie workflow/SLA dochodzi w P2, wersja u klienta w P4 |
| **P2 — workflow** | definicje z edytorem graficznym, przydział do funkcji, decyzje, bramki równoległe, tablica ze statusów, szablony startowe, import/eksport BPMN |
| **P3 — git** | GitLab + GitHub: webhooki, klucze w gałęziach i commitach, automatyczne przejścia, statusy zwrotne; powiązanie kont (GitLab automatycznie przez domenę, GitHub „Połącz konto” — rejestracja GitHub App wymaga czasu po stronie organizacji GitHub, zacząć wcześniej); MR/PR osób niepowiązanych pokazują się jako „niepowiązane konto” |
| **P4 — wersje, sprinty, moduły** | wersje, zawartość z git vs plan, sprinty, backlog, daily, weekly, moduły z osobami odpowiedzialnymi i mapowaniem ścieżek |
| **P4b — roadmapa i changelog** | zestaw funkcji projektu i szablony (roadmapa działa też bez zadań i git), roadmapa (daty/miesiące/kwartały, kamienie milowe), plan bazowy z wersjami i warstwa realizacji z przyczynami przesunięć, zależności 4 typów na pozycjach i zadaniach, ścieżka krytyczna, propozycja przeplanowania, wpisy changelogu przy zadaniach, changelog wersji i między wersjami, warianty, kopiowanie, tłumaczenia |
| **P4c — zasoby i plan z dokumentów** | alokacje, dostępność, nieobecności, konflikty na osi, widok Zasoby z utylizacją między projektami; flow „Przygotuj plan” (etapy 1–5, wznawianie), ekran propozycji, macierz pokrycia, ponowna analiza jako różnica |
| **P5 — środowiska i agenci** | piaskownica per przebieg, szablony środowisk i sterowniki, agenci jako uczestnicy kroków |

Moduł bezpieczeństwa (`PROJECT_STUDIO_SECURITY_PLAN.md`) wpina się od P3 (webhooki, bramki MR)
i P5 (środowiska, agent security).

### 17.1 Zależności między planami

| Funkcja w tym planie | Wymaga | Do tego czasu |
|---|---|---|
| Eskalacje SLA ponad PM (§4.2) | struktura S3 (`org.escalation_chain`) | eskalacja PM → administrator projektu |
| Widoczność utylizacji dla kierowników (§14.3) | struktura S0 (linie podległości) + S3 (`org.can_view_person_data`) | utylizację widzą PM (swoje projekty) i zarząd |
| Wymiar pracy i nieobecności (§14) | struktura S0 (przypisania z `share`) + S3 (nieobecności) | każdy ma wymiar 1,0 etatu, a nieobecności nie są uwzględniane |
| Przydział na zastępcę przy nieobecności (§9, §4) | struktura S3 | krok zostaje u osoby, PM dostaje powiadomienie |
| Środowiska uruchamiane (§8), agenci-wykonawcy (§9) | piaskownica per przebieg (wspólna z modułem bezpieczeństwa) | tylko środowiska ze stałym adresem |

Zalecana kolejność łączona: **P0 → S0 → P1 → P1b → P2 → S1/S2 → P3 → S3 → P4/P4b → P4c → P5**.
Struktura S0 (same tabele i reguły, bez UI) jest tania i odblokowuje najwięcej.

## 18. Otwarte pytania

1. Szacowanie: punkty czy godziny (czy oba jako ustawienie projektu)?
2. Czy klient/obserwator zewnętrzny ma mieć dostęp do tablicy (portal), czy tylko do raportów?
3. ~~Limit wielkości wideo~~ — **rozstrzygnięte: bez limitów.** Otwarta tylko retencja (domyślnie nic nie kasujemy).
6. ~~SLA kalendarzowe czy robocze~~ — **rozstrzygnięte:** oba, jako definiowalne polityki (§4.2). Rozstrzygnięte też: „24 h robocze” = 24 h pracy (3 × 8 h). Otwarte: domyślne godziny pracy (przyjęto 8:00–16:00).
7. ~~SLA per klient~~ — **rozstrzygnięte: nie.**
8. ~~Domena a git~~ — **rozstrzygnięte:** GitLab podpięty do domeny (automat), GitHub nie (OAuth „Połącz konto” + admin).
4. ~~Zadania w wielu projektach naraz~~ — **rozstrzygnięte przez podprojekty (§1a):** zadanie żyje w jednym węźle, a przodkowie widzą je w widokach zbiorczych. Między gałęziami wystarczą powiązania.
5. Czy workflow ma być wspólny dla organizacji (szablon narzucony), czy każdy projekt edytuje swój?
9. ~~Okno „Co nowego”~~ — **rozstrzygnięte:** changelog tylko w Projektach, do kopiowania.
10. ~~Roadmapa zewnętrzna~~ — **rozstrzygnięte:** tylko w Projektach; na zewnątrz przez kopiowanie/PDF.
11. ~~Planowanie zasobów~~ — **rozstrzygnięte: tak**, z utylizacją między projektami (§14).
12. ~~Ewidencja czasu~~ — **rozstrzygnięte:** opcjonalna per projekt (§14.3a).
13. ~~Kto widzi utylizację~~ — **rozstrzygnięte:** PM (swoje projekty), kierownicy (poddrzewo ze struktury), zarząd (wszystko).
14. ~~Źródło nieobecności~~ — **rozstrzygnięte:** wpis ręczny; eDokumenty jako dodatek na końcu (`ORG_STRUCTURE_PLAN.md` S6).

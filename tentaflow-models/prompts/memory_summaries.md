# PROMPT GENERATORA: MEMORY — HIERARCHICZNE PODSUMOWANIA

KRYTYCZNE: Twój JEDYNY output to surowe linie JSONL. ZERO komentarzy, ZERO markdown, ZERO tabel. Pierwszym znakiem outputu musi być `{`. Cokolwiek innego = porażka.

Generujesz dane treningowe JSONL do modelu AI, który uczy się robić hierarchiczne podsumowania konwersacji. Model musi obsługiwać 3 typy operacji:

1. **L1** — podsumowanie 5 wiadomości (user+assistant)
2. **L2** — podsumowanie 5 podsumowań L1
3. **TOP_UPDATE** — aktualizacja rolling summary (stare TOP + nowe L1 → nowe TOP)

## ZADANIE
Wygeneruj **15 linii JSONL** (6 PL, 5 EN, 4 Inne: DE, FR, ES, RU).
Format: `{"input": "typ operacji + dane wejściowe", "output": "podsumowanie"}`

## TYPY OPERACJI

### Typ 1: L1 — Podsumowanie 5 wiadomości (8 rekordów per batch)

Input:
```
<|summary|> L1
USER: Hej, mam problem z deployem na K8s
ASSISTANT: Sprawdź logi poda. Jaki error?
USER: CrashLoopBackOff, nie łączy się z bazą
ASSISTANT: Pewnie brakuje serwisu. Sprawdź kubectl get svc
USER: Faktycznie nie ma! Dzięki, naprawione.
```

Output:
```
TEMAT: Debugowanie deployu K8s — CrashLoopBackOff
USTALENIA: Problem z brakiem Service dla bazy danych. Naprawione po kubectl get svc.
PROBLEMY: Pod nie łączył się z bazą → brakujący Service
FEEDBACK: Pozytywny — "Dzięki, naprawione"
STATUS: Rozwiązane
```

### Typ 2: L2 — Podsumowanie 5 podsumowań L1 (4 rekordy per batch)

Input:
```
<|summary|> L2
L1_1: TEMAT: Setup projektu. USTALENIA: Next.js + PostgreSQL. STATUS: Zrobione
L1_2: TEMAT: Konfiguracja auth. USTALENIA: NextAuth z JWT. PROBLEMY: Brak NEXTAUTH_SECRET na prod. STATUS: Naprawione
L1_3: TEMAT: API endpoints. USTALENIA: REST + OpenAPI. FEEDBACK: User wolał GraphQL ale zostali przy REST. STATUS: Zrobione
L1_4: TEMAT: Testy. USTALENIA: Jest + Playwright. PROBLEMY: CI timeout. STATUS: W trakcie
L1_5: TEMAT: Deploy. USTALENIA: Docker + GitHub Actions. PROBLEMY: Build fails na ARM. STATUS: W trakcie
```

Output:
```
OKRES: Wiadomości 1-25
GŁÓWNY_TEMAT: Setup i konfiguracja projektu webowego
TECH_STACK: Next.js, PostgreSQL, NextAuth (JWT), REST API, OpenAPI, Jest, Playwright, Docker, GitHub Actions
ROZWIĄZANE: Setup projektu, auth (NEXTAUTH_SECRET fix), API design
W_TRAKCIE: Testy (CI timeout), Deploy (ARM build issue)
KLUCZOWE_DECYZJE: REST zamiast GraphQL (user wolał GraphQL), JWT sessions
FEEDBACK: User wolał GraphQL ale zaakceptował REST
```

### Typ 3: TOP_UPDATE — Aktualizacja rolling summary (3 rekordy per batch)

Input:
```
<|summary|> TOP_UPDATE
PREVIOUS_TOP: Projekt webowy: Next.js + PostgreSQL + NextAuth. Auth działa. API w REST. Testy i deploy w trakcie.
NEW_L1: TEMAT: Naprawienie CI. USTALENIA: Timeout zwiększony do 10min, testy przechodzą. Deploy na staging działa. PROBLEMY: Prod deploy wymaga secrets w GitHub. STATUS: Częściowo rozwiązane.
```

Output:
```
Projekt webowy: Next.js + PostgreSQL + NextAuth. Auth i API (REST) działają. Testy przechodzą (CI timeout naprawiony — 10min). Deploy na staging OK, prod wymaga konfiguracji secrets w GitHub Actions.
```

WAŻNE dla TOP_UPDATE:
- Output max 300 tokenów — ZAWSZE krótki
- Zachowaj kluczowe fakty z PREVIOUS_TOP
- Dodaj nowe informacje z NEW_L1
- Usuń nieaktualne detale (np. "w trakcie" → "zrobione")
- ZAWSZE zachowaj korekty użytkownika (UserCorrected)

## KLUCZOWE REGUŁY

1. **L1 summary: 100-200 tokenów** — zwięzłe ale kompletne
2. **L2 summary: 150-250 tokenów** — agregacja bez powtórzeń
3. **TOP update: max 300 tokenów** — rolling, nie rośnie
4. **Konwersacje muszą być REALISTYCZNE** — prawdziwe problemy techniczne, biznesowe, codzienne
5. **Min. 30% zawiera feedback użytkownika** — pozytywny, negatywny, korekty
6. **Min. 20% zawiera zmiany decyzji** — "wcześniej X, teraz Y"
7. **Używaj sekcji**: TEMAT, USTALENIA, PROBLEMY, FEEDBACK, STATUS, DECYZJE, TECH_STACK
8. **Status**: Rozwiązane, W trakcie, Odłożone, Zablokowane
9. **Podsumowanie w tym samym języku co konwersacja**

## KONTEKSTY KONWERSACJI (wylosuj 3-4 na batch)

- Programowanie (debug, code review, architektura, deployment)
- DevOps (CI/CD, Docker, K8s, monitoring)
- AI/ML (fine-tuning, embeddings, prompty)
- Biznes (planowanie, budżet, klient, sprzedaż)
- Infrastruktura (serwery, sieci, bazy danych)
- Design (UI/UX, Figma, prototypy)
- Nauka/research (paper review, eksperymenty)

## PRZYKŁADY

{"input": "<|summary|> L1\nUSER: Cześć, muszę skonfigurować CI w GitHub Actions dla projektu Rust.\nASSISTANT: Jasne! Czy chcesz tylko cargo test, czy także clippy + fmt + cache?\nUSER: Wszystko, ma być pełny pipeline.\nASSISTANT: Polecam actions-rs/toolchain + Swatinem/rust-cache na cache rejestru. Workflow: fmt → clippy → test.\nUSER: Ok, zrobiłem ale czas builda 8 minut. Mogę szybciej?", "output": "TEMAT: Konfiguracja pipeline CI dla projektu Rust\nUSTALENIA: GitHub Actions z fmt+clippy+test, Swatinem/rust-cache do cache rejestru\nPROBLEMY: Build 8 minut — nie rozwiązane\nFEEDBACK: \"mogę szybciej?\" (negatywny — wolny build)\nSTATUS: W trakcie — optymalizacja czasu builda"}
{"input": "<|summary|> L2\nL1_1: TEMAT: Konfiguracja CI. USTALENIA: fmt+clippy+test, Swatinem/rust-cache. PROBLEMY: Build 8 min. STATUS: W trakcie\nL1_2: TEMAT: Fix slow build. USTALENIA: Dodany sccache + cargo-chef warstwy. FEEDBACK: \"3 minuty, super\". STATUS: Rozwiązane\nL1_3: TEMAT: Test matrix. USTALENIA: ubuntu-latest + macos-latest + windows-latest. STATUS: Rozwiązane\nL1_4: TEMAT: Coverage. USTALENIA: cargo-llvm-cov + Codecov upload. FEEDBACK: User chce raport per-PR. STATUS: W trakcie\nL1_5: TEMAT: Release pipeline. USTALENIA: cargo-dist + signed binaries. DECYZJE: Wersjonowanie semver. STATUS: W trakcie", "output": "OKRES: Wiadomości 1-25\nGŁÓWNY_TEMAT: Pełen pipeline CI/CD dla projektu Rust\nTECH_STACK: GitHub Actions, sccache, cargo-chef, cargo-llvm-cov, Codecov, cargo-dist\nROZWIĄZANE: Konfiguracja CI, fix slow build (sccache), test matrix\nW_TRAKCIE: Coverage raport per-PR, release pipeline (cargo-dist)\nKLUCZOWE_DECYZJE: sccache zamiast cargo-cache only, semver dla release, signed binaries\nFEEDBACK: Build 8→3 min (\"super\"), user chce coverage per-PR"}
{"input": "<|summary|> TOP_UPDATE\nPREVIOUS_TOP: CI Rust w GitHub Actions: fmt+clippy+test, sccache (3 min build), test matrix 3 OS. Coverage przez cargo-llvm-cov, raport per-PR w toku. Release pipeline cargo-dist + signed binaries — w toku.\nNEW_L1: TEMAT: Coverage report per-PR. USTALENIA: Codecov action publikuje komentarz na PR z coverage delta. FEEDBACK: \"świetnie, dokładnie o to chodziło\". STATUS: Rozwiązane.", "output": "CI Rust w GitHub Actions: fmt+clippy+test, sccache (3 min build), test matrix 3 OS. Coverage report per-PR działa (Codecov delta na PR). Release pipeline cargo-dist + signed binaries — do zrobienia."}

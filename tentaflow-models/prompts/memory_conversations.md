# PROMPT GENERATORA: MEMORY — PODSUMOWANIE KONWERSACJI

KRYTYCZNE: Twój JEDYNY output to surowe linie JSONL. ZERO komentarzy, ZERO markdown, ZERO tabel. Pierwszym znakiem outputu musi być `{`. Cokolwiek innego = porażka.

Generujesz dane treningowe JSONL do modelu AI, który uczy się podsumowywać konwersacje z asystentem AI — wyciągać kluczowe ustalenia, decyzje, problemy, feedback użytkownika i action items.

## ZADANIE
Wygeneruj **15 linii JSONL** (6 PL, 5 EN, 4 Inne: DE, FR, ES, RU).
Format: `{"input": "pełna konwersacja", "output": "strukturalne podsumowanie"}`

## FORMAT KONWERSACJI (input)

Symulacja rozmowy user↔assistant w formacie:

```
USER: Hej, potrzebuję pomocy z konfiguracją CI w GitHub Actions
ASSISTANT: Jasne! Jaki masz stack? Node, Python, Rust?
USER: Rust. Chce odpalac cargo test na PR.
ASSISTANT: Uzyj actions-rs/toolchain + cache rejestru. Pokaze przyklad workflow...
USER: Zrobilem. Build dziala ale jest wolny — 8 minut na PR.
ASSISTANT: Dodaj sccache albo cargo-chef do warstw cache w jobie.
USER: Dodalem sccache, teraz 3 minuty. Dzieki!
```

## FORMAT OUTPUT (podsumowania)

```
TEMAT: Konfiguracja CI dla projektu Rust w GitHub Actions
USTALENIA:
- Workflow odpala cargo test na kazdym PR
- Cache: actions-rs/toolchain + sccache
- Time-to-green: 3 minuty po dodaniu sccache (z 8 min)
PROBLEMY_ZGŁOSZONE:
- Build wolny (8 min) → rozwiazane sccache
FEEDBACK_UŻYTKOWNIKA:
- "Dodalem sccache, teraz 3 minuty. Dzieki!" (pozytywny)
ACTION_ITEMS: brak
DECYZJE: sccache jako cache build dla CI
```

Kluczowe sekcje:
- **TEMAT** — o czym była rozmowa (1 zdanie)
- **USTALENIA** — co zostało ustalone/skonfigurowane/zdecydowane
- **PROBLEMY_ZGŁOSZONE** — co nie działało, błędy, bugi (z zaznaczeniem czy rozwiązane)
- **FEEDBACK_UŻYTKOWNIKA** — dosłowne cytaty z oceną (pozytywny/negatywny/neutralny)
- **ACTION_ITEMS** — co jeszcze trzeba zrobić
- **DECYZJE** — podjęte decyzje techniczne lub biznesowe
- **KOD** — jeśli w rozmowie był istotny kod/config, kluczowe fragmenty

## TYPY KONWERSACJI (wylosuj 4-5 na batch)

### Techniczne (programowanie):
- Konfiguracja CI/CD (GitHub Actions, GitLab CI, Jenkins)
- Debugowanie błędu w kodzie (Python, Rust, TypeScript, C#)
- Review kodu / refactoring
- Setup projektu (Docker, Kubernetes, Terraform)
- Implementacja feature'a (API endpoint, UI komponent, algorytm)
- Migracja bazy danych
- Optymalizacja wydajności (profiling, caching, indeksy)
- Konfiguracja monitoringu (Grafana, Prometheus, alerting)

### Techniczne (AI/ML):
- Fine-tuning modelu (hiperparametry, dane, ewaluacja)
- Prompt engineering (system prompt, few-shot, chain-of-thought)
- Deployment modelu (GGUF, quantization, inference server)
- Integracja z API (OpenAI, Claude, local LLM)

### Biznesowe:
- Planowanie sprintu / roadmap
- Analiza wymagań klienta
- Przygotowanie oferty / wyceny
- Rozwiązywanie problemu klienta (support)
- Onboarding nowego pracownika

### Z dokumentem w kontekście:
- User wkleja fragment dokumentu → pyta o szczegóły
- User uploaduje raport → prosi o analizę
- User cytuje mail → prosi o odpowiedź

## KLUCZOWE REGUŁY

1. **Konwersacja 8-20 wymian** (USER/ASSISTANT) — wystarczająco długa żeby było co podsumować
2. **Konwersacja 400-1200 tokenów**
3. **Min. 30% konwersacji musi zawierać PROBLEMY** — user mówi że coś nie działa, AI poprawia
4. **Min. 20% konwersacji musi zawierać NEGATYWNY FEEDBACK** — "to źle", "nie o to chodziło", "ciągle nie działa", "zrobiłeś błąd"
5. **Podsumowanie wyciąga DOKŁADNE cytaty** feedbacku użytkownika
6. **Konwersacje muszą być REALISTYCZNE** — prawdziwe problemy, prawdziwe rozwiązania, prawdziwe nazwy technologii
7. **Podsumowanie w tym samym języku co konwersacja**
8. **Action items tylko gdy są wyraźnie ustalane** — nie wymyślaj

## WZORCE FEEDBACKU UŻYTKOWNIKA (model musi się nauczyć je wykrywać)

Negatywny:
- "To nie działa" / "This doesn't work"
- "Źle zrobiłeś" / "That's wrong"
- "Nie o to chodziłem" / "That's not what I meant"
- "Ciągle ten sam błąd" / "Still the same error"
- "Ale to jest wolne" / "It's too slow"
- "To nie rozwiązuje mojego problemu" / "This doesn't solve my issue"

Pozytywny:
- "Ok, działa!" / "It works now!"
- "Dzięki, to pomogło" / "Thanks, that helped"
- "Idealne" / "Perfect"
- "Dobra, to miało sens" / "That makes sense"

Korekta (user poprawia AI):
- "Nie, ja używam PostgreSQL a nie MySQL"
- "Wersja jest 3.12, nie 3.11"
- "Nie w tym pliku, w tamtym"
- "Chodziło mi o frontend, nie backend"

## PRZYKŁADY

{"input": "USER: Cześć, mam problem z deploym na Kubernetes. Pod ciągle się restartuje.\nASSISTANT: Sprawdźmy logi. Jaki jest wynik kubectl logs dla tego poda?\nUSER: CrashLoopBackOff, w logach: Error: cannot connect to database at db-service:5432\nASSISTANT: Pod nie może połączyć się z bazą. Sprawdź czy serwis db-service istnieje: kubectl get svc db-service\nUSER: Nie ma takiego serwisu! Ale wcześniej działało.\nASSISTANT: Prawdopodobnie ktoś usunął Service. Utwórz go ponownie z manifestu. Masz plik YAML?\nUSER: Tak, mam db-service.yaml. Odpalam kubectl apply -f db-service.yaml\nASSISTANT: Świetnie. Po utworzeniu serwisu zrestartuj poda: kubectl rollout restart deployment/app\nUSER: Działa! Pod jest Running. Ale teraz mam inny problem — odpowiedzi są bardzo wolne, timeout po 30s.\nASSISTANT: To może być problem z connection poolingiem. Ile replik bazy masz?\nUSER: Jedną.\nASSISTANT: Przy jednej replice i wielu podach aplikacji połączenia się wyczerpują. Dodaj PgBouncer jako connection pooler albo zwiększ max_connections w PostgreSQL.\nUSER: Dodam PgBouncera. Dzięki za pomoc!", "output": "TEMAT: Problem z deploymentem na Kubernetes — CrashLoopBackOff i wolne odpowiedzi\nUSTALENIA:\n- Przyczyna CrashLoopBackOff: brakujący Service db-service (połączenie z bazą)\n- Fix: kubectl apply -f db-service.yaml + rollout restart\n- Problem z wydajnością: brak connection poolingu przy wielu podach i 1 replice bazy\n- Decyzja: dodanie PgBouncer jako connection pooler\nPROBLEMY_ZGŁOSZONE:\n- CrashLoopBackOff, cannot connect to database → rozwiązane (brakujący Service)\n- Timeout 30s, wolne odpowiedzi → rozwiązane (PgBouncer)\nFEEDBACK_UŻYTKOWNIKA:\n- \"Nie ma takiego serwisu! Ale wcześniej działało.\" (zaskoczenie)\n- \"Działa! Pod jest Running.\" (pozytywny)\n- \"odpowiedzi są bardzo wolne, timeout po 30s\" (negatywny)\n- \"Dzięki za pomoc!\" (pozytywny)\nACTION_ITEMS:\n- Dodać PgBouncer do deploymentu\nDECYZJE: PgBouncer jako connection pooler dla PostgreSQL"}
{"input": "USER: I need help setting up authentication in my Next.js app.\nASSISTANT: Sure! Are you using Next.js App Router or Pages Router? And what auth provider — NextAuth, Clerk, or custom?\nUSER: App Router with NextAuth.\nASSISTANT: Great. Install next-auth@beta for App Router support. Create app/api/auth/[...nextauth]/route.ts with your providers.\nUSER: Done. But I'm getting 'NEXTAUTH_SECRET is missing' error.\nASSISTANT: You need to set NEXTAUTH_SECRET in your .env.local. Generate one with: openssl rand -base64 32\nUSER: That's not the issue — I already have the secret set. The error is specifically on production, not local.\nASSISTANT: Ah, sorry. On production you need to set NEXTAUTH_URL to your actual domain, and ensure the secret is in your production environment variables, not just .env.local.\nUSER: Fixed! Auth works now. But the session expires after 5 minutes, I need it longer.\nASSISTANT: In your auth config, set session.maxAge. Default is 30 days but you might have it overridden. Set: session: { strategy: 'jwt', maxAge: 30 * 24 * 60 * 60 }\nUSER: Perfect, that's exactly what I needed. Thanks!", "output": "TOPIC: Setting up NextAuth authentication in Next.js App Router\nDECISIONS:\n- Auth provider: NextAuth (beta for App Router)\n- Session strategy: JWT\n- Session maxAge: 30 days (30*24*60*60)\nISSUES_REPORTED:\n- NEXTAUTH_SECRET missing on production → fixed (env vars in production, not just .env.local)\n- Session expiring after 5 minutes → fixed (session.maxAge config)\nUSER_FEEDBACK:\n- \"That's not the issue — I already have the secret set\" (correction — AI assumed wrong cause)\n- \"Fixed! Auth works now.\" (positive)\n- \"Perfect, that's exactly what I needed.\" (positive)\nACTION_ITEMS: none\nKEY_CONFIG:\n- File: app/api/auth/[...nextauth]/route.ts\n- Env: NEXTAUTH_SECRET, NEXTAUTH_URL (production)"}

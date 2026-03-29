# PROMPT GENERATORA: CHECK — WALIDACJA WYNIKÓW

KRYTYCZNE: Twój JEDYNY output to surowe linie JSONL. ZERO komentarzy, ZERO markdown, ZERO tabel. Pierwszym znakiem outputu musi być `{`. Cokolwiek innego = porażka.

Generujesz dane treningowe JSONL do modelu AI, który uczy się walidować wyniki kroków — czy krok się udał, czy trzeba powtórzyć, czy eskalować do dużego LLM.

## ZADANIE
Wygeneruj **30 linii JSONL** (12 PL, 10 EN, 8 Inne: DE, FR, ES, RU).
Format: `{"input": "<|check|>\nSTEP: ...\nRESULT: ...\nATTEMPT: N/3", "output": "OK lub RETRY|fix=... lub ESCALATE|reason=..."}`

## FORMAT

### Input
```
<|check|>
STEP: TOOL outlook.send_email|to=jan@firma.pl|subject=Raport|body=...
RESULT: {"status": "sent", "message_id": "AAMk..."}
ATTEMPT: 1/3
```

### Output — sukces
```
OK
```

### Output — błąd naprawialny (RETRY)
```
RETRY|fix=Change recipient to jan.kowalski@firma.pl (typo in email)
```

### Output — błąd nienaprawialny (ESCALATE)
```
ESCALATE|reason=Logic error in generated SQL query — requires human review
```

## REGUŁY DECYZJI

### OK — krok się udał
- Tool zwrócił sukces (status 200, "sent", "created", "updated")
- Wynik zawiera oczekiwane dane
- Brak errorów w response

### RETRY (max 3 próby) — Qwen może naprawić sam
- Literówka w parametrze (email, nazwa pliku)
- Brak wymaganego pola — można uzupełnić z kontekstu
- Timeout — spróbuj jeszcze raz
- Prosty syntax error (brakujący cudzysłów, nawias)
- HTTP 429 (rate limit) — poczekaj i spróbuj
- Plik nie znaleziony — spróbuj inną ścieżkę/nazwę
- Format daty zły — popraw format

### ESCALATE — Qwen NIE może naprawić
- Błąd logiczny (zły algorytm, zła koncepcja)
- Permission denied / 403 (nie da się naprawić bez admina)
- Wynik jest nieoczekiwany i Qwen nie rozumie dlaczego
- Po 3 nieudanych próbach RETRY
- Zadanie wymaga kreatywnego myślenia
- Błąd w kodzie wymagający rozumienia biznesowej logiki
- Qwen nie jest pewny co jest źle

## PROPORCJE
- 40% OK — różne typy sukcesów
- 35% RETRY — różne typy naprawialnych błędów
- 15% ESCALATE — różne typy nienaprawialnych błędów
- 10% RETRY na attempt 3/3 → ESCALATE (automatyczna eskalacja po 3 próbach)

## TYPY WYNIKÓW DO WALIDACJI (wylosuj per batch)

### Tool results
- Email wysłany/nie wysłany
- Plik znaleziony/nie znaleziony
- API zwróciło dane/error
- Baza danych: query OK/error
- Deploy: success/failed
- Monitoring: alert/normal

### Build/compile results
- cargo build: success/error z linią
- npm build: success/error
- docker build: success/error
- Test: passed/failed z assertion

### API responses
- 200 OK z danymi
- 400 Bad Request (parametry)
- 401/403 Unauthorized
- 404 Not Found
- 429 Rate Limited
- 500 Internal Server Error
- Timeout

### Data validation
- Wyciągnięte dane kompletne/niekompletne
- Format danych prawidłowy/nieprawidłowy
- Wartości w oczekiwanym zakresie/poza

## PRZYKŁADY

{"input": "<|check|>\nSTEP: TOOL outlook.send_email|to=jan@firma.pl|subject=Raport Q1|body=W zalaczeniu raport.\nRESULT: {\"status\": \"sent\", \"message_id\": \"AAMkADM3...\"}\nATTEMPT: 1/3", "output": "OK"}
{"input": "<|check|>\nSTEP: TOOL outlook.send_email|to=jan@firma|subject=Raport|body=...\nRESULT: {\"error\": \"Invalid email address: jan@firma\"}\nATTEMPT: 1/3", "output": "RETRY|fix=Invalid email address. Change to jan@firma.pl or jan.kowalski@firma.pl"}
{"input": "<|check|>\nSTEP: TOOL sharepoint-rag.search_files|query=raport Q1 2026\nRESULT: {\"files\": [], \"count\": 0}\nATTEMPT: 1/3", "output": "RETRY|fix=No files found. Try broader search: query=raport kwartalny Q1"}
{"input": "<|check|>\nSTEP: TOOL sharepoint-rag.search_files|query=raport kwartalny\nRESULT: {\"files\": [], \"count\": 0}\nATTEMPT: 3/3", "output": "ESCALATE|reason=File not found after 3 attempts with different search queries. User may need to specify exact file name or location."}
{"input": "<|check|>\nSTEP: TOOL k8s.get_pods|namespace=production\nRESULT: {\"pods\": [{\"name\": \"api-7d8f9\", \"status\": \"Running\"}, {\"name\": \"worker-5c4d8\", \"status\": \"CrashLoopBackOff\"}]}\nATTEMPT: 1/3", "output": "OK"}
{"input": "<|check|>\nSTEP: TOOL bash|cmd=cargo build\nRESULT: error[E0308]: mismatched types at src/main.rs:42 — expected String, found &str\nATTEMPT: 1/3", "output": "RETRY|fix=Type mismatch at line 42. Change &str to String::from() or add .to_string()"}
{"input": "<|check|>\nSTEP: TOOL bash|cmd=cargo build\nRESULT: error[E0599]: no method named `process` found for struct `DataPipeline` in the current scope\nATTEMPT: 2/3", "output": "ESCALATE|reason=Missing method implementation requires understanding business logic. Need code-gen model to implement DataPipeline::process()"}
{"input": "<|check|>\nSTEP: TOOL jira.update_issue|issue_id=ALPHA-123|status=closed\nRESULT: {\"error\": \"403 Forbidden: User does not have permission to close issues\"}\nATTEMPT: 1/3", "output": "ESCALATE|reason=Permission denied — cannot close Jira issues. User needs admin to grant close permission."}
{"input": "<|check|>\nSTEP: MODEL code-gen|task=generate CSV parser\nRESULT: def parse_csv(file):\n    import csv\n    ...\nATTEMPT: 1/3", "output": "OK"}
{"input": "<|check|>\nSTEP: EXTRACT revenue,costs|document=Annual Report 2025...\nRESULT: revenue=BRAK\ncosts=12.4M PLN\nATTEMPT: 1/3", "output": "RETRY|fix=Revenue not extracted. Try looking for: przychody, income, sales, obroty in the document"}

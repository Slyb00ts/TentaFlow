# PROMPT GENERATORA: PLAN — PLANOWANIE WIELOKROKOWE

KRYTYCZNE: Twój JEDYNY output to surowe linie JSONL. ZERO komentarzy, ZERO markdown, ZERO tabel. Pierwszym znakiem outputu musi być `{`. Cokolwiek innego = porażka.

Generujesz dane treningowe JSONL do modelu AI, który uczy się tworzyć plany wykonania złożonych zadań. Model dostaje listę dostępnych narzędzi i modeli, oraz zadanie od usera. Tworzy plan krok po kroku.

## ZADANIE
Wygeneruj **15 linii JSONL** (6 PL, 5 EN, 4 Inne: DE, FR, ES, RU).
Format: `{"input": "<|plan|>\nTOOLS: ...\nMODELS: ...\nUSER: ...", "output": "plan kroków"}`

## FORMAT

### Input
```
<|plan|>
TOOLS: outlook.send_email(to*,subject*,body*), outlook.search_emails(query*), sharepoint-rag.search_files(query*), sharepoint-rag.get_file_content(file_id*), teams.send_message(to*,message*)
MODELS: default|chat, code-gen|code, rag-expert|rag
USER: Znajdź raport Q1 w SharePoint, wyciągnij przychody i koszty, porównaj z Q4 i wyślij analizę do CFO mailem
```

### Output — pełny plan
```
1. TOOL sharepoint-rag.search_files|query=raport Q1 2026
2. TOOL sharepoint-rag.get_file_content|file_id={from_1}
3. EXTRACT revenue,costs,period|document={from_2}
4. RECALL Q4 revenue and costs data
5. MODEL rag-expert|task=compare Q1 vs Q4|context={from_3},{from_4}
6. TOOL outlook.send_email|to=cfo|subject=Analiza Q1 vs Q4|body={from_5}
7. DONE
```

### Output — brakuje informacji
```
#ASK|question=Do którego CFO mam wysłać? Jaki okres Q4 — 2025 czy 2024?
```

## TYPY KROKÓW

| Typ | Format | Kto wykonuje |
|-----|--------|-------------|
| `TOOL` | `TOOL addon.function\|param=val` | Qwen (tool calling) |
| `EXTRACT` | `EXTRACT pola\|document={from_N}` | Qwen (extract) |
| `RECALL` | `RECALL search query` | Qwen (recall) → vector/graph search |
| `MODEL` | `MODEL alias\|task=opis\|context={from_N}` | Wybrany LLM |
| `CHECK` | `CHECK {from_N}\|expect=opis` | Qwen (walidacja) |
| `STORE` | `STORE layer\|fact=...` | Qwen (memory) |
| `ASK` | `#ASK\|question=...` | Zwróć pytanie do usera |
| `DONE` | `DONE` | Zakończ plan |

## REGUŁY

1. **Referencje `{from_N}`** — wynik kroku N jest przekazany jako input
2. **Max 10 kroków** — plan nie powinien być za długi
3. **TOOL przed MODEL** — najpierw zbierz dane, potem daj LLM do przetworzenia
4. **RECALL na początku** — jeśli potrzebny kontekst z pamięci
5. **CHECK po krytycznych krokach** — np. po wysłaniu maila, po zapisie
6. **STORE na końcu** — zapamiętaj ważne wyniki
7. **#ASK zamiast zgadywania** — jeśli brakuje kluczowej informacji, PYTAJ
8. **Plan w języku angielskim** — kroki zawsze EN (nazwy tooli, parametry), opisy mogą być w języku usera
9. **Każdy TOOL musi mieć wystarczające parametry** — lub `{from_N}` jeśli z poprzedniego kroku

## KIEDY #ASK

- Brak odbiorcy ("wyślij to do...") — kogo?
- Niejasny zakres ("zrób raport") — z czego? za jaki okres?
- Brak pliku/dokumentu ("wyciągnij dane z...") — z którego dokumentu?
- Niejednoznaczne ("sprawdź to") — co dokładnie?
- Ale NIE pytaj o rzeczy które można wyciągnąć z RECALL

## SCENARIUSZE (wylosuj 3-4 per batch)

### Biznesowe
- Znajdź dokument → wyciągnij dane → wyślij komuś
- Sprawdź maile od klienta → przygotuj odpowiedź → wyślij
- Zbierz dane z wielu źródeł → porównaj → zrób raport
- Sprawdź kalendarz → znajdź wolny termin → zaproponuj spotkanie

### Techniczne
- Sprawdź logi → znajdź errory → napisz raport → wyślij na Slack
- Przeczytaj kod → zrób review → dodaj komentarze w GitHub
- Sprawdź monitoring → porównaj z SLA → zgłoś jeśli przekroczone
- Przeszukaj dokumentację → znajdź rozwiązanie → zastosuj

### DevOps
- Sprawdź status deploymentu → jeśli error to rollback → powiadom zespół
- Zrób backup → zweryfikuj → potwierdź mailowo
- Sprawdź certyfikaty SSL → jeśli wygasające → odnów → powiadom

### Wielosystemowe
- Dane z CRM + dane z ERP → porównaj → raport dla zarządu
- Mail od klienta → sprawdź historię w CRM → odpowiedz
- Ticket z Jira → sprawdź logi → znajdź rozwiązanie → zaktualizuj ticket

## TRUDNOŚĆ
- 30% proste: 3-4 kroki, jeden system
- 40% średnie: 5-7 kroków, 2-3 systemy
- 20% trudne: 7-10 kroków, wiele systemów, warunki
- 10% #ASK: brakuje informacji

## PRZYKŁADY

{"input": "<|plan|>\nTOOLS: outlook.list_emails(folder?,filter?), outlook.read_email(message_id*), outlook.reply_email(message_id*,body*), teams.send_message(to*,message*)\nMODELS: default|chat, rag-expert|rag\nUSER: Sprawdź czy mam nowe maile od klienta Atman i odpowiedz na najważniejszy", "output": "1. RECALL Atman client context and history\n2. TOOL outlook.list_emails|filter=from:atman isRead eq false\n3. TOOL outlook.read_email|message_id={from_2.first}\n4. MODEL rag-expert|task=draft reply to client email|context={from_1},{from_3}\n5. TOOL outlook.reply_email|message_id={from_2.first}|body={from_4}\n6. STORE project|fact=Replied to Atman email about {from_3.subject}\n7. DONE"}
{"input": "<|plan|>\nTOOLS: jira.list_issues(project*,status?), jira.update_issue(issue_id*,status?,comment?), slack.send_message(channel*,message*)\nMODELS: default|chat\nUSER: Zamknij wszystkie ukończone taski w projekcie ALPHA i powiadom zespół", "output": "1. TOOL jira.list_issues|project=ALPHA|status=done\n2. CHECK {from_1}|expect=list of completed issues\n3. TOOL jira.update_issue|issue_id={from_1.each}|status=closed|comment=Closed by AI assistant\n4. MODEL default|task=summarize closed issues for team notification|context={from_1}\n5. TOOL slack.send_message|channel=#alpha-team|message={from_4}\n6. STORE project|fact=Closed {from_1.count} issues in ALPHA project\n7. DONE"}
{"input": "<|plan|>\nTOOLS: outlook.send_email(to*,subject*,body*)\nMODELS: default|chat\nUSER: Wyślij raport do zarządu", "output": "#ASK|question=Jaki raport mam wysłać? Czy masz gotowy dokument, czy mam go przygotować? Do kogo konkretnie z zarządu?"}
{"input": "<|plan|>\nTOOLS: k8s.get_pods(namespace*), k8s.get_logs(pod*,lines?), grafana.query(metric*,period?), slack.send_message(channel*,message*)\nMODELS: default|chat, code-gen|code\nUSER: Check if production is healthy and report any issues to the ops channel", "output": "1. TOOL k8s.get_pods|namespace=production\n2. CHECK {from_1}|expect=all pods Running\n3. TOOL grafana.query|metric=error_rate_5xx|period=1h\n4. TOOL grafana.query|metric=response_time_p99|period=1h\n5. CHECK {from_3}|expect=error rate below 1%\n6. CHECK {from_4}|expect=p99 below 500ms\n7. MODEL default|task=summarize production health status|context={from_1},{from_3},{from_4}\n8. TOOL slack.send_message|channel=#ops|message={from_7}\n9. DONE"}

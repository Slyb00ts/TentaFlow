# PROMPT GENERATORA: INTENT ROUTER

KRYTYCZNE: Twój JEDYNY output to surowe linie JSONL. ZERO komentarzy, ZERO markdown, ZERO tabel. Pierwszym znakiem outputu musi być `{`. Cokolwiek innego = porażka.

Generujesz dane treningowe JSONL do modelu AI, który uczy się rozpoznawać jakie taski trzeba wykonać dla danej wiadomości użytkownika. Model pełni rolę ROUTERA — decyduje CO zrobić, nie JAK. Router NIE wie jakie konkretne narzędzia istnieją — wie tylko CZY potrzebne jest narzędzie.

## ZADANIE
Wygeneruj **50 linii JSONL** (20 PL, 15 EN, 15 Inne: DE, FR, ES, RU, ZH, JA, KO, AR, PT, IT).
Format: `{"input": "wiadomość użytkownika", "output": "lista tasków"}`

## DOSTĘPNE TASKI

GUARD NIE jest tutaj — guard jest ZAWSZE osobno, hardcoded.

| Task | Kiedy |
|------|-------|
| `TEXT` | Pytanie o wiedzę, small talk, definicje — przekaż do domyślnego LLM. ZAWSZE samodzielne. |
| `TOOLS` | Trzeba ZROBIĆ coś w zewnętrznym systemie — wysłać, sprawdzić, uruchomić, utworzyć, usunąć |
| `MODEL` | Trzeba specyficznego modelu AI — generowanie kodu, obrazu, analiza medyczna |
| `MEMORY` | User podaje fakt, decyzję, preferencję — trzeba zapamiętać |
| `FEEDBACK` | User ocenia odpowiedź AI — korekta, potwierdzenie, narzekanie, pochwała |
| `RECALL` | Odpowiedź wymaga kontekstu z pamięci (prawie zawsze oprócz TEXT) |
| `EXTRACT` | User chce wyciągnąć konkretne pola/dane z dokumentu/tekstu |
| `PLAN` | Złożone zadanie wymagające wielu zależnych kroków — nie da się jednym toolem |

## REGUŁY

1. `TEXT` — samodzielne, NIGDY z innymi
2. `RECALL` — z prawie wszystkim (potrzeba kontekstu)
3. `FEEDBACK` + `MEMORY` — często razem (korekta = zapamiętaj poprawkę)
4. `EXTRACT` — może łączyć z `MEMORY` (wyciągnij i zapamiętaj) lub `RECALL`
5. `MODEL` — gdy zadanie wymaga specyficznego modelu (kod, obraz, medycyna)
6. `PLAN` — gdy zadanie ma wiele ZALEŻNYCH kroków (znajdź → wyciągnij → wyślij)
7. `TOOLS` oznacza DOWOLNE narzędzie — router nie wie jakie, wie tylko że trzeba coś ZROBIĆ
8. `MODEL` vs `TEXT` — TEXT to prosta odpowiedź domyślnym LLM, MODEL to gdy potrzeba konkretnego modelu
9. Max 5 tasków jednocześnie

## PROPORCJE
- 15% `TEXT` (samo)
- 20% pojedynczy task + RECALL
- 30% dwa taski
- 20% trzy taski
- 15% cztery taski

## KLUCZOWE: RÓŻNORODNOŚĆ

Model musi rozpoznawać TOOLS dla SETEK różnych scenariuszy, nie tylko "wyślij maila". Wylosuj kontekst z poniższych list.

### Domeny narzędzi (wylosuj 5-8 per batch):

**Komunikacja:** email (Outlook, Gmail, Thunderbird), czat (Teams, Slack, Discord, Mattermost, Telegram, WhatsApp Business), SMS gateway, push notifications, wideorozmowy (Zoom, Meet, Webex), VoIP

**Kalendarze i planowanie:** Google Calendar, Outlook Calendar, CalDAV, harmonogramy, rezerwacje sal, terminy wizyt, przypomnienia, time tracking (Toggl, Clockify)

**CRM i sprzedaż:** Salesforce, HubSpot, Pipedrive, Zoho CRM, leady, szanse sprzedaży, kontakty, oferty, follow-upy, pipeline, prognozowanie

**ERP i finanse:** SAP, Oracle, Sage, Enova, Comarch, faktury, zamówienia, magazyn, stany magazynowe, przelewy, rozliczenia, księgowość, kontroling, budżety

**Zarządzanie projektami:** Jira, Asana, Monday, Trello, Linear, ClickUp, Notion, Confluence, tickety, sprinty, backlog, story pointy, epiki, roadmapa

**Dokumenty i pliki:** SharePoint, OneDrive, Google Drive, Dropbox, Box, Nextcloud, S3, dokumenty, umowy, faktury, regulaminy, podpisy elektroniczne (DocuSign, Autenti)

**HR i rekrutacja:** BambooHR, Workday, SAP SuccessFactors, urlopy, delegacje, ewidencja czasu, rekrutacja, onboarding, szkolenia, oceny pracownicze

**DevOps i infrastruktura:** GitHub, GitLab, Bitbucket, Jenkins, ArgoCD, Docker, Kubernetes, Terraform, Ansible, Proxmox, VMware vSphere, ESXi, Hyper-V, deployment, rollback, logi, monitoring

**Monitorowanie i alerting:** Grafana, Prometheus, Datadog, New Relic, Zabbix, Nagios, PagerDuty, OpsGenie, alerty, metryki, dashboardy, SLA

**BMS i IoT:** SCADA, Modbus, BACnet, OPC UA, czujniki, sterowniki, temperatury, wilgotność, alarmy, klimatyzacja, kontrola dostępu, oświetlenie, energia

**Smart Home:** Home Assistant, Apple HomeKit, Google Home, Alexa, światła, termostaty, rolety, kamery, zamki, sensory ruchu, sceny, automatyzacje

**Cloud i hosting:** AWS (EC2, S3, RDS, Lambda), Azure (VM, Blob, SQL), GCP (GCE, GCS, BigQuery), Hetzner, OVH, DigitalOcean, Cloudflare, DNS, SSL

**Bazy danych:** PostgreSQL, MySQL, MongoDB, Redis, ClickHouse, Elasticsearch, Cassandra, Neo4j, DynamoDB, backup, restore, migracje, query

**Sieć i bezpieczeństwo:** firewall (pfSense, FortiGate, OPNsense), VPN (WireGuard, OpenVPN), SSH, proxy, certyfikaty SSL, skanowanie portów, IDS/IPS

**AI/ML:** trenowanie modeli, inference, embeddingi, fine-tuning, prompt engineering, ewaluacja, dataset management, model deployment

**Observability:** logi (ELK, Loki, Splunk), tracing (Jaeger, Zipkin), APM, error tracking (Sentry, Bugsnag), uptime monitoring

**E-commerce:** Shopify, WooCommerce, Magento, PrestaShop, zamówienia, produkty, ceny, promocje, stany magazynowe, wysyłki, zwroty

**Automatyzacja:** n8n, Zapier, Make (Integromat), Power Automate, cron joby, webhooks, ETL, data pipeline

**Medycyna:** systemy HIS (szpitalne), eWUŚ, e-recepty, terminarz wizyt, karty pacjentów, wyniki badań, DICOM

**Prawo i compliance:** systemy kancelaryjne, terminy procesowe, rejestr umów, RODO, audyty, zarządzanie ryzykiem, polityki bezpieczeństwa

### Typy wiadomości (mieszaj długości i style):

**Krótkie bezpośrednie** (1 zdanie):
- "Sprawdź status serwera prod-web-01"
- "Utwórz ticket w Jira"
- "Ile mamy leadów w tym miesiącu?"

**Średnie z kontekstem** (2-3 zdania):
- "Wracając do sprawy klienta ABC — sprawdź czy faktura została wysłana i przypomnij mi jakie były ustalenia z ostatniego spotkania."
- "W CRM mamy kontakt od Jan Kowalski z firmy TechCorp, zaktualizuj mu status na 'negocjacje' i zapamiętaj że interesuje go pakiet Enterprise."

**Długie złożone** (3-5 zdań):
- "Patrz, z tym deploymentem jest problem — wczoraj robiliśmy rollback bo nowa wersja crashowała na produkcji. Sprawdź logi z klastra kubernetes za ostatnie 24h, wyciągnij wszystkie errory i porównaj z tym co było przed deployem. Zapamiętaj że wersja 2.4.1 jest niestabilna."
- "Mieliśmy spotkanie z klientem z Atman i ustaliliśmy kilka rzeczy. Po pierwsze, zmieniamy zakres SLA z 99.9% na 99.95%. Po drugie, klient chce dodatkowy monitoring na klucze szafowe. Zaktualizuj te informacje i wyślij potwierdzenie do Krzysztofa."

**Z wklejonym fragmentem:**
- "Dostałem taki mail:\n\nSzanowni Państwo, informuję o opóźnieniu dostawy...\n\nWyciągnij z niego kto pisze, czego dotyczy i jaki jest termin, i przygotuj odpowiedź."
- "Ten fragment z umowy:\n\n§12. Kary umowne wynoszą 0.5% wartości...\n\nZapamiętaj te warunki."

**Feedback + akcja:**
- "Nie, kurde, źle to zrobiłeś — miało być na staging a nie na produkcję! Zrób rollback natychmiast."
- "Dobra, w końcu działa. Zapamiętaj to rozwiązanie i wyślij info na kanał devops na Slacku."
- "To nie to co chciałem — miałeś sprawdzić w Jira a nie w GitHub. Sprawdź jeszcze raz we właściwym miejscu."

## PRZYKŁADY

{"input": "Jaka jest różnica między TCP a UDP?", "output": "TEXT"}
{"input": "Sprawdź temperaturę w serwerowni z czujnika SH-04", "output": "TOOLS,RECALL"}
{"input": "Ile mamy otwartych ticketów w Jira w sprincie 47?", "output": "TOOLS,RECALL"}
{"input": "Zrób backup bazy PostgreSQL z serwera prod-db-01", "output": "TOOLS,RECALL"}
{"input": "Sprawdź status maszyny wirtualnej VM-web-03 na Proxmoxie", "output": "TOOLS,RECALL"}
{"input": "Wyłącz światła w sali konferencyjnej B", "output": "TOOLS"}
{"input": "Pokaż leady z tego tygodnia w CRM", "output": "TOOLS,RECALL"}
{"input": "Nie, to jest źle. Nie używamy MySQL tylko PostgreSQL 16 z pgvector.", "output": "FEEDBACK,MEMORY"}
{"input": "Wyciągnij kwoty, strony i kary z tej umowy:\n\nUMOWA NR 2026/03/127 zawarta dnia 15.03.2026 pomiędzy TechVision Sp. z o.o. (Zamawiający) a CloudMax...", "output": "EXTRACT,RECALL"}
{"input": "Zdecydowaliśmy że deployment robimy przez ArgoCD a nie ręcznie przez SSH", "output": "MEMORY,RECALL"}
{"input": "Dobra, działa! Zapamiętaj to rozwiązanie i wyślij info na Slack do kanału #devops", "output": "FEEDBACK,MEMORY,TOOLS,RECALL"}
{"input": "Przeanalizuj ten dokument i zapamiętaj najważniejsze ustalenia, potem wyślij podsumowanie do Marka", "output": "EXTRACT,MEMORY,TOOLS,RECALL"}
{"input": "Hej, dzięki za pomoc!", "output": "TEXT"}
{"input": "Wracając do tematu alarmów — wczoraj mówiłeś o progach ale to nie pomogło. Sprawdź konfigurację w BMS i zmień próg na 22 stopnie.", "output": "FEEDBACK,MEMORY,TOOLS,RECALL"}
{"input": "Zapamiętaj: klient Atman ma SLA 99.95% i monitoring szaf z czujnikami temperatury i wilgotności", "output": "MEMORY"}
{"input": "Was ist der aktuelle Status des Kubernetes-Clusters?", "output": "TOOLS,RECALL"}
{"input": "Проверь последние заказы в CRM за эту неделю", "output": "TOOLS,RECALL"}
{"input": "Envoie un message à l'équipe DevOps sur Slack pour confirmer le déploiement", "output": "TOOLS,RECALL"}
{"input": "检查一下生产服务器的CPU使用率", "output": "TOOLS,RECALL"}
{"input": "Não, está errado. Usamos Redis, não Memcached.", "output": "FEEDBACK,MEMORY"}
{"input": "Ustaw termostat w biurze na 22 stopnie i zapamiętaj że Kowalski woli chłodniej", "output": "TOOLS,MEMORY,RECALL"}
{"input": "Sprawdź co mamy w SharePoint o projekcie Alpha i wyciągnij z dokumentacji wymagania niefunkcjonalne", "output": "TOOLS,EXTRACT,RECALL"}
{"input": "Kurde, znowu ten sam błąd z certyfikatem SSL. Mówiłem żebyś użył Let's Encrypt a nie self-signed. Napraw to na serwerze proxy.", "output": "FEEDBACK,MEMORY,TOOLS,RECALL"}
{"input": "Napisz mi funkcję w Rust, która parsuje CSV i zwraca Vec<HashMap<String, String>>", "output": "MODEL,RECALL"}
{"input": "Wygeneruj obrazek logo dla naszej aplikacji — niebieskie ośmiornice na ciemnym tle", "output": "MODEL"}
{"input": "Na podstawie dokumentacji w SharePoint odpowiedz jak skonfigurować VPN", "output": "TOOLS,MODEL,RECALL"}
{"input": "Znajdź raport Q1 w SharePoint, wyciągnij przychody i koszty, porównaj z Q4 i wyślij analizę do CFO", "output": "PLAN,RECALL"}
{"input": "Sprawdź logi K8s, znajdź errory z ostatnich 24h, napisz raport i wyślij na Slack do kanału #ops", "output": "PLAN,RECALL"}
{"input": "Przeanalizuj ten kontrakt pod kątem ryzyk prawnych", "output": "MODEL,EXTRACT,RECALL"}
{"input": "Zrób code review tego PR-a i dodaj komentarze w GitHub", "output": "MODEL,TOOLS,RECALL"}
{"input": "患者の最新の血液検査結果を分析して、推奨事項を作成してください", "output": "MODEL,RECALL"}
{"input": "Erstelle ein Bild von einem futuristischen Bürogebäude", "output": "MODEL"}

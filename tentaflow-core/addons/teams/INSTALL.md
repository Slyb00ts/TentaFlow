# Instalacja addonu Microsoft Teams

Instrukcja konfiguracji integracji TentaFlow z Microsoft Teams.
Wymaga konta administratora w Azure Active Directory (Entra ID) oraz w TentaFlow.

---

## 1. Rejestracja aplikacji w Azure Portal

### 1.1 Utworz rejestracje aplikacji

1. Zaloguj sie do [Azure Portal](https://portal.azure.com)
2. Przejdz do **Azure Active Directory** (Entra ID) → **App registrations** → **New registration**
3. Wypelnij formularz:
   - **Name**: `TentaFlow Teams`
   - **Supported account types**: wybierz odpowiednio:
     - *Single tenant* — tylko uzytkowniczy z Twojej organizacji
     - *Multitenant* — uzytkowniczy z dowolnej organizacji Azure AD
   - **Redirect URI**: wybierz **Web** i wpisz:
     ```
     https://<adres-tentaflow-ai>:8090/api/addons/teams/oauth/callback
     ```
     Gdzie `<adres-tentaflow-ai>` to adres serwera TentaFlow (np. `localhost` dla lokalnej instalacji
     lub publiczny adres domeny).

     Przyklady:
     - Lokalna instalacja: `https://localhost:8090/api/addons/teams/oauth/callback`
     - Serwer produkcyjny: `https://ai.twojafirma.pl:8090/api/addons/teams/oauth/callback`

4. Kliknij **Register**

### 1.2 Zapisz identyfikatory

Po rejestracji na stronie **Overview** znajdziesz:

| Pole | Gdzie wpisac w TentaFlow |
|------|---------------------------|
| **Application (client) ID** | Ustawienia addonu → `client_id` (sekrety) |
| **Directory (tenant) ID** | Ustawienia addonu → `Azure Tenant ID` |

### 1.3 Utworz client secret

1. Przejdz do **Certificates & secrets** → **Client secrets** → **New client secret**
2. Wpisz opis (np. `TentaFlow`) i ustaw wygasniecie (zalecane: 24 miesiace)
3. Kliknij **Add**
4. **NATYCHMIAST** skopiuj wartosc sekretu (pole **Value**) — nie bedzie mozna jej pozniej podejrzec
5. Wpisz ta wartosc w TentaFlow jako sekret `client_secret`

### 1.4 Skonfiguruj uprawnienia API (API Permissions)

1. Przejdz do **API permissions** → **Add a permission** → **Microsoft Graph**
2. Wybierz **Delegated permissions** i dodaj:

| Uprawnienie | Opis | Wymagane do |
|-------------|------|-------------|
| `User.Read` | Odczyt profilu zalogowanego uzytkownika | Podstawowe — zawsze wymagane |
| `Chat.ReadWrite` | Odczyt i wysylanie wiadomosci w czatach | Czaty Teams |
| `ChannelMessage.Send` | Wysylanie wiadomosci do kanalow | Kanaly Teams |
| `Calendars.Read` | Odczyt kalendarza | Kalendarz i spotkania |
| `Files.Read` | Odczyt plikow OneDrive/SharePoint | Przegladanie plikow |
| `Files.ReadWrite` | Zapis plikow OneDrive/SharePoint | Tworzenie/edycja plikow |
| `OnlineMeetings.ReadWrite` | Zarzadzanie spotkaniami | Bot na spotkaniach |
| `offline_access` | Odswiezanie tokenu bez ponownego logowania | Dlugotrwale sesje |

3. Kliknij **Grant admin consent for [Twoja organizacja]** — wymaga uprawnien administratora Azure AD

> **Uwaga**: Jesli nie masz uprawnien Global Admin, popros administratora organizacji
> o wyrazenie zgody (Admin Consent). Bez tego uzytkownicy nie beda mogli sie autoryzowac.

### 1.5 (Opcjonalnie) Bot na spotkaniach

Jesli chcesz uzywac bota AI na spotkaniach Teams (automatyczne dolaczanie, transkrypcja,
notatki), potrzebujesz dodatkowej konfiguracji:

1. Przejdz do **API permissions** → dodaj **Application permissions**:
   - `Calls.JoinGroupCall.All`
   - `Calls.InitiateGroupCall.All`
2. Przejdz do [Teams Developer Portal](https://dev.teams.microsoft.com) → **Apps** → **New app**
3. Skonfiguruj bota z Communication Services lub Azure Bot Service
4. Dodaj URL webhooka: `https://<adres-tentaflow-ai>:8090/api/addons/teams/webhook`

---

## 2. Konfiguracja w TentaFlow

### 2.1 Ustawienia addonu

1. Otworz dashboard TentaFlow (`https://localhost:8090`)
2. Zaloguj sie jako administrator
3. Przejdz do **Aplikacje** → kliknij kafelek **Microsoft Teams**
4. W zakladce **Ustawienia** wypelnij:

| Pole | Wartosc | Opis |
|------|---------|------|
| **Azure Tenant ID** | `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx` | Directory (tenant) ID z Azure Portal |
| **Automatycznie dolaczaj do spotkan** | Wl/Wyl | Bot automatycznie dolacza do nadchodzacych spotkan |
| **Tworz notatki ze spotkan** | Wl/Wyl | AI generuje podsumowania spotkan |
| **Nazwa bota na spotkaniach** | `TentaFlow` | Nazwa wyswietlana w Teams gdy bot jest na spotkaniu |

5. Kliknij **Zapisz ustawienia**

### 2.2 Sekrety OAuth

Sekrety sa przechowywane w zaszyfrowanej tabeli w bazie danych (AES-256-GCM).
Nie sa widoczne w konfiguracji addonu.

Ustaw sekrety przez API (lub przez przyszly panel sekretow w UI):

```bash
# Client ID
curl -sk -X PUT https://localhost:8090/api/addons/teams/secrets \
  -H "Authorization: Bearer <TOKEN>" \
  -H "Content-Type: application/json" \
  -d '{"key": "client_id", "value": "<APPLICATION_CLIENT_ID>"}'

# Client Secret
curl -sk -X PUT https://localhost:8090/api/addons/teams/secrets \
  -H "Authorization: Bearer <TOKEN>" \
  -H "Content-Type: application/json" \
  -d '{"key": "client_secret", "value": "<CLIENT_SECRET_VALUE>"}'
```

Gdzie `<TOKEN>` to JWT token z endpointu logowania:
```bash
curl -sk -X POST https://localhost:8090/api/auth/login \
  -H "Content-Type: application/json" \
  -d '{"username":"admin","password":"admin"}'
```

### 2.3 Redirect URI (serwer publiczny)

Jesli TentaFlow jest dostepne pod publicznym adresem (nie localhost),
ustaw bazowy URL redirectu:

```bash
curl -sk -X PUT https://localhost:8090/api/settings \
  -H "Authorization: Bearer <TOKEN>" \
  -H "Content-Type: application/json" \
  -d '{"key": "oauth_redirect_base_url", "value": "https://ai.twojafirma.pl:8090"}'
```

Domyslnie uzywany jest `https://localhost:8090`.

### 2.4 Autoryzacja uzytkownika

Kazdy uzytkownik TentaFlow musi jednorazowo autoryzowac swoje konto Microsoft:

1. W zakladce **Ustawienia** addonu Teams kliknij przycisk **Zaloguj do Microsoft**
2. Otworzy sie okno logowania Microsoft — zaloguj sie kontem organizacyjnym
3. Zaakceptuj wymagane uprawnienia
4. Po powrocie na strone TentaFlow, tokeny zostana zapisane automatycznie

Token jest odswiezany automatycznie (dzieki `offline_access`). Jesli wygasnie,
uzytkownik zostanie poproszony o ponowna autoryzacje.

### 2.5 Uprawnienia w TentaFlow

Administrator moze kontrolowac jakie funkcje Teams sa dostepne dla uzytkownikow
i grup w zakladce **Uprawnienia**:

| Uprawnienie | Opis | Domyslny poziom |
|-------------|------|-----------------|
| `chat_read` | Odczyt wiadomosci z czatow i kanalow | RO (odczyt) |
| `chat_write` | Wysylanie wiadomosci do czatow i kanalow | RW (odczyt/zapis) |
| `calendar_read` | Przegladanie kalendarza i spotkan | RO (odczyt) |
| `files_read` | Przegladanie plikow OneDrive/SharePoint | RO (odczyt) |
| `files_write` | Tworzenie i edycja plikow | RW (odczyt/zapis) |
| `meeting_join` | Dolaczanie bota do spotkan | RW (odczyt/zapis) |
| `meeting_audio` | Nasluchiwanie i mowienie na spotkaniach | RW (odczyt/zapis) |
| `meeting_notes` | Automatyczne notatki ze spotkan | RW (odczyt/zapis) |
| `notifications` | Wysylanie powiadomien | RW (odczyt/zapis) |
| `llm_access` | Korzystanie z modeli AI | RW (odczyt/zapis) |

Uprawnienia ustawia sie per uzytkownik lub per grupa. Poziomy dostepu:
- **Brak** — funkcja calkowicie wylaczona
- **RO** — tylko odczyt
- **RW** — odczyt i zapis
- **RWD** — pelny dostep (odczyt, zapis, usuwanie)

### 2.6 Limity zasobow (opcjonalnie)

W zakladce **Zasoby** mozna ustawic limity dla addonu:

| Limit | Opis | Domyslnie |
|-------|------|-----------|
| Maks. instancji | Ile jednoczesnych instancji WASM | 0 (bez limitu) |
| Limit CPU | Milisekundy CPU na minute | 0 (bez limitu) |
| Limit RAM | Megabajty pamieci RAM | 0 (bez limitu) |
| Dostep do GPU | Czy addon moze uzywac GPU | Tak |
| Limit VRAM | Megabajty pamieci GPU | 0 (bez limitu) |
| Limit storage | Megabajty na dysku | 0 (bez limitu) |
| Limit HTTP | Zadania HTTP na minute | 0 (bez limitu) |
| Limit tokenow LLM | Tokeny LLM na minute | 0 (bez limitu) |

---

## 3. Weryfikacja dzialania

### 3.1 Sprawdz status addonu

Addon powinien byc widoczny w dashboardzie z statusem **Aktywna**:

```bash
curl -sk https://localhost:8090/api/addons \
  -H "Authorization: Bearer <TOKEN>" | python3 -m json.tool
```

### 3.2 Sprawdz narzedzia

Po autoryzacji OAuth, narzedzia Teams sa dostepne dla LLM:

```bash
curl -sk https://localhost:8090/api/addons/teams/tools \
  -H "Authorization: Bearer <TOKEN>" | python3 -m json.tool
```

Oczekiwany wynik: 8 narzedzi (`send_message`, `list_messages`, `list_chats`,
`list_channels`, `get_calendar`, `list_files`, `join_meeting`, `get_meeting_notes`).

### 3.3 Testowy czat z AI

W czacie TentaFlow napisz:

```
Pokaz mi moje nadchodzace spotkania z Teams na ten tydzien
```

AI powinno uzyc narzedzia `teams.get_calendar` i zwrocic liste spotkan z Twojego kalendarza.

Inne przykladowe komendy:
- *"Wyslij wiadomosc do jan.kowalski@firma.pl: spotkanie przelozone na 15:00"*
- *"Pokaz ostatnie wiadomosci z kanalu General"*
- *"Jakie pliki sa na moim OneDrive?"*
- *"Dolacz do spotkania o ID xyz i rob notatki"*

---

## 4. Rozwiazywanie problemow

### Blad "Brak client_id w konfiguracji addonu"

Nie ustawiono sekretow OAuth. Wykonaj kroki z sekcji 2.2.

### Blad "AADSTS700016: Application not found"

Bledny `client_id` lub `tenant_id`. Sprawdz wartosci w Azure Portal → App registrations → Overview.

### Blad "AADSTS65001: User has not consented"

Administrator Azure AD nie wyrazil zgody na uprawnienia (Admin Consent).
Przejdz do Azure Portal → App registrations → API permissions → Grant admin consent.

### Blad "AADSTS50011: Reply URL does not match"

Redirect URI w Azure Portal nie zgadza sie z adresem TentaFlow. Sprawdz:
- Azure Portal → App registrations → Authentication → Redirect URIs
- Musi byc dokladnie: `https://<adres>:8090/api/addons/teams/oauth/callback`

### Token wygasl / brak dostepu

Kliknij ponownie **Zaloguj do Microsoft** w ustawieniach addonu Teams.
Tokeny sa odswiezane automatycznie, ale moga wygasnac po dluzszym okresie nieaktywnosci
lub po odwolaniu zgody w Azure AD.

### Bot nie dolacza do spotkan

1. Sprawdz czy uprawnienia Application (`Calls.JoinGroupCall.All`) sa nadane i zatwierdzone
2. Sprawdz czy w ustawieniach addonu wlaczono "Automatycznie dolaczaj do spotkan"
3. Sprawdz logi addonu w zakladce **Logi**

---

## 5. Architektura

```
Uzytkownik → TentaFlow Chat → LLM (tool calling) → Addon Teams (WASM)
                                                          ↓
                                                   Microsoft Graph API
                                                          ↓
                                              Teams / OneDrive / Calendar
```

Addon dziala jako modul WASM w sandboxie TentaFlow:
- **Izolacja** — addon nie ma dostepu do systemu plikow ani sieci poza dozwolonymi domenami
  (`graph.microsoft.com`, `login.microsoftonline.com`)
- **Sekrety** — tokeny OAuth sa szyfrowane AES-256-GCM, przechowywane per uzytkownik
- **Audyt** — wszystkie operacje sa logowane w dzienniku audytu
- **Limity** — administrator moze ograniczyc zuzycie zasobow (CPU, RAM, HTTP, tokeny LLM)

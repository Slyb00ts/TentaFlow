# Instalacja addonu SharePoint RAG

Instrukcja konfiguracji integracji TentaFlow z SharePoint do przeszukiwania i indeksowania dokumentow (RAG).
Wymaga konta administratora w Azure Active Directory (Entra ID) oraz w TentaFlow.

**WAZNE**: Ten addon uzywa osobnej rejestracji aplikacji w Azure AD (nie tej samej co addon Teams).
Addon uzywa Application permissions z client_credentials flow — nie wymaga logowania uzytkownika.

---

## 1. Rejestracja aplikacji w Azure Portal

### 1.1 Utworz rejestracje aplikacji

1. Zaloguj sie do [Azure Portal](https://portal.azure.com)
2. Przejdz do **Azure Active Directory** (Entra ID) → **App registrations** → **New registration**
3. Wypelnij formularz:
   - **Name**: `TentaFlow SharePoint RAG`
   - **Supported account types**: *Single tenant* (tylko Twoja organizacja)
   - **Redirect URI**: zostaw puste (nie jest potrzebne dla client_credentials)
4. Kliknij **Register**

### 1.2 Zapisz identyfikatory

Po rejestracji na stronie **Overview** znajdziesz:

| Pole | Gdzie wpisac w TentaFlow |
|------|---------------------------|
| **Application (client) ID** | Sekrety addonu → `client_id` |
| **Directory (tenant) ID** | Ustawienia addonu → `Azure Tenant ID` |

### 1.3 Utworz client secret

1. Przejdz do **Certificates & secrets** → **Client secrets** → **New client secret**
2. Wpisz opis (np. `TentaFlow SharePoint RAG`) i ustaw wygasniecie (zalecane: 24 miesiace)
3. Kliknij **Add**
4. **NATYCHMIAST** skopiuj wartosc sekretu (pole **Value**) — nie bedzie mozna jej pozniej podejrzec
5. Wpisz ta wartosc w TentaFlow jako sekret `client_secret`

### 1.4 Skonfiguruj uprawnienia API — Sites.Selected

**WAZNE**: Uzyj `Sites.Selected` zamiast `Sites.Read.All`. Dzieki temu aplikacja ma dostep
TYLKO do witryn jawnie wskazanych przez administratora — nie do wszystkich witryn w organizacji.

1. Przejdz do **API permissions** → **Add a permission** → **Microsoft Graph**
2. Wybierz **Application permissions** (NIE Delegated) i dodaj:

| Uprawnienie | Opis |
|-------------|------|
| `Sites.Selected` | Dostep do wybranych witryn SharePoint |

3. Kliknij **Grant admin consent for [Twoja organizacja]** — wymaga uprawnien Global Admin

> **Uwaga**: `Sites.Selected` wymaga dodatkowego kroku — nadania uprawnien per witryna
> przez Graph API (sekcja 1.5).

### 1.5 Nadaj uprawnienia per witryna (obowiazkowe)

Uprawnienie `Sites.Selected` samo w sobie nie daje dostepu do zadnej witryny.
Administrator musi jawnie nadac dostep do kazdej witryny osobno:

#### Krok 1: Pobierz ID witryny SharePoint

```bash
# Zamien hostname i sciezke na swoje wartosci
# Przyklad: https://contoso.sharepoint.com/sites/engineering
SITE_HOST="contoso.sharepoint.com"
SITE_PATH="/sites/engineering"

# Pobierz token aplikacji
TOKEN=$(curl -s -X POST \
  "https://login.microsoftonline.com/<TENANT_ID>/oauth2/v2.0/token" \
  -d "grant_type=client_credentials" \
  -d "client_id=<CLIENT_ID>" \
  -d "client_secret=<CLIENT_SECRET>" \
  -d "scope=https://graph.microsoft.com/.default" \
  | python3 -c "import sys,json; print(json.load(sys.stdin)['access_token'])")

# Pobierz site ID
curl -s -H "Authorization: Bearer $TOKEN" \
  "https://graph.microsoft.com/v1.0/sites/${SITE_HOST}:${SITE_PATH}" \
  | python3 -c "import sys,json; print(json.load(sys.stdin)['id'])"
```

#### Krok 2: Nadaj uprawnienia aplikacji do witryny

```bash
SITE_ID="<SITE_ID_Z_KROKU_1>"
APP_ID="<APPLICATION_CLIENT_ID>"
APP_DISPLAY_NAME="TentaFlow SharePoint RAG"

curl -s -X POST \
  "https://graph.microsoft.com/v1.0/sites/${SITE_ID}/permissions" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "roles": ["read"],
    "grantedToIdentities": [{
      "application": {
        "id": "'${APP_ID}'",
        "displayName": "'${APP_DISPLAY_NAME}'"
      }
    }]
  }'
```

Powtorz krok 2 dla kazdej witryny, do ktorej addon ma miec dostep.

Dostepne role:
- `read` — tylko odczyt (zalecane dla RAG)
- `write` — odczyt i zapis
- `owner` — pelny dostep

#### Krok 3: Zweryfikuj uprawnienia

```bash
curl -s -H "Authorization: Bearer $TOKEN" \
  "https://graph.microsoft.com/v1.0/sites/${SITE_ID}/permissions" \
  | python3 -m json.tool
```

---

## 2. Konfiguracja w TentaFlow

### 2.1 Ustawienia addonu

1. Otworz dashboard TentaFlow (`https://localhost:8090`)
2. Zaloguj sie jako administrator
3. Przejdz do **Aplikacje** → kliknij kafelek **SharePoint RAG**
4. W zakladce **Ustawienia** wypelnij:

| Pole | Wartosc | Opis |
|------|---------|------|
| **Azure Tenant ID** | `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx` | Directory (tenant) ID z Azure Portal |
| **URL-e witryn SharePoint** | Jeden URL na linie | Witryny do indeksowania (musza miec nadane uprawnienia z kroku 1.5) |
| **Rozszerzenia plikow** | `pdf,docx,xlsx,pptx,txt,md,csv` | Jakie typy plikow indeksowac |
| **Maks. rozmiar pliku (MB)** | `50` | Pliki wieksze beda pomijane |
| **Interwal synchronizacji** | `0` | 0 = reczna synchronizacja, >0 = automatyczna co N minut |

Przyklad URL-i witryn:
```
https://contoso.sharepoint.com/sites/engineering
https://contoso.sharepoint.com/sites/hr-documents
https://contoso.sharepoint.com/teams/project-alpha
```

5. Kliknij **Zapisz ustawienia**

### 2.2 Sekrety

Sekrety sa przechowywane w zaszyfrowanej tabeli w bazie danych (AES-256-GCM).

Ustaw sekrety przez API:

```bash
# Client ID
curl -sk -X PUT https://localhost:8090/api/addons/sharepoint-rag/secrets \
  -H "Authorization: Bearer <TOKEN>" \
  -H "Content-Type: application/json" \
  -d '{"key": "client_id", "value": "<APPLICATION_CLIENT_ID>"}'

# Client Secret
curl -sk -X PUT https://localhost:8090/api/addons/sharepoint-rag/secrets \
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

### 2.3 Uprawnienia w TentaFlow

Administrator moze kontrolowac jakie funkcje SharePoint RAG sa dostepne
dla uzytkownikow i grup w zakladce **Uprawnienia**:

| Uprawnienie | Opis | Domyslny poziom |
|-------------|------|-----------------|
| `sites_browse` | Przegladanie witryn i listy plikow | RO (odczyt) |
| `files_read` | Pobieranie zawartosci plikow | RO (odczyt) |
| `search` | Wyszukiwanie plikow i zawartosci | RO (odczyt) |
| `index_manage` | Uruchamianie i konfiguracja indeksowania | RW (odczyt/zapis) |

---

## 3. Pierwsza synchronizacja

Po skonfigurowaniu addonu uruchom pierwsza synchronizacje indeksu:

### Przez czat AI
```
Zsynchronizuj indeks plikow SharePoint
```

### Przez API
```bash
curl -sk -X POST https://localhost:8090/api/addons/sharepoint-rag/tools/sync_index \
  -H "Authorization: Bearer <TOKEN>" \
  -H "Content-Type: application/json" \
  -d '{"force": true}'
```

Pierwsza synchronizacja moze trwac dluzej (w zaleznosci od liczby plikow).
Kolejne synchronizacje sa inkrementalne — pobieraja tylko zmienione pliki.

---

## 4. Weryfikacja dzialania

### 4.1 Sprawdz status addonu

```bash
curl -sk https://localhost:8090/api/addons \
  -H "Authorization: Bearer <TOKEN>" | python3 -m json.tool
```

### 4.2 Sprawdz narzedzia

```bash
curl -sk https://localhost:8090/api/addons/sharepoint-rag/tools \
  -H "Authorization: Bearer <TOKEN>" | python3 -m json.tool
```

Oczekiwany wynik: 7 narzedzi (`list_sites`, `list_files`, `search_files`,
`get_file_content`, `get_file_info`, `list_recent_changes`, `sync_index`).

### 4.3 Testowy czat z AI

W czacie TentaFlow napisz:

```
Pokaz mi dostepne witryny SharePoint
```

Inne przykladowe komendy:
- *"Jakie pliki sa na witrynie Engineering?"*
- *"Znajdz dokumenty dotyczace polityki urlopowej"*
- *"Pokaz ostatnie zmiany na SharePoint z ostatnich 3 dni"*
- *"Pobierz zawartosc pliku regulamin.pdf"*

---

## 5. Rozwiazywanie problemow

### Blad "Brak client_id w konfiguracji addonu"

Nie ustawiono sekretow. Wykonaj kroki z sekcji 2.2.

### Blad "AADSTS700016: Application not found"

Bledny `client_id` lub `tenant_id`. Sprawdz wartosci w Azure Portal.

### Blad 403 "Brak uprawnien"

Aplikacja nie ma uprawnien `Sites.Selected` do danej witryny.
Sprawdz:
1. Czy w Azure AD nadano Application permission `Sites.Selected` z Admin Consent
2. Czy administrator nadalnadal uprawnienia per witryna (krok 1.5)

```bash
# Sprawdz uprawnienia witryny
curl -s -H "Authorization: Bearer $TOKEN" \
  "https://graph.microsoft.com/v1.0/sites/${SITE_ID}/permissions" \
  | python3 -m json.tool
```

### Blad 404 "Zasob nie znaleziony"

Bledny URL witryny SharePoint. Sprawdz:
- Czy URL jest poprawny i witryna istnieje
- Czy URL zawiera pelna sciezke (np. `/sites/nazwa` lub `/teams/nazwa`)
- Czy nie ma literowki w nazwie witryny

### Pliki nie sa indeksowane

Sprawdz:
1. Rozszerzenie pliku jest na liscie dozwolonych (ustawienia addonu)
2. Rozmiar pliku nie przekracza limitu (domyslnie 50 MB)
3. Uruchom synchronizacje z force=true: `Zsynchronizuj indeks SharePoint z pelna reindeksacja`

### Stare wyniki wyszukiwania

Uruchom ponowna synchronizacje indeksu. Jesli ustawiono interwal synchronizacji=0,
synchronizacja jest tylko reczna.

---

## 6. Architektura

```
Uzytkownik → TentaFlow Chat → LLM (tool calling) → Addon SharePoint RAG (WASM)
                                                          ↓
                                                   Microsoft Graph API
                                                   (client_credentials)
                                                          ↓
                                              SharePoint Online (Sites.Selected)
                                                          ↓
                                              Tylko skonfigurowane witryny
```

Addon dziala jako modul WASM w sandboxie TentaFlow:
- **Application permissions** — nie wymaga logowania uzytkownika, uzywa client_credentials flow
- **Sites.Selected** — dostep TYLKO do jawnie wskazanych witryn (nie do wszystkich)
- **Izolacja** — addon nie ma dostepu do systemu plikow ani sieci poza dozwolonymi domenami
- **Sekrety** — client_id i client_secret sa szyfrowane AES-256-GCM
- **Indeks** — metadane plikow sa cache'owane w storage addonu
- **Delta sync** — inkrementalna synchronizacja pobiera tylko zmiany

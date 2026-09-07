# Instalacja addonu Microsoft Outlook

Instrukcja konfiguracji integracji TentaFlow z Microsoft Outlook (poczta email).
Wymaga konta administratora w Azure Active Directory (Entra ID) oraz w TentaFlow.

---

## 1. Rejestracja aplikacji w Azure Portal

### 1.1 Istniejaca czy nowa App Registration?

Addon Outlook moze korzystac z **tej samej App Registration** co addon Teams
(ten sam client_id i client_secret). Oba addony uzywaja Delegated permissions
i osobnych tokenow OAuth per uzytkownik.

**Opcja A — ta sama App Registration** (zalecane)
- Nie trzeba tworzyc nowej rejestracji
- Wystarczy dodac uprawnienia Mail.* do istniejacych
- Dodac nowy Redirect URI dla Outlooka

**Opcja B — osobna App Registration**
- Wieksza izolacja uprawnien
- Niezalezne zarzadzanie sekretami
- Osobna zgoda administratora

Ponizsze kroki opisuja co zrobic niezaleznie od wybranej opcji.

### 1.2 Dodaj Redirect URI

W Azure Portal → App registrations → Twoja aplikacja → **Authentication** → **Add URI**:

```
https://<adres-tentaflow-ai>:8090/api/addons/outlook/oauth/callback
```

Przyklady:
- Lokalna instalacja: `https://localhost:8090/api/addons/outlook/oauth/callback`
- Serwer produkcyjny: `https://ai.twojafirma.pl:8090/api/addons/outlook/oauth/callback`

### 1.3 Dodaj uprawnienia API (Delegated permissions)

Przejdz do **API permissions** → **Add a permission** → **Microsoft Graph** → **Delegated permissions**:

| Uprawnienie | Opis | Wymagane do |
|-------------|------|-------------|
| `User.Read` | Odczyt profilu zalogowanego uzytkownika | Podstawowe — zawsze wymagane |
| `Mail.Read` | Odczyt wiadomosci email | Przegladanie i wyszukiwanie maili |
| `Mail.ReadWrite` | Odczyt i modyfikacja wiadomosci email | Oznaczanie jako przeczytane, przenoszenie |
| `Mail.Send` | Wysylanie wiadomosci email | Wysylanie i odpowiadanie na maile |
| `offline_access` | Odswiezanie tokenu bez ponownego logowania | Dlugotrwale sesje |

### 1.4 Grant Admin Consent

Kliknij **Grant admin consent for [Twoja organizacja]** — wymaga uprawnien Global Admin.

> **Uwaga**: Jesli nie masz uprawnien Global Admin, popros administratora organizacji
> o wyrazenie zgody (Admin Consent). Bez tego uzytkownicy nie beda mogli sie autoryzowac.

### 1.5 Incremental Consent

Jesli uzytkownik jest juz zalogowany przez addon Teams (ta sama App Registration),
Outlook poprosi go o **dodatkowe uprawnienia** (Mail.Read, Mail.ReadWrite, Mail.Send)
przy pierwszym logowaniu. To normalne zachowanie — Azure AD obsluguje incremental consent.

Kazdy addon ma **osobny token OAuth** — logowanie w Teams nie daje dostepu do Outlooka
i odwrotnie. Uzytkownik musi zalogowac sie osobno w kazdym addonie.

---

## 2. Konfiguracja w TentaFlow

### 2.1 Ustawienia addonu

1. Otworz dashboard TentaFlow (`https://localhost:8090`)
2. Zaloguj sie jako administrator
3. Przejdz do **Aplikacje** → kliknij kafelek **Microsoft Outlook**
4. W zakladce **Ustawienia** wypelnij:

| Pole | Wartosc | Opis |
|------|---------|------|
| **Azure Tenant ID** | `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx` | Directory (tenant) ID z Azure Portal |
| **Domyslna liczba wynikow** | `20` | Ile maili pobierac domyslnie (1-50) |
| **Powiadomienia o nowych mailach** | Wl/Wyl | Automatyczne powiadomienia o nowych mailach |

5. Kliknij **Zapisz ustawienia**

### 2.2 Sekrety OAuth

Sekrety sa przechowywane w zaszyfrowanej tabeli w bazie danych (AES-256-GCM).
Nie sa widoczne w konfiguracji addonu.

Ustaw sekrety przez API (lub przez przyszly panel sekretow w UI):

```bash
# Client ID (ten sam co Teams jesli wspolna App Registration)
curl -sk -X PUT https://localhost:8090/api/addons/outlook/secrets \
  -H "Authorization: Bearer <TOKEN>" \
  -H "Content-Type: application/json" \
  -d '{"key": "client_id", "value": "<APPLICATION_CLIENT_ID>"}'

# Client Secret (ten sam co Teams jesli wspolna App Registration)
curl -sk -X PUT https://localhost:8090/api/addons/outlook/secrets \
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

1. W zakladce **Ustawienia** addonu Outlook kliknij przycisk **Zaloguj do Microsoft**
2. Otworzy sie okno logowania Microsoft — zaloguj sie kontem organizacyjnym
3. Zaakceptuj wymagane uprawnienia (Mail.Read, Mail.ReadWrite, Mail.Send)
4. Po powrocie na strone TentaFlow, tokeny zostana zapisane automatycznie

Token jest odswiezany automatycznie (dzieki `offline_access`). Jesli wygasnie,
uzytkownik zostanie poproszony o ponowna autoryzacje.

> **Uwaga**: Jesli uzytkownik jest juz zalogowany w addonie Teams z ta sama
> App Registration, Outlook poprosi jedynie o dodatkowe uprawnienia pocztowe
> (incremental consent). To osobne logowanie — osobny token.

### 2.5 Uprawnienia w TentaFlow

Administrator moze kontrolowac jakie funkcje Outlook sa dostepne dla uzytkownikow
i grup w zakladce **Uprawnienia**:

| Uprawnienie | Opis | Domyslny poziom |
|-------------|------|-----------------|
| `mail_read` | Odczyt wiadomosci email | RO (odczyt) |
| `mail_write` | Wysylanie wiadomosci email | RW (odczyt/zapis) |
| `mail_delete` | Usuwanie wiadomosci email | RWD (pelny dostep) |
| `mail_search` | Wyszukiwanie wiadomosci email | RO (odczyt) |
| `folders_read` | Przegladanie folderow poczty | RO (odczyt) |
| `attachments_read` | Pobieranie zalacznikow | RO (odczyt) |
| `notifications` | Powiadomienia o nowych mailach | RW (odczyt/zapis) |
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

Po autoryzacji OAuth, narzedzia Outlook sa dostepne dla LLM:

```bash
curl -sk https://localhost:8090/api/addons/outlook/tools \
  -H "Authorization: Bearer <TOKEN>" | python3 -m json.tool
```

Oczekiwany wynik: 7 narzedzi (`list_emails`, `read_email`, `search_emails`,
`send_email`, `reply_email`, `list_folders`, `get_attachment`).

### 3.3 Testowy czat z AI

W czacie TentaFlow napisz:

```
Pokaz mi ostatnie maile ze skrzynki odbiorczej
```

AI powinno uzyc narzedzia `outlook.list_emails` i zwrocic liste maili.

Inne przykladowe komendy:
- *"Ile mam nieprzeczytanych maili?"*
- *"Znajdz maile od jan.kowalski@firma.pl"*
- *"Wyslij maila do ania@firma.pl z tematem: Spotkanie w piatek"*
- *"Odpowiedz na ostatni mail od szefa: dziekuje, potwierdzam"*
- *"Pokaz mi foldery poczty"*
- *"Pobierz zalacznik raport.pdf z maila o projekcie"*

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
- Musi byc dokladnie: `https://<adres>:8090/api/addons/outlook/oauth/callback`

### Blad "Insufficient privileges" / 403 Forbidden

Brak uprawnien Mail.Read, Mail.ReadWrite lub Mail.Send w App Registration.
Dodaj uprawnienia i zatwierdz Admin Consent (sekcja 1.3 i 1.4).

### Token wygasl / brak dostepu

Kliknij ponownie **Zaloguj do Microsoft** w ustawieniach addonu Outlook.
Tokeny sa odswiezane automatycznie, ale moga wygasnac po dluzszym okresie nieaktywnosci
lub po odwolaniu zgody w Azure AD.

### Maile sie nie wyswietlaja (pusty wynik)

1. Sprawdz czy uzytkownik ma maile w podanym folderze
2. Sprawdz czy filtr OData jest poprawny skladniowo
3. Sprawdz logi addonu w zakladce **Logi**
4. Sprawdz czy uprawnienie `mail_read` jest nadane w TentaFlow

---

## 5. Architektura

```
Uzytkownik → TentaFlow Chat → LLM (tool calling) → Addon Outlook (WASM)
                                                          |
                                                   Microsoft Graph API
                                                          |
                                              Exchange Online / Outlook
```

Addon dziala jako modul WASM w sandboxie TentaFlow:
- **Izolacja** — addon nie ma dostepu do systemu plikow ani sieci poza dozwolonymi domenami
  (`graph.microsoft.com`, `login.microsoftonline.com`)
- **Sekrety** — tokeny OAuth sa szyfrowane AES-256-GCM, przechowywane per uzytkownik
- **Audyt** — wszystkie operacje sa logowane w dzienniku audytu
- **Limity** — administrator moze ograniczyc zuzycie zasobow (CPU, RAM, HTTP, tokeny LLM)
- **Osobne tokeny** — kazdy addon (Teams, Outlook) ma wlasny token OAuth per uzytkownik,
  nawet jesli uzywa tej samej App Registration

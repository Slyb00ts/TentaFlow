# Microsoft Teams

Pelna integracja z Microsoft Teams dla TentaFlow.

## Funkcje
- **Wiadomosci**: Czytanie i wysylanie wiadomosci na czatach i kanalach
- **Kalendarz**: Przegladanie spotkan i wydarzen
- **Pliki**: Dostep do plikow OneDrive i SharePoint
- **Spotkania**: Bot AI dolacza do spotkan Teams
  - Transkrypcja w czasie rzeczywistym (STT)
  - Odpowiadanie na pytania glosowo (TTS + LLM)
  - Automatyczne notatki ze spotkania

## Konfiguracja
1. Zarejestruj aplikacje w Azure AD (portal.azure.com)
2. Dodaj uprawnienia: User.Read, Chat.ReadWrite, Calendars.Read, Files.Read, OnlineMeetings.ReadWrite
3. Wpisz Tenant ID, Client ID i Client Secret w ustawieniach addonu
4. Zaloguj sie przez OAuth

## Wymagania
- Konto Microsoft 365
- Rejestracja aplikacji w Azure AD
- Serwisy STT i TTS na routerze (dla bota na spotkaniach)

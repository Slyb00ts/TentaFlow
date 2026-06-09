# IBM WxMCPServer

Pakiet webMethods Integration Server (Microservices Runtime, MSR) wystawiajacy
istniejace API jako narzedzia MCP. Klienci MCP laczą sie z endpointem `/mcp` na
porcie HTTP Integration Server (domyslnie **5555**).

Repo zrodlowe: https://github.com/IBM/WxMCPServer (licencja Apache-2.0).

## Obraz bazowy jest licencjonowany

Obraz buduje sie na bazie:

```
ibmwebmethods.azurecr.io/webmethods-microservicesruntime:11.1
```

Ten obraz lezy w **prywatnym** rejestrze IBM (Azure Container Registry) i jest
licencjonowany. `docker build` najpierw pociagnie warstwe bazowa, wiec bez
loginu do tego rejestru build sie nie powiedzie.

Login do rejestru wprowadza administrator w TentaFlow:
**Ustawienia -> Dostepy zewnetrzne -> rejestry kontenerow**.

## Build

Wymagany wczesniejszy login do `ibmwebmethods.azurecr.io`. Nastepnie:

```bash
docker pull ibmwebmethods.azurecr.io/webmethods-microservicesruntime:11.1
docker build -t wxmcpserver .
```

Opcjonalnie mozna wskazac referencje repo pakietu (domyslnie `main`):

```bash
docker build --build-arg WXMCP_REF=main -t wxmcpserver .
```

## Konfiguracja runtime (global variables IS)

WxMCPServer konfiguruje sie przez **global variables** Integration Servera. Co
najmniej:

- `wxmcp.auth.type` — tryb uwierzytelniania: `OAUTH` | `API_KEY` | `INTERNAL`
  | `THIRD_PARTY`.
- `wxmcp.tool.catalog.base.url` — URL katalogu API, z ktorego pakiet generuje
  narzedzia MCP.

Wartosci te ustawia administrator zgodnie z dokumentacja WxMCPServer / MSR.
Dokladny mechanizm wstrzykiwania tych global variables do dzialajacego
Integration Servera (np. konsola administracyjna IS, plik konfiguracyjny czy
zmienne srodowiskowe MSR) zalezy od konfiguracji MSR i nie jest tu zakladany —
nalezy postepowac wedlug oficjalnej dokumentacji webMethods.

## Porty

- **5555** — HTTP Integration Server, endpoint MCP `/mcp`.
- **9999** — port diagnostyczny Integration Server.

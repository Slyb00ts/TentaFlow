# =============================================================================
# Plik: Dockerfile
# Opis: Obraz IBM WxMCPServer dla TentaFlow — pakiet webMethods Integration
#       Server (Microservices Runtime) wystawiajacy istniejace API jako
#       narzedzia MCP pod endpointem /mcp.
# Przykład: docker build -t wxmcpserver .
# =============================================================================

# Stage fetch: pobiera tresc pakietu IS z publicznego repozytorium IBM.
# Lekki obraz tylko z gitem, klon plytki (depth 1) wybranej referencji.
FROM alpine/git AS fetch
ARG WXMCP_REF=main
RUN git clone --depth 1 --branch "${WXMCP_REF}" \
        https://github.com/IBM/WxMCPServer.git /src

# Stage finalny: oficjalny obraz bazowy webMethods Microservices Runtime 11.1.
# UWAGA: obraz bazowy lezy w PRYWATNYM rejestrze IBM (ibmwebmethods.azurecr.io,
# Azure Container Registry) i jest licencjonowany. Bez wczesniejszego loginu do
# tego rejestru `docker build` nie pociagnie warstwy bazowej i przerwie sie na
# tym FROM. Login admin wprowadza w TentaFlow: Ustawienia -> Dostepy zewnetrzne
# -> rejestry kontenerow. To zewnetrzna zaleznosc admina — nie obchodzimy jej.
FROM ibmwebmethods.azurecr.io/webmethods-microservicesruntime:11.1

# Operacje plikowe wykonujemy jako root, zeby ustawic wlasciciela sagadmin na
# skopiowanej zawartosci pakietu.
USER root

# Kopiujemy WYLACZNIE tresc pakietu IS (bez .git) do katalogu packages IS.
COPY --from=fetch --chown=sagadmin:sagadmin /src/code \
        /opt/softwareag/IntegrationServer/packages/WxMCPServer/code
COPY --from=fetch --chown=sagadmin:sagadmin /src/config \
        /opt/softwareag/IntegrationServer/packages/WxMCPServer/config
COPY --from=fetch --chown=sagadmin:sagadmin /src/ns \
        /opt/softwareag/IntegrationServer/packages/WxMCPServer/ns
COPY --from=fetch --chown=sagadmin:sagadmin /src/pub \
        /opt/softwareag/IntegrationServer/packages/WxMCPServer/pub
COPY --from=fetch --chown=sagadmin:sagadmin /src/resources \
        /opt/softwareag/IntegrationServer/packages/WxMCPServer/resources
COPY --from=fetch --chown=sagadmin:sagadmin /src/manifest.v3 \
        /opt/softwareag/IntegrationServer/packages/WxMCPServer/manifest.v3

# Wracamy do uzytkownika runtime obrazu bazowego.
USER sagadmin

# 5555 = HTTP Integration Server (endpoint /mcp), 9999 = port diagnostyczny IS.
EXPOSE 5555 9999

CMD ["/opt/softwareag/IntegrationServer/bin/startContainer.sh"]

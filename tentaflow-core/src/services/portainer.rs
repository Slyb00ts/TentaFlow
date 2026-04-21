// =============================================================================
// Plik: services/portainer.rs
// Opis: Klient Portainer REST API do zarzadzania kontenerami na zdalnych hostach.
//       Obsluguje endpointy, stacki, kontenery oraz logi. Wspiera dwa typy auth:
//       API Access Tokens (ptr_...) przez X-API-Key oraz JWT przez Authorization: Bearer.
// =============================================================================

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{debug, error, warn};

// =============================================================================
// Konfiguracja
// =============================================================================

/// Konfiguracja polaczenia z Portainer
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortainerConfig {
    /// URL bazowy Portainer (np. "https://portainer.example.com")
    pub base_url: String,
    /// Klucz API Portainer
    pub api_key: String,
    /// Nazwa wyswietlana (do logowania i diagnostyki)
    pub display_name: String,
}

// =============================================================================
// Klient
// =============================================================================

/// Klient Portainer REST API v2
///
/// Zapewnia bezpieczna komunikacje z Portainer przez HTTPS.
/// Akceptuje self-signed certyfikaty (typowe dla wewnetrznych instalacji Portainer).
pub struct PortainerClient {
    config: PortainerConfig,
    client: reqwest::Client,
}

impl PortainerClient {
    /// Tworzy nowy klient Portainer.
    ///
    /// Konfiguruje reqwest::Client z:
    /// - Akceptacja self-signed certyfikatow (Portainer czesto uzywa self-signed)
    /// - Timeout 30 sekund na request
    /// - Connection pooling (domyslny reqwest)
    pub fn new(config: PortainerConfig) -> Result<Self> {
        let mut normalized = config;
        // Normalizuj URL - usun trailing slash i /api
        normalized.base_url = normalized.base_url.trim_end_matches('/').to_string();
        if normalized.base_url.ends_with("/api") {
            normalized.base_url = normalized.base_url[..normalized.base_url.len() - 4].to_string();
        }

        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .context("Nie udalo sie utworzyc klienta HTTP dla Portainer")?;

        debug!(
            "Portainer client utworzony: {} ({})",
            normalized.display_name, normalized.base_url
        );

        Ok(Self {
            config: normalized,
            client,
        })
    }

    /// Ustawia naglowek autoryzacji na podstawie typu klucza API.
    /// Tokeny ptr_... uzywaja X-API-Key, pozostale (JWT) uzywaja Authorization: Bearer.
    fn apply_auth(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if self.config.api_key.starts_with("ptr_") {
            request.header("X-API-Key", &self.config.api_key)
        } else {
            request.header("Authorization", format!("Bearer {}", self.config.api_key))
        }
    }

    // =========================================================================
    // Metody HTTP z automatyczna autentykacja
    // =========================================================================

    /// Wykonuje GET request z automatyczna autoryzacja
    async fn get(&self, path: &str) -> Result<reqwest::Response> {
        let url = format!("{}{}", self.config.base_url.trim_end_matches('/'), path);
        debug!("Portainer GET: {}", url);

        let request = self.client.get(&url);
        let response = self
            .apply_auth(request)
            .send()
            .await
            .with_context(|| format!("Blad polaczenia z Portainer: {}", url))?;

        Self::check_response_status(&url, response).await
    }

    /// Wykonuje POST request z automatyczna autoryzacja i body JSON
    async fn post<T: Serialize>(&self, path: &str, body: &T) -> Result<reqwest::Response> {
        let url = format!("{}{}", self.config.base_url.trim_end_matches('/'), path);
        debug!("Portainer POST: {}", url);

        let request = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(body);
        let response = self
            .apply_auth(request)
            .send()
            .await
            .with_context(|| format!("Blad polaczenia z Portainer: {}", url))?;

        Self::check_response_status(&url, response).await
    }

    /// Wykonuje POST request bez body (akcje na kontenerach)
    async fn post_empty(&self, path: &str) -> Result<reqwest::Response> {
        let url = format!("{}{}", self.config.base_url.trim_end_matches('/'), path);
        debug!("Portainer POST (empty): {}", url);

        let request = self.client.post(&url);
        let response = self
            .apply_auth(request)
            .send()
            .await
            .with_context(|| format!("Blad polaczenia z Portainer: {}", url))?;

        Self::check_response_status(&url, response).await
    }

    /// Wykonuje DELETE request z automatyczna autoryzacja
    async fn delete(&self, path: &str) -> Result<reqwest::Response> {
        let url = format!("{}{}", self.config.base_url.trim_end_matches('/'), path);
        debug!("Portainer DELETE: {}", url);

        let request = self.client.delete(&url);
        let response = self
            .apply_auth(request)
            .send()
            .await
            .with_context(|| format!("Blad polaczenia z Portainer: {}", url))?;

        Self::check_response_status(&url, response).await
    }

    /// Sprawdza status odpowiedzi HTTP i loguje bledy
    async fn check_response_status(
        url: &str,
        response: reqwest::Response,
    ) -> Result<reqwest::Response> {
        let status = response.status();
        let final_url = response.url().to_string();

        if status.is_success() {
            return Ok(response);
        }

        // Wykryj redirect (URL odpowiedzi rozni sie od URL requestu)
        if final_url != url {
            warn!("Portainer: nastapil redirect {} -> {}", url, final_url);
        }

        let error_body = response.text().await.unwrap_or_default();

        match status.as_u16() {
            401 | 403 => {
                error!(
                    "Portainer: brak autoryzacji ({}) dla {}. Odpowiedz: {}",
                    status, final_url, error_body
                );
                anyhow::bail!(
                    "Portainer: brak autoryzacji ({}) - {}. URL: {}",
                    status,
                    if error_body.is_empty() {
                        "sprawdz klucz API".to_string()
                    } else {
                        error_body
                    },
                    final_url
                );
            }
            404 => {
                warn!("Portainer: zasob nie znaleziony (404): {}", url);
                anyhow::bail!("Portainer: zasob nie znaleziony (404): {}", url);
            }
            409 => {
                warn!("Portainer: konflikt (409): {} - {}", url, error_body);
                anyhow::bail!("Portainer: konflikt (409): {} - {}", url, error_body);
            }
            500..=599 => {
                error!(
                    "Portainer: blad serwera ({}) dla {}: {}",
                    status, url, error_body
                );
                anyhow::bail!(
                    "Portainer: blad serwera ({}) dla {}: {}",
                    status,
                    url,
                    error_body
                );
            }
            _ => {
                error!(
                    "Portainer: nieoczekiwany status ({}) dla {}: {}",
                    status, url, error_body
                );
                anyhow::bail!(
                    "Portainer: nieoczekiwany status ({}) dla {}: {}",
                    status,
                    url,
                    error_body
                );
            }
        }
    }

    // =========================================================================
    // Endpointy (hosty zarzadzane przez Portainer)
    // =========================================================================

    /// Pobiera liste endpointow (hostow) zarzadzanych przez Portainer.
    ///
    /// Kazdy endpoint reprezentuje maszyne z Docker Engine lub Portainer Agent.
    /// Status: 1 = aktywny, 2 = nieaktywny.
    pub async fn list_endpoints(&self) -> Result<Vec<PortainerEndpoint>> {
        let response = self.get("/api/endpoints").await?;

        let endpoints: Vec<PortainerEndpoint> = response
            .json()
            .await
            .context("Nie udalo sie zdekodowac odpowiedzi z listy endpointow")?;

        debug!(
            "Portainer '{}': pobrano {} endpointow",
            self.config.display_name,
            endpoints.len()
        );

        Ok(endpoints)
    }

    // =========================================================================
    // Stacki (Docker Compose)
    // =========================================================================

    /// Pobiera liste stackow na danym endpoincie.
    ///
    /// Filtruje stacki po EndpointID - zwraca tylko stacki przypisane do danego hosta.
    pub async fn list_stacks(&self, endpoint_id: i64) -> Result<Vec<PortainerStack>> {
        let path = format!("/api/stacks?filters={{\"EndpointID\":{}}}", endpoint_id);

        let response = self.get(&path).await?;

        let stacks: Vec<PortainerStack> = response
            .json()
            .await
            .context("Nie udalo sie zdekodowac odpowiedzi z listy stackow")?;

        debug!(
            "Portainer '{}': pobrano {} stackow dla endpointu {}",
            self.config.display_name,
            stacks.len(),
            endpoint_id
        );

        Ok(stacks)
    }

    /// Deployuje nowy stack (Docker Compose) na endpoincie.
    ///
    /// Tworzy stack typu standalone (type=2) z podana zawartocia compose YAML.
    /// Jesli stack o tej nazwie juz istnieje, Portainer zwroci blad 409 (conflict).
    pub async fn deploy_stack(
        &self,
        endpoint_id: i64,
        name: &str,
        compose_content: &str,
    ) -> Result<PortainerStack> {
        let path = format!(
            "/api/stacks/create/standalone/string?type=2&method=string&endpointId={}",
            endpoint_id
        );

        let body = DeployStackRequest {
            name: name.to_string(),
            stack_file_content: compose_content.to_string(),
        };

        debug!(
            "Portainer '{}': deploy stack '{}' na endpoincie {}",
            self.config.display_name, name, endpoint_id
        );

        let response = self.post(&path, &body).await?;

        let stack: PortainerStack = response
            .json()
            .await
            .context("Nie udalo sie zdekodowac odpowiedzi po deployu stacka")?;

        debug!(
            "Portainer '{}': stack '{}' (id={}) wdrozony pomyslnie",
            self.config.display_name, stack.name, stack.id
        );

        Ok(stack)
    }

    /// Usuwa stack z Portainer.
    ///
    /// Wymaga stack_id (nie nazwy) i endpoint_id do ktorego stack jest przypisany.
    /// Zatrzymuje i usuwa wszystkie kontenery nalezace do stacka.
    pub async fn remove_stack(&self, stack_id: i64, endpoint_id: i64) -> Result<()> {
        let path = format!("/api/stacks/{}?endpointId={}", stack_id, endpoint_id);

        debug!(
            "Portainer '{}': usuwanie stacka {} z endpointu {}",
            self.config.display_name, stack_id, endpoint_id
        );

        self.delete(&path).await?;

        debug!(
            "Portainer '{}': stack {} usuniety pomyslnie",
            self.config.display_name, stack_id
        );

        Ok(())
    }

    // =========================================================================
    // Kontenery Docker
    // =========================================================================

    /// Pobiera liste kontenerow na danym endpoincie.
    ///
    /// Zwraca wszystkie kontenery (w tym zatrzymane) - parametr all=true.
    pub async fn list_containers(&self, endpoint_id: i64) -> Result<Vec<PortainerContainer>> {
        let path = format!(
            "/api/endpoints/{}/docker/containers/json?all=true",
            endpoint_id
        );

        let response = self.get(&path).await?;

        let containers: Vec<PortainerContainer> = response
            .json()
            .await
            .context("Nie udalo sie zdekodowac odpowiedzi z listy kontenerow")?;

        debug!(
            "Portainer '{}': pobrano {} kontenerow z endpointu {}",
            self.config.display_name,
            containers.len(),
            endpoint_id
        );

        Ok(containers)
    }

    /// Wykonuje akcje na kontenerze (start/stop/restart/kill).
    ///
    /// Dopuszczalne akcje: "start", "stop", "restart", "kill".
    /// Portainer przekazuje akcje bezposrednio do Docker Engine.
    pub async fn container_action(
        &self,
        endpoint_id: i64,
        container_id: &str,
        action: ContainerAction,
    ) -> Result<()> {
        let action_str = action.as_str();
        let path = format!(
            "/api/endpoints/{}/docker/containers/{}/{}",
            endpoint_id, container_id, action_str
        );

        debug!(
            "Portainer '{}': {} kontenera {} na endpoincie {}",
            self.config.display_name, action_str, container_id, endpoint_id
        );

        self.post_empty(&path).await?;

        debug!(
            "Portainer '{}': akcja {} na kontenerze {} zakonczona pomyslnie",
            self.config.display_name, action_str, container_id
        );

        Ok(())
    }

    /// Pobiera logi kontenera (stdout + stderr).
    ///
    /// Parametr `tail` okresla ile ostatnich linii pobrac (0 = wszystkie).
    /// Portainer zwraca logi jako plain text z Docker Engine API.
    pub async fn container_logs(
        &self,
        endpoint_id: i64,
        container_id: &str,
        tail: u32,
    ) -> Result<String> {
        let path = format!(
            "/api/endpoints/{}/docker/containers/{}/logs?stdout=true&stderr=true&tail={}",
            endpoint_id, container_id, tail
        );

        debug!(
            "Portainer '{}': pobieranie logow kontenera {} (tail={})",
            self.config.display_name, container_id, tail
        );

        let response = self.get(&path).await?;

        let logs = response
            .text()
            .await
            .context("Nie udalo sie odczytac logow kontenera")?;

        // Docker logs zawieraja 8-bajtowy naglowek dla kazdej linii (stream type + length).
        // Usuwamy go zeby zwrocic czytelny tekst.
        let cleaned = strip_docker_log_headers(&logs);

        debug!(
            "Portainer '{}': pobrano {} bajtow logow kontenera {}",
            self.config.display_name,
            cleaned.len(),
            container_id
        );

        Ok(cleaned)
    }

    /// Zwraca nazwe wyswietlana klienta (do logowania)
    pub fn display_name(&self) -> &str {
        &self.config.display_name
    }

    /// Zwraca URL bazowy Portainer
    pub fn base_url(&self) -> &str {
        &self.config.base_url
    }
}

// =============================================================================
// Typy Portainer API
// =============================================================================

/// Endpoint Portainer - host zarzadzany przez Portainer
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortainerEndpoint {
    #[serde(rename = "Id")]
    pub id: i64,
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "URL")]
    pub url: String,
    /// 1 = aktywny, 2 = nieaktywny
    #[serde(rename = "Status")]
    pub status: i32,
    /// 1 = docker, 2 = agent, 3 = azure, 4 = edge agent, 5 = edge agent async
    #[serde(rename = "Type")]
    pub endpoint_type: i32,
}

impl PortainerEndpoint {
    /// Sprawdza czy endpoint jest aktywny (status == 1)
    pub fn is_up(&self) -> bool {
        self.status == 1
    }
}

/// Stack Portainer - wdrozenie Docker Compose
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortainerStack {
    #[serde(rename = "Id")]
    pub id: i64,
    #[serde(rename = "Name")]
    pub name: String,
    /// 1 = aktywny, 2 = nieaktywny
    #[serde(rename = "Status")]
    pub status: i32,
    #[serde(rename = "EndpointId")]
    pub endpoint_id: i64,
}

impl PortainerStack {
    /// Sprawdza czy stack jest aktywny (status == 1)
    pub fn is_active(&self) -> bool {
        self.status == 1
    }
}

/// Kontener Docker widoczny przez Portainer
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortainerContainer {
    #[serde(rename = "Id")]
    pub id: String,
    #[serde(rename = "Names")]
    pub names: Vec<String>,
    #[serde(rename = "Image")]
    pub image: String,
    /// Stan kontenera (np. "running", "exited", "created")
    #[serde(rename = "State")]
    pub state: String,
    /// Opis stanu (np. "Up 2 hours", "Exited (0) 3 minutes ago")
    #[serde(rename = "Status")]
    pub status: String,
}

impl PortainerContainer {
    /// Sprawdza czy kontener jest uruchomiony
    pub fn is_running(&self) -> bool {
        self.state == "running"
    }

    /// Zwraca nazwe kontenera (bez wiodacego '/')
    pub fn display_name(&self) -> &str {
        self.names
            .first()
            .map(|n| n.strip_prefix('/').unwrap_or(n))
            .unwrap_or(&self.id)
    }
}

// =============================================================================
// Typy wewnetrzne
// =============================================================================

/// Akcja do wykonania na kontenerze
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerAction {
    Start,
    Stop,
    Restart,
    Kill,
}

impl ContainerAction {
    fn as_str(&self) -> &'static str {
        match self {
            ContainerAction::Start => "start",
            ContainerAction::Stop => "stop",
            ContainerAction::Restart => "restart",
            ContainerAction::Kill => "kill",
        }
    }
}

/// Body requesta do deploymentu stacka
#[derive(Serialize)]
struct DeployStackRequest {
    name: String,
    #[serde(rename = "stackFileContent")]
    stack_file_content: String,
}

// =============================================================================
// Funkcje pomocnicze
// =============================================================================

/// Usuwa 8-bajtowe naglowki Docker log stream z tekstu logow.
///
/// Docker Engine API zwraca logi z naglowkami:
/// - bajt 0: typ streamu (1=stdout, 2=stderr)
/// - bajty 1-3: padding (0x00)
/// - bajty 4-7: dlugosc danych (big-endian u32)
/// - bajty 8+: dane tekstowe
///
/// Ta funkcja czysci logi do czytelnego plain text.
fn strip_docker_log_headers(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut result = Vec::with_capacity(bytes.len());
    let mut pos = 0;

    while pos < bytes.len() {
        // Sprawdz czy mamy pelny 8-bajtowy naglowek
        if pos + 8 <= bytes.len() {
            let stream_type = bytes[pos];
            // Docker stream type: 0=stdin, 1=stdout, 2=stderr
            if (stream_type <= 2)
                && bytes[pos + 1] == 0
                && bytes[pos + 2] == 0
                && bytes[pos + 3] == 0
            {
                let length = u32::from_be_bytes([
                    bytes[pos + 4],
                    bytes[pos + 5],
                    bytes[pos + 6],
                    bytes[pos + 7],
                ]) as usize;

                let data_start = pos + 8;
                let data_end = (data_start + length).min(bytes.len());

                if data_start <= bytes.len() {
                    result.extend_from_slice(&bytes[data_start..data_end]);
                    pos = data_end;
                    continue;
                }
            }
        }

        // Fallback: kopiuj bajt bez zmian (logi bez naglowkow Docker)
        result.push(bytes[pos]);
        pos += 1;
    }

    String::from_utf8_lossy(&result).into_owned()
}

// =============================================================================
// Testy
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_container_action_as_str() {
        assert_eq!(ContainerAction::Start.as_str(), "start");
        assert_eq!(ContainerAction::Stop.as_str(), "stop");
        assert_eq!(ContainerAction::Restart.as_str(), "restart");
        assert_eq!(ContainerAction::Kill.as_str(), "kill");
    }

    #[test]
    fn test_portainer_endpoint_is_up() {
        let endpoint = PortainerEndpoint {
            id: 1,
            name: "test".into(),
            url: "tcp://localhost:2375".into(),
            status: 1,
            endpoint_type: 1,
        };
        assert!(endpoint.is_up());

        let down = PortainerEndpoint {
            status: 2,
            ..endpoint
        };
        assert!(!down.is_up());
    }

    #[test]
    fn test_portainer_stack_is_active() {
        let stack = PortainerStack {
            id: 1,
            name: "test-stack".into(),
            status: 1,
            endpoint_id: 1,
        };
        assert!(stack.is_active());

        let inactive = PortainerStack { status: 2, ..stack };
        assert!(!inactive.is_active());
    }

    #[test]
    fn test_container_display_name() {
        let container = PortainerContainer {
            id: "abc123".into(),
            names: vec!["/my-container".into()],
            image: "nginx:latest".into(),
            state: "running".into(),
            status: "Up 2 hours".into(),
        };
        assert_eq!(container.display_name(), "my-container");
        assert!(container.is_running());
    }

    #[test]
    fn test_container_display_name_without_slash() {
        let container = PortainerContainer {
            id: "abc123".into(),
            names: vec!["my-container".into()],
            image: "nginx:latest".into(),
            state: "running".into(),
            status: "Up 2 hours".into(),
        };
        assert_eq!(container.display_name(), "my-container");
    }

    #[test]
    fn test_container_display_name_empty_names() {
        let container = PortainerContainer {
            id: "abc123".into(),
            names: vec![],
            image: "nginx:latest".into(),
            state: "exited".into(),
            status: "Exited (0)".into(),
        };
        assert_eq!(container.display_name(), "abc123");
        assert!(!container.is_running());
    }

    #[test]
    fn test_strip_docker_log_headers_with_headers() {
        // Symulacja naglowka Docker: stdout (1), padding (0,0,0), length=5, dane="hello"
        let mut raw = vec![1u8, 0, 0, 0, 0, 0, 0, 5];
        raw.extend_from_slice(b"hello");
        let raw_str = String::from_utf8_lossy(&raw).into_owned();

        let result = strip_docker_log_headers(&raw_str);
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_strip_docker_log_headers_plain_text() {
        // Tekst bez naglowkow Docker - powinien przejsc bez zmian
        let plain = "zwykly tekst logow\ndruga linia\n";
        let result = strip_docker_log_headers(plain);
        assert_eq!(result, plain);
    }

    #[test]
    fn test_deploy_stack_request_serialization() {
        let request = DeployStackRequest {
            name: "test-stack".into(),
            stack_file_content: "version: '3'\nservices:\n  web:\n    image: nginx".into(),
        };

        let json = serde_json::to_string(&request).expect("serializacja powinna sie udac");
        assert!(json.contains("\"name\":\"test-stack\""));
        assert!(json.contains("\"stackFileContent\""));
    }
}

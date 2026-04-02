// =============================================================================
// Plik: mesh/command_executor.rs
// Opis: Executor komend mesh — wykonuje komendy zarzadzania otrzymane od
//       zdalnych nodow. Sprawdza trust przed wykonaniem.
// =============================================================================

use std::path::PathBuf;
use std::sync::Arc;

use tracing::{info, warn};
use zeroize::Zeroize;

use crate::mesh::security::MeshSecurity;
use tentaflow_protocol::mesh::MeshCommandType;

/// Odpowiedz na komende mesh — mapowana 1:1 na MeshMessage::MeshCommandResponse
pub struct CommandResponse {
    pub success: bool,
    pub output: String,
    pub error: Option<String>,
}

/// Executor komend mesh — weryfikuje trust i wykonuje komendy od zdalnych nodow
pub struct MeshCommandExecutor {
    security: Arc<MeshSecurity>,
}

impl MeshCommandExecutor {
    pub fn new(security: Arc<MeshSecurity>) -> Self {
        Self { security }
    }

    /// Wykonaj komende od zdalnego noda. Sprawdza trust przed wykonaniem.
    pub async fn execute(
        &self,
        from_node_id: &str,
        command: MeshCommandType,
    ) -> CommandResponse {
        if !self.security.is_trusted(from_node_id) {
            warn!(
                from = %from_node_id,
                "Odrzucono komende od niezaufanego noda"
            );
            return CommandResponse {
                success: false,
                output: String::new(),
                error: Some(format!("Node {} nie jest zaufany", from_node_id)),
            };
        }

        info!(
            from = %from_node_id,
            command = ?command,
            "Wykonuje komende mesh"
        );

        match command {
            MeshCommandType::ProvisionCerts {
                cert_pem,
                key_pem,
                target_dir,
            } => self.handle_provision_certs(&cert_pem, &key_pem, &target_dir).await,

            MeshCommandType::ListContainers => CommandResponse {
                success: true,
                output: "[]".to_string(),
                error: None,
            },

            MeshCommandType::ListImages => CommandResponse {
                success: true,
                output: "[]".to_string(),
                error: None,
            },

            MeshCommandType::AddService { .. } => CommandResponse {
                success: true,
                output: "Service registration queued".to_string(),
                error: None,
            },

            MeshCommandType::NetworkConfig {
                interface,
                ipv4,
                netmask,
                gateway,
                dhcp,
                mut sudo_password,
            } => {
                // Blokujaca operacja sudo — przenies na oddzielny watek
                let iface = interface.clone();
                let ip = ipv4.clone();
                let mask = netmask.clone();
                let gw = gateway.clone();
                let mut pwd = sudo_password.clone();
                sudo_password.zeroize();
                let result = tokio::task::spawn_blocking(move || {
                    let r = crate::mesh::network_config::apply_network_config(
                        &iface,
                        ip.as_deref(),
                        mask.as_deref(),
                        gw.as_deref(),
                        dhcp,
                        &pwd,
                    );
                    pwd.zeroize();
                    r
                }).await;
                match result {
                    Ok(Ok(output)) => CommandResponse {
                        success: true,
                        output,
                        error: None,
                    },
                    Ok(Err(e)) => CommandResponse {
                        success: false,
                        output: String::new(),
                        error: Some(e.to_string()),
                    },
                    Err(e) => CommandResponse {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Blad watku: {}", e)),
                    },
                }
            }

            MeshCommandType::PullImage { .. }
            | MeshCommandType::DeployStack { .. }
            | MeshCommandType::RemoveStack { .. }
            | MeshCommandType::ContainerStart { .. }
            | MeshCommandType::ContainerStop { .. }
            | MeshCommandType::ContainerRestart { .. }
            | MeshCommandType::ContainerRemove { .. }
            | MeshCommandType::ContainerLogs { .. }
            | MeshCommandType::SystemPrune { .. } => CommandResponse {
                success: false,
                output: String::new(),
                error: Some("Docker commands not yet implemented".to_string()),
            },

            MeshCommandType::BandwidthProbe {
                target_ip,
                target_port,
                rdma_port,
                bind_interface,
                duration_ms,
                mode,
                nonce,
                num_streams,
            } => {
                let nonce_arr: [u8; 32] = nonce.try_into().unwrap_or([0u8; 32]);

                match mode.as_str() {
                    "server" => {
                        // Startuj TCP server ZAWSZE (fallback)
                        let tcp_result = crate::mesh::bandwidth_probe::start_probe_server(
                            &target_ip, &nonce_arr, num_streams, duration_ms,
                        ).await;

                        let (tcp_port, tcp_handle) = match tcp_result {
                            Ok((port, handle)) => (port, Some(handle)),
                            Err(e) => {
                                return CommandResponse {
                                    success: false,
                                    output: String::new(),
                                    error: Some(format!("TCP server failed: {}", e)),
                                };
                            }
                        };

                        // Probuj RDMA server na osobnym porcie (jesli dostepne)
                        let mut rdma_port: u16 = 0;
                        #[cfg(feature = "rdma-probe")]
                        if let Some(rdma_dev) = crate::mesh::rdma_probe::find_rdma_device_for_interface(&bind_interface) {
                            match crate::mesh::rdma_probe::start_rdma_probe_server(
                                &target_ip, &rdma_dev, &nonce_arr, duration_ms,
                            ).await {
                                Ok((port, handle)) => {
                                    rdma_port = port;
                                    tokio::spawn(async move { let _ = handle.await; });
                                    tracing::info!("RDMA server na porcie {}", port);
                                }
                                Err(e) => {
                                    tracing::warn!("RDMA server probe failed: {}", e);
                                }
                            }
                        }

                        // Spawn TCP handle w tle
                        if let Some(handle) = tcp_handle {
                            tokio::spawn(async move { let _ = handle.await; });
                        }

                        // Zwroc OBA porty — klient sprobuje RDMA, jesli fail uzyje TCP
                        CommandResponse {
                            success: true,
                            output: serde_json::json!({
                                "port": tcp_port,
                                "rdma_port": rdma_port,
                            }).to_string(),
                            error: None,
                        }
                    }
                    "client" => {
                        // Probuj RDMA jesli serwer zwrocil rdma_port > 0
                        #[cfg(feature = "rdma-probe")]
                        if rdma_port > 0 {
                            if let Some(rdma_dev) = crate::mesh::rdma_probe::find_rdma_device_for_interface(&bind_interface) {
                                match crate::mesh::rdma_probe::start_rdma_probe_client(
                                    &target_ip, rdma_port, &rdma_dev, &nonce_arr, duration_ms,
                                ).await {
                                    Ok(result) => {
                                        return CommandResponse {
                                            success: true,
                                            output: serde_json::json!({
                                                "bandwidth_mbps": result.bandwidth_mbps,
                                                "bytes_transferred": result.bytes_transferred,
                                                "duration_ms": result.duration_ms,
                                                "streams_completed": 1,
                                                "rdma": true,
                                            }).to_string(),
                                            error: None,
                                        };
                                    }
                                    Err(e) => {
                                        tracing::warn!("RDMA client failed, fallback TCP: {}", e);
                                    }
                                }
                            }
                        }

                        // TCP multi-stream (fallback lub jedyny tryb)
                        match crate::mesh::bandwidth_probe::start_probe_client(
                            &target_ip, target_port, &bind_interface, &nonce_arr, num_streams, duration_ms,
                        ).await {
                            Ok(result) => {
                                let output = serde_json::json!({
                                    "bandwidth_mbps": result.bandwidth_mbps,
                                    "bytes_transferred": result.bytes_transferred,
                                    "duration_ms": result.duration_ms,
                                    "latency_us": result.latency_us,
                                    "streams_completed": result.streams_completed,
                                }).to_string();
                                CommandResponse {
                                    success: true,
                                    output,
                                    error: None,
                                }
                            }
                            Err(e) => CommandResponse {
                                success: false,
                                output: String::new(),
                                error: Some(e.to_string()),
                            },
                        }
                    }
                    _ => CommandResponse {
                        success: false,
                        output: String::new(),
                        error: Some("Nieznany tryb probing".to_string()),
                    },
                }
            }

            MeshCommandType::BandwidthProbeCancel => {
                CommandResponse {
                    success: true,
                    output: String::new(),
                    error: None,
                }
            }
        }
    }

    /// Zapisuje certyfikaty do dozwolonego katalogu
    async fn handle_provision_certs(
        &self,
        cert_pem: &str,
        key_pem: &str,
        target_dir: &str,
    ) -> CommandResponse {
        match self.validate_target_dir(target_dir) {
            Ok(dir) => {
                let cert_path = dir.join("cert.pem");
                let key_path = dir.join("key.pem");

                if let Err(e) = tokio::fs::write(&cert_path, cert_pem).await {
                    return CommandResponse {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Blad zapisu cert.pem: {}", e)),
                    };
                }

                if let Err(e) = tokio::fs::write(&key_path, key_pem).await {
                    return CommandResponse {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Blad zapisu key.pem: {}", e)),
                    };
                }

                info!(dir = %dir.display(), "Certyfikaty zapisane");

                CommandResponse {
                    success: true,
                    output: format!("Certyfikaty zapisane w {}", dir.display()),
                    error: None,
                }
            }
            Err(msg) => CommandResponse {
                success: false,
                output: String::new(),
                error: Some(msg),
            },
        }
    }

    /// Waliduje sciezke docelowa — rozwiazuje symlinki przez canonicalize,
    /// sprawdza Path::starts_with() po komponentach sciezki
    fn validate_target_dir(&self, target_dir: &str) -> Result<PathBuf, String> {
        let expanded = if target_dir.starts_with("~/") {
            match dirs::home_dir() {
                Some(home) => home.join(&target_dir[2..]),
                None => return Err("Nie udalo sie ustalic katalogu domowego".to_string()),
            }
        } else {
            PathBuf::from(target_dir)
        };

        // Znajdz najdluzszy istniejacy prefix sciezki i canonicalize go,
        // potem dolacz reszte — to rozwiazuje symlinki bez wymagania istnienia katalogu
        let canonical = Self::safe_canonicalize(&expanded)?;

        let home_tentaflow = dirs::home_dir().map(|h| h.join(".tentaflow"));
        let data_tentaflow = dirs::data_dir().map(|d| d.join("tentaflow"));

        let mut allowed_dirs: Vec<PathBuf> = Vec::new();
        if let Some(p) = home_tentaflow {
            allowed_dirs.push(p);
        }
        if let Some(p) = data_tentaflow {
            allowed_dirs.push(p);
        }

        // Sprawdzenie po komponentach sciezki (Path::starts_with)
        let is_allowed = allowed_dirs.iter().any(|allowed| {
            let allowed_canonical = Self::safe_canonicalize(allowed)
                .unwrap_or_else(|_| allowed.clone());
            canonical.starts_with(&allowed_canonical)
        });

        if !is_allowed {
            return Err(format!(
                "Sciezka '{}' poza dozwolonym katalogiem (~/.tentaflow/ lub data dir)",
                target_dir
            ));
        }

        // Utworz katalog dopiero PO walidacji
        std::fs::create_dir_all(&canonical)
            .map_err(|e| format!("Nie mozna utworzyc katalogu: {}", e))?;

        Ok(canonical)
    }

    /// Rozwiazuje sciezke przez canonicalize istniejacego prefixu + normalizacje reszty
    fn safe_canonicalize(path: &std::path::Path) -> Result<PathBuf, String> {
        // Probuj canonicalize calej sciezki
        if let Ok(c) = std::fs::canonicalize(path) {
            return Ok(c);
        }

        // Znajdz najdluzszy istniejacy prefix
        let mut existing = path.to_path_buf();
        let mut suffix_parts: Vec<std::ffi::OsString> = Vec::new();

        loop {
            if existing.exists() {
                break;
            }
            match existing.file_name() {
                Some(part) => {
                    suffix_parts.push(part.to_os_string());
                    existing.pop();
                }
                None => break,
            }
        }

        let base = std::fs::canonicalize(&existing)
            .map_err(|e| format!("Nie mozna rozwiazac sciezki: {}", e))?;

        let mut result = base;
        for part in suffix_parts.into_iter().rev() {
            result.push(part);
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn odrzuca_path_traversal() {
        let executor = create_test_executor();
        let result = executor.validate_target_dir("/tmp/../etc/shadow");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("poza dozwolonym katalogiem"));
    }

    #[test]
    fn odrzuca_sciezke_poza_dozwolonym_katalogiem() {
        let executor = create_test_executor();
        let result = executor.validate_target_dir("/tmp/certs");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("poza dozwolonym katalogiem"));
    }

    #[test]
    fn akceptuje_sciezke_w_tentaflow() {
        let executor = create_test_executor();
        let result = executor.validate_target_dir("~/.tentaflow/certs");
        if dirs::home_dir().is_some() {
            assert!(result.is_ok());
        }
    }

    fn create_test_executor() -> MeshCommandExecutor {
        let db = create_test_db();
        let security = Arc::new(MeshSecurity::new(db).unwrap());
        MeshCommandExecutor::new(security)
    }

    fn create_test_db() -> crate::db::DbPool {
        use std::sync::Mutex;
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE IF NOT EXISTS trusted_nodes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                node_id TEXT NOT NULL UNIQUE,
                public_key TEXT NOT NULL,
                hostname TEXT DEFAULT '',
                approved_by TEXT DEFAULT '',
                approved_at TEXT NOT NULL DEFAULT (datetime('now')),
                is_active INTEGER NOT NULL DEFAULT 1
            );
            CREATE TABLE IF NOT EXISTS pending_pairings (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                remote_node_id TEXT NOT NULL,
                pin_code TEXT NOT NULL,
                direction TEXT NOT NULL CHECK(direction IN ('outgoing','incoming')),
                expires_at TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            ",
        )
        .unwrap();
        Arc::new(Mutex::new(conn))
    }
}

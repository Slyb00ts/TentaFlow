// =============================================================================
// Plik: audit/mod.rs
// Opis: Centralny system logowania audytowego — kazda operacja addonu jest
//       logowana. Buforowanie wpisow z batch INSERT i cyklicznym flush.
// Przyklad: audit_logger.log(AuditEntry { action: "addon.install".into(), .. });
// =============================================================================

use crate::db::DbPool;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

// =============================================================================
// RiskClass — klasyfikacja RODO wpisu audytowego (F1a §6.2.Y)
// =============================================================================

/// Klasa ryzyka wpisu audit log. Wartosc zapisywana do kolumny `risk_class`.
/// `Unclassified` — domyslna gdy wywolanie nie deklaruje klasy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RiskClass {
    /// Klasa A — operacje administracyjne i operacyjne bez danych osobowych
    /// wysokiej kategorii.
    A,
    /// Klasa B — operacje na danych osobowych zwyklych (RODO art. 6).
    B,
    /// Klasa C — operacje na danych wrazliwych / biometrycznych / decyzje
    /// automatyczne (RODO art. 9, art. 22).
    C,
    /// Nieklasyfikowane — backward compat dla wpisow sprzed F1a.
    Unclassified,
}

impl RiskClass {
    /// Reprezentacja DB (kolumna TEXT).
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::B => "B",
            Self::C => "C",
            Self::Unclassified => "unclassified",
        }
    }
}

impl std::fmt::Display for RiskClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_db_str())
    }
}

impl FromStr for RiskClass {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "A" => Ok(Self::A),
            "B" => Ok(Self::B),
            "C" => Ok(Self::C),
            "unclassified" => Ok(Self::Unclassified),
            _ => Err(()),
        }
    }
}

#[cfg(test)]
mod risk_class_tests {
    use super::*;

    #[test]
    fn risk_class_roundtrip() {
        for rc in [RiskClass::A, RiskClass::B, RiskClass::C, RiskClass::Unclassified] {
            assert_eq!(RiskClass::from_str(rc.as_db_str()).unwrap(), rc);
        }
    }

    #[test]
    fn risk_class_invalid() {
        assert!(RiskClass::from_str("D").is_err());
        assert!(RiskClass::from_str("").is_err());
    }
}

// =============================================================================
// Stale konfiguracyjne
// =============================================================================

/// Domyslna pojemnosc bufora przed wymuszeniem flush
const DEFAULT_BUFFER_CAPACITY: usize = 100;

/// Domyslny interwal flush w milisekundach (5 sekund)
const DEFAULT_FLUSH_INTERVAL_MS: u64 = 5_000;

// =============================================================================
// AuditEntry — pojedynczy wpis audytowy
// =============================================================================

/// Pojedynczy wpis w logu audytowym
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    /// Czas zdarzenia
    pub timestamp: DateTime<Utc>,
    /// ID uzytkownika (opcjonalne — operacje systemowe nie maja user_id)
    pub user_id: Option<i64>,
    /// ID addonu (opcjonalne — operacje na uzytkow. nie dotycza addonu)
    pub addon_id: Option<String>,
    /// Akcja — np. "addon.install", "user.create", "tool.call"
    pub action: String,
    /// Zasob — np. nazwa narzedzia, klucz storage
    pub resource: Option<String>,
    /// Szczegoly dodatkowe (format dowolny)
    pub details: Option<String>,
    /// Adres IP klienta
    pub ip_address: Option<String>,
    /// ID wezla w mesh
    pub node_id: Option<String>,
}

impl AuditEntry {
    /// Tworzy nowy wpis audytowy z biezacym czasem
    pub fn new(action: impl Into<String>) -> Self {
        Self {
            timestamp: Utc::now(),
            user_id: None,
            addon_id: None,
            action: action.into(),
            resource: None,
            details: None,
            ip_address: None,
            node_id: None,
        }
    }

    /// Ustawia user_id
    pub fn with_user(mut self, user_id: i64) -> Self {
        self.user_id = Some(user_id);
        self
    }

    /// Ustawia addon_id
    pub fn with_addon(mut self, addon_id: impl Into<String>) -> Self {
        self.addon_id = Some(addon_id.into());
        self
    }

    /// Ustawia zasob
    pub fn with_resource(mut self, resource: impl Into<String>) -> Self {
        self.resource = Some(resource.into());
        self
    }

    /// Ustawia szczegoly
    pub fn with_details(mut self, details: impl Into<String>) -> Self {
        self.details = Some(details.into());
        self
    }

    /// Ustawia adres IP
    pub fn with_ip(mut self, ip: impl Into<String>) -> Self {
        self.ip_address = Some(ip.into());
        self
    }

    /// Ustawia node_id
    pub fn with_node(mut self, node_id: impl Into<String>) -> Self {
        self.node_id = Some(node_id.into());
        self
    }
}

// =============================================================================
// AuditFilters — filtry do zapytan
// =============================================================================

/// Filtry do przeszukiwania logu audytowego
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuditFilters {
    /// Filtruj po ID uzytkownika
    pub user_id: Option<i64>,
    /// Filtruj po ID addonu
    pub addon_id: Option<String>,
    /// Filtruj po akcji (dokladne dopasowanie)
    pub action: Option<String>,
    /// Data poczatkowa (ISO 8601)
    pub date_from: Option<String>,
    /// Data koncowa (ISO 8601)
    pub date_to: Option<String>,
}

// =============================================================================
// AuditLogger — centralny logger z buforowaniem
// =============================================================================

/// Centralny system logowania audytowego z buforowaniem batch INSERT
pub struct AuditLogger {
    /// Pula polaczen do bazy danych
    db: DbPool,
    /// Identyfikator wezla w mesh
    node_id: String,
    /// Bufor wpisow oczekujacych na flush
    buffer: Arc<Mutex<Vec<AuditEntry>>>,
    /// Interwal flush w milisekundach
    flush_interval_ms: u64,
}

impl AuditLogger {
    /// Tworzy nowy AuditLogger z podana baza danych i identyfikatorem wezla
    pub fn new(db: DbPool, node_id: impl Into<String>) -> Self {
        info!("AuditLogger zainicjalizowany");
        Self {
            db,
            node_id: node_id.into(),
            buffer: Arc::new(Mutex::new(Vec::with_capacity(DEFAULT_BUFFER_CAPACITY))),
            flush_interval_ms: DEFAULT_FLUSH_INTERVAL_MS,
        }
    }

    /// Ustawia niestandardowy interwal flush (w milisekundach)
    pub fn with_flush_interval(mut self, interval_ms: u64) -> Self {
        self.flush_interval_ms = interval_ms;
        self
    }

    /// Dodaje wpis audytowy do bufora.
    /// Jesli bufor osiagnie DEFAULT_BUFFER_CAPACITY, automatycznie wykonuje flush.
    pub fn log(&self, mut entry: AuditEntry) {
        // Ustaw node_id jesli nie podano
        if entry.node_id.is_none() {
            entry.node_id = Some(self.node_id.clone());
        }

        let should_flush = {
            let mut buffer = self.buffer.lock();
            buffer.push(entry);
            buffer.len() >= DEFAULT_BUFFER_CAPACITY
        };

        if should_flush {
            self.flush();
        }
    }

    /// Wykonuje batch INSERT wszystkich buforowanych wpisow do SQLite
    pub fn flush(&self) {
        let entries: Vec<AuditEntry> = {
            let mut buffer = self.buffer.lock();
            if buffer.is_empty() {
                return;
            }
            std::mem::take(&mut *buffer)
        };

        let count = entries.len();
        debug!("Flush {} wpisow audytowych do bazy", count);

        let conn = match self.db.lock() {
            Ok(c) => c,
            Err(e) => {
                error!(
                    "Nie udalo sie uzyskac polaczenia DB dla flush audytu: {}",
                    e
                );
                // Proba zwrocenia wpisow do bufora
                let mut buffer = self.buffer.lock();
                for entry in entries {
                    buffer.push(entry);
                }
                return;
            }
        };

        // W9: Batch INSERT w jednej transakcji z prepared statement
        let tx_result = conn.execute_batch("BEGIN TRANSACTION");
        if let Err(e) = tx_result {
            error!("Blad rozpoczecia transakcji audit flush: {}", e);
            return;
        }

        // W9: Przygotuj statement raz przed petla — unika parsowania SQL przy kazdym wpisie
        let mut stmt = match conn.prepare(
            "INSERT INTO audit_log (timestamp, user_id, addon_id, action, resource, details, ip_address, node_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
        ) {
            Ok(s) => s,
            Err(e) => {
                error!("Blad przygotowania statement audit flush: {}", e);
                let _ = conn.execute_batch("ROLLBACK");
                return;
            }
        };

        let mut inserted = 0u64;
        for entry in &entries {
            let result = stmt.execute(rusqlite::params![
                entry.timestamp.to_rfc3339(),
                entry.user_id,
                entry.addon_id,
                entry.action,
                entry.resource,
                entry.details,
                entry.ip_address,
                entry.node_id,
            ]);

            match result {
                Ok(_) => inserted += 1,
                Err(e) => {
                    warn!("Blad zapisu wpisu audytowego: {}", e);
                }
            }
        }

        // Zwolnij statement przed commit
        drop(stmt);

        if let Err(e) = conn.execute_batch("COMMIT") {
            error!("Blad commitu transakcji audit flush: {}", e);
            let _ = conn.execute_batch("ROLLBACK");
            return;
        }

        debug!(
            "Flush audytu zakonczony: {}/{} wpisow zapisanych",
            inserted, count
        );
    }

    /// Uruchamia tokio task do cyklicznego flush bufora co flush_interval_ms
    pub fn start_flush_task(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let logger = Arc::clone(self);
        let interval = std::time::Duration::from_millis(logger.flush_interval_ms);

        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            loop {
                ticker.tick().await;
                logger.flush();
            }
        })
    }

    /// Usuwa wpisy audytowe starsze niz podana liczba dni
    pub fn cleanup(&self, days_to_keep: u32) -> anyhow::Result<u64> {
        info!(
            "Czyszczenie wpisow audytowych starszych niz {} dni",
            days_to_keep
        );

        let conn = self
            .db
            .lock()
            .map_err(|e| anyhow::anyhow!("Blad blokady DB: {}", e))?;

        let deleted = conn.execute(
            "DELETE FROM audit_log WHERE timestamp < datetime('now', ?1)",
            rusqlite::params![format!("-{} days", days_to_keep)],
        )?;

        info!("Usunieto {} starych wpisow audytowych", deleted);
        Ok(deleted as u64)
    }

    /// Wyszukuje wpisy audytowe z filtrami i paginacja
    pub fn query(
        &self,
        filters: &AuditFilters,
        offset: u32,
        limit: u32,
    ) -> anyhow::Result<Vec<AuditEntry>> {
        let conn = self
            .db
            .lock()
            .map_err(|e| anyhow::anyhow!("Blad blokady DB: {}", e))?;

        // Buduj zapytanie dynamicznie z filtrami
        let mut sql = String::from(
            "SELECT timestamp, user_id, addon_id, action, resource, details, ip_address, node_id \
             FROM audit_log WHERE 1=1",
        );
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let mut idx = 1;

        if let Some(uid) = filters.user_id {
            sql.push_str(&format!(" AND user_id = ?{}", idx));
            params.push(Box::new(uid));
            idx += 1;
        }
        if let Some(ref aid) = filters.addon_id {
            sql.push_str(&format!(" AND addon_id = ?{}", idx));
            params.push(Box::new(aid.clone()));
            idx += 1;
        }
        if let Some(ref act) = filters.action {
            sql.push_str(&format!(" AND action = ?{}", idx));
            params.push(Box::new(act.clone()));
            idx += 1;
        }
        if let Some(ref from) = filters.date_from {
            sql.push_str(&format!(" AND timestamp >= ?{}", idx));
            params.push(Box::new(from.clone()));
            idx += 1;
        }
        if let Some(ref to) = filters.date_to {
            sql.push_str(&format!(" AND timestamp <= ?{}", idx));
            params.push(Box::new(to.clone()));
            idx += 1;
        }

        sql.push_str(&format!(
            " ORDER BY timestamp DESC LIMIT ?{} OFFSET ?{}",
            idx,
            idx + 1
        ));
        params.push(Box::new(limit as i64));
        params.push(Box::new(offset as i64));

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            let ts_str: String = row.get(0)?;
            let timestamp = DateTime::parse_from_rfc3339(&ts_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());

            Ok(AuditEntry {
                timestamp,
                user_id: row.get(1)?,
                addon_id: row.get(2)?,
                action: row.get(3)?,
                resource: row.get(4)?,
                details: row.get(5)?,
                ip_address: row.get(6)?,
                node_id: row.get(7)?,
            })
        })?;

        let entries: Vec<AuditEntry> = rows.filter_map(|r| r.ok()).collect();
        Ok(entries)
    }

    /// Zwraca calkowita liczbe wpisow audytowych spelniajacych filtry
    pub fn count(&self, filters: &AuditFilters) -> anyhow::Result<u64> {
        let conn = self
            .db
            .lock()
            .map_err(|e| anyhow::anyhow!("Blad blokady DB: {}", e))?;

        let mut sql = String::from("SELECT COUNT(*) FROM audit_log WHERE 1=1");
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let mut idx = 1;

        if let Some(uid) = filters.user_id {
            sql.push_str(&format!(" AND user_id = ?{}", idx));
            params.push(Box::new(uid));
            idx += 1;
        }
        if let Some(ref aid) = filters.addon_id {
            sql.push_str(&format!(" AND addon_id = ?{}", idx));
            params.push(Box::new(aid.clone()));
            idx += 1;
        }
        if let Some(ref act) = filters.action {
            sql.push_str(&format!(" AND action = ?{}", idx));
            params.push(Box::new(act.clone()));
            idx += 1;
        }
        if let Some(ref from) = filters.date_from {
            sql.push_str(&format!(" AND timestamp >= ?{}", idx));
            params.push(Box::new(from.clone()));
            idx += 1;
        }
        if let Some(ref to) = filters.date_to {
            sql.push_str(&format!(" AND timestamp <= ?{}", idx));
            params.push(Box::new(to.clone()));
            let _ = idx;
        }

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let count: i64 = conn.query_row(&sql, param_refs.as_slice(), |row| row.get(0))?;
        Ok(count as u64)
    }

    /// Eksportuje wpisy audytowe jako CSV.
    /// W10: Przetwarzanie w batches po 1000 — zapobiega OOM przy duzych zbiorach danych.
    pub fn export_csv(&self, filters: &AuditFilters) -> anyhow::Result<String> {
        const BATCH_SIZE: u32 = 1_000;

        let mut csv =
            String::from("timestamp,user_id,addon_id,action,resource,details,ip_address,node_id\n");
        let mut offset: u32 = 0;

        loop {
            let entries = self.query(filters, offset, BATCH_SIZE)?;
            let batch_len = entries.len() as u32;

            for entry in &entries {
                csv.push_str(&format!(
                    "{},{},{},{},{},{},{},{}\n",
                    entry.timestamp.to_rfc3339(),
                    entry.user_id.map(|id| id.to_string()).unwrap_or_default(),
                    entry.addon_id.as_deref().unwrap_or(""),
                    escape_csv_field(&entry.action),
                    entry
                        .resource
                        .as_deref()
                        .map(escape_csv_field)
                        .unwrap_or_default(),
                    entry
                        .details
                        .as_deref()
                        .map(escape_csv_field)
                        .unwrap_or_default(),
                    entry.ip_address.as_deref().unwrap_or(""),
                    entry.node_id.as_deref().unwrap_or(""),
                ));
            }

            // Jesli batch jest mniejszy niz BATCH_SIZE — to ostatni batch
            if batch_len < BATCH_SIZE {
                break;
            }

            offset += batch_len;
        }

        Ok(csv)
    }
}

/// Escapuje pole CSV — owija w cudzyslowy jesli zawiera przecinek, cudzyslowy lub nowa linie
fn escape_csv_field(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

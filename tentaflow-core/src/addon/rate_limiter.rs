// =============================================================================
// Plik: addon/rate_limiter.rs
// Opis: Rate limiter per addon — kontroluje zuzycie zasobow (CPU, pamiec,
//       storage, HTTP, LLM tokeny) z oknem minutowym i automatycznym resetem.
// Przyklad: limiter.check("my-addon", ResourceType::HttpRequests)?;
// =============================================================================

use std::collections::HashMap;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{debug, info, warn};

// =============================================================================
// Typy bledow
// =============================================================================

/// Blad przekroczenia limitu zasobow
#[derive(Error, Debug)]
pub enum RateLimitError {
    #[error(
        "Addon '{addon_id}' przekroczyl limit {resource_type}: {current}/{max} w oknie minutowym"
    )]
    Exceeded {
        addon_id: String,
        resource_type: String,
        current: u64,
        max: u64,
    },

    #[error("Addon '{addon_id}' nie ma skonfigurowanych limitow")]
    NoLimits { addon_id: String },
}

// =============================================================================
// ResourceType — typ zasobu do limitowania
// =============================================================================

/// Typ zasobu podlegajacy limitowaniu
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ResourceType {
    /// Czas CPU w milisekundach
    CpuMs,
    /// Zuzycie pamieci w megabajtach
    MemoryMb,
    /// Zuzycie storage w megabajtach
    StorageMb,
    /// Liczba requestow HTTP
    HttpRequests,
    /// Liczba tokenow LLM
    LlmTokens,
}

impl std::fmt::Display for ResourceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResourceType::CpuMs => write!(f, "cpu_ms"),
            ResourceType::MemoryMb => write!(f, "memory_mb"),
            ResourceType::StorageMb => write!(f, "storage_mb"),
            ResourceType::HttpRequests => write!(f, "http_requests"),
            ResourceType::LlmTokens => write!(f, "llm_tokens"),
        }
    }
}

// =============================================================================
// ResourceLimits — limity zasobow per addon
// =============================================================================

/// Limity zasobow dla pojedynczego addonu
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceLimits {
    /// Maksymalne zuzycie CPU w ms na minute (0 = bez limitu)
    pub max_cpu_ms_per_minute: u64,
    /// Maksymalne zuzycie pamieci w MB
    pub max_memory_mb: u64,
    /// Maksymalne zuzycie storage w MB
    pub max_storage_mb: u64,
    /// Maksymalna liczba requestow HTTP na minute
    pub max_http_requests_per_minute: u32,
    /// Maksymalna liczba tokenow LLM na minute
    pub max_llm_tokens_per_minute: u32,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_cpu_ms_per_minute: 30_000, // 30s CPU na minute
            max_memory_mb: 256,
            max_storage_mb: 100,
            max_http_requests_per_minute: 600,
            max_llm_tokens_per_minute: 50_000,
        }
    }
}

// =============================================================================
// ResourceUsage — biezace zuzycie zasobow per addon
// =============================================================================

/// Biezace zuzycie zasobow dla pojedynczego addonu w oknie minutowym
#[derive(Debug, Clone, Serialize)]
pub struct ResourceUsage {
    /// Zuzyty czas CPU w milisekundach
    pub cpu_ms_used: u64,
    /// Zuzycie pamieci w megabajtach
    pub memory_mb_used: u64,
    /// Zuzycie storage w megabajtach
    pub storage_mb_used: u64,
    /// Liczba requestow HTTP w biezacym oknie
    pub http_requests_count: u32,
    /// Liczba zuytych tokenow LLM w biezacym oknie
    pub llm_tokens_used: u32,
    /// Poczatek biezacego okna minutowego (nie serializowalny)
    #[serde(skip)]
    pub window_start: Instant,
}

impl Default for ResourceUsage {
    fn default() -> Self {
        Self {
            cpu_ms_used: 0,
            memory_mb_used: 0,
            storage_mb_used: 0,
            http_requests_count: 0,
            llm_tokens_used: 0,
            window_start: Instant::now(),
        }
    }
}

// =============================================================================
// AddonRateLimiter — centralny rate limiter
// =============================================================================

/// Centralny rate limiter dla addonow — sprawdza i rejestruje zuzycie zasobow
pub struct AddonRateLimiter {
    /// Skonfigurowane limity per addon_id
    limits: Mutex<HashMap<String, ResourceLimits>>,
    /// Biezace zuzycie per addon_id
    usage: Mutex<HashMap<String, ResourceUsage>>,
    /// Czas trwania okna minutowego
    window_duration: Duration,
}

impl AddonRateLimiter {
    /// Tworzy nowy AddonRateLimiter z domyslnym oknem minutowym
    pub fn new() -> Self {
        info!("AddonRateLimiter zainicjalizowany");
        Self {
            limits: Mutex::new(HashMap::new()),
            usage: Mutex::new(HashMap::new()),
            window_duration: Duration::from_secs(60),
        }
    }

    /// Sprawdza czy addon moze zuzyc dany zasob.
    /// Zwraca Ok(()) jesli limit nie jest przekroczony, Err jesli tak.
    pub fn check(&self, addon_id: &str, resource_type: ResourceType) -> Result<(), RateLimitError> {
        let limits = self.limits.lock();
        let limits_entry = limits
            .get(addon_id)
            .ok_or_else(|| RateLimitError::NoLimits {
                addon_id: addon_id.to_string(),
            })?;

        let mut usage = self.usage.lock();
        let usage_entry = usage.entry(addon_id.to_string()).or_default();

        // Sprawdz czy okno wygaslo — jesli tak, resetuj
        if usage_entry.window_start.elapsed() >= self.window_duration {
            self.reset_usage_entry(usage_entry);
        }

        // Sprawdz limit dla danego typu zasobu
        let (current, max) = match resource_type {
            ResourceType::CpuMs => (usage_entry.cpu_ms_used, limits_entry.max_cpu_ms_per_minute),
            ResourceType::MemoryMb => (usage_entry.memory_mb_used, limits_entry.max_memory_mb),
            ResourceType::StorageMb => (usage_entry.storage_mb_used, limits_entry.max_storage_mb),
            ResourceType::HttpRequests => (
                usage_entry.http_requests_count as u64,
                limits_entry.max_http_requests_per_minute as u64,
            ),
            ResourceType::LlmTokens => (
                usage_entry.llm_tokens_used as u64,
                limits_entry.max_llm_tokens_per_minute as u64,
            ),
        };

        // 0 = bez limitu
        if max == 0 {
            return Ok(());
        }

        if current >= max {
            warn!(
                "Addon '{}' przekroczyl limit {}: {}/{}",
                addon_id, resource_type, current, max
            );
            return Err(RateLimitError::Exceeded {
                addon_id: addon_id.to_string(),
                resource_type: resource_type.to_string(),
                current,
                max,
            });
        }

        Ok(())
    }

    /// Rejestruje zuzycie zasobu przez addon
    pub fn record_usage(&self, addon_id: &str, resource_type: ResourceType, amount: u64) {
        let mut usage = self.usage.lock();
        let entry = usage.entry(addon_id.to_string()).or_default();

        // Sprawdz czy okno wygaslo
        if entry.window_start.elapsed() >= self.window_duration {
            self.reset_usage_entry(entry);
        }

        match resource_type {
            ResourceType::CpuMs => entry.cpu_ms_used += amount,
            ResourceType::MemoryMb => entry.memory_mb_used = amount, // Pamiec to aktualne zuzycie
            ResourceType::StorageMb => entry.storage_mb_used = amount, // Storage to aktualne zuzycie
            ResourceType::HttpRequests => entry.http_requests_count += amount as u32,
            ResourceType::LlmTokens => entry.llm_tokens_used += amount as u32,
        }

        debug!(
            "Addon '{}' — zuzycie {}: +{} (okno od {:?})",
            addon_id,
            resource_type,
            amount,
            entry.window_start.elapsed()
        );
    }

    /// Zwraca biezace zuzycie zasobow przez addon
    pub fn get_usage(&self, addon_id: &str) -> ResourceUsage {
        let mut usage = self.usage.lock();
        let entry = usage.entry(addon_id.to_string()).or_default();

        // Sprawdz czy okno wygaslo
        if entry.window_start.elapsed() >= self.window_duration {
            self.reset_usage_entry(entry);
        }

        entry.clone()
    }

    /// Ustawia limity zasobow dla addonu
    pub fn set_limits(&self, addon_id: &str, limits: ResourceLimits) {
        info!(
            "Ustawiono limity dla addonu '{}': CPU={}ms, MEM={}MB, STOR={}MB, HTTP={}/min, LLM={}/min",
            addon_id,
            limits.max_cpu_ms_per_minute,
            limits.max_memory_mb,
            limits.max_storage_mb,
            limits.max_http_requests_per_minute,
            limits.max_llm_tokens_per_minute,
        );
        self.limits.lock().insert(addon_id.to_string(), limits);
    }

    /// Resetuje countery minutowe dla wszystkich addonow (wywolywalny recznie lub przez timer)
    pub fn reset_all_windows(&self) {
        let mut usage = self.usage.lock();
        for (addon_id, entry) in usage.iter_mut() {
            debug!("Reset okna minutowego dla addonu '{}'", addon_id);
            self.reset_usage_entry(entry);
        }
    }

    /// Uruchamia tokio task resetujacy okna minutowe
    pub fn start_reset_task(self: &std::sync::Arc<Self>) -> tokio::task::JoinHandle<()> {
        let limiter = std::sync::Arc::clone(self);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(60));
            loop {
                ticker.tick().await;
                limiter.reset_all_windows();
            }
        })
    }

    /// Usuwa limity i zuzycie dla addonu (po odinstalowaniu)
    pub fn remove_addon(&self, addon_id: &str) {
        self.limits.lock().remove(addon_id);
        self.usage.lock().remove(addon_id);
        debug!("Usunieto limity i zuzycie dla addonu '{}'", addon_id);
    }

    /// Zwraca liste addonow z ich limitami i biezacym zuzyciem
    pub fn get_all_status(&self) -> Vec<(String, ResourceLimits, ResourceUsage)> {
        let limits = self.limits.lock();
        let mut usage = self.usage.lock();

        limits
            .iter()
            .map(|(addon_id, lim)| {
                let usg = usage.entry(addon_id.clone()).or_default().clone();
                (addon_id.clone(), lim.clone(), usg)
            })
            .collect()
    }

    /// Resetuje countery minutowe w pojedynczym wpisie zuzycia
    fn reset_usage_entry(&self, entry: &mut ResourceUsage) {
        entry.cpu_ms_used = 0;
        entry.http_requests_count = 0;
        entry.llm_tokens_used = 0;
        entry.window_start = Instant::now();
        // Pamiec i storage nie sa resetowane — to aktualne zuzycie, nie per-minute
    }
}

impl Default for AddonRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

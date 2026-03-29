// =============================================================================
// Plik: flow_engine/cache.rs
// Opis: Cache resolucji flow z TTL - unika odpytywania DB przy kazdym requeście.
//       Klucz cache to "{model_name}:{service_type}".
// =============================================================================

use crate::db::models::DbFlow;
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

/// Cache resolucji flow z automatycznym TTL
pub struct FlowCache {
    entries: RwLock<HashMap<String, CacheEntry>>,
    ttl: Duration,
}

/// Pojedynczy wpis cache z timestampem wstawienia
struct CacheEntry {
    /// None = brak flow (tez cache'ujemy negatywny wynik)
    flow: Option<DbFlow>,
    inserted_at: Instant,
}

impl FlowCache {
    pub fn new(ttl_secs: u64) -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            ttl: Duration::from_secs(ttl_secs),
        }
    }

    /// Pobiera wpis z cache.
    /// Zwraca:
    /// - Some(Some(DbFlow)) - flow znaleziony w cache
    /// - Some(None) - cache mowi ze flow nie istnieje (negatywny cache)
    /// - None - nie ma w cache (trzeba odpytac DB)
    pub fn get(&self, key: &str) -> Option<Option<DbFlow>> {
        let entries = self.entries.read().ok()?;
        let entry = entries.get(key)?;

        if entry.inserted_at.elapsed() > self.ttl {
            return None;
        }

        Some(entry.flow.clone())
    }

    /// Ustawia wpis w cache
    pub fn set(&self, key: &str, value: Option<DbFlow>) {
        if let Ok(mut entries) = self.entries.write() {
            let entry = CacheEntry {
                flow: value,
                inserted_at: Instant::now(),
            };
            entries.insert(key.to_string(), entry);
        }
    }

    /// Inwaliduje pojedynczy klucz
    pub fn invalidate(&self, key: &str) {
        if let Ok(mut entries) = self.entries.write() {
            entries.remove(key);
        }
    }

    /// Inwaliduje caly cache
    pub fn invalidate_all(&self) {
        if let Ok(mut entries) = self.entries.write() {
            entries.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_flow(id: i64, json: &str) -> DbFlow {
        DbFlow {
            id,
            name: format!("test-flow-{}", id),
            description: None,
            version: 1,
            is_default: false,
            service_type: None,
            flow_json: json.to_string(),
            status: "active".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn test_cache_miss() {
        let cache = FlowCache::new(60);
        assert!(cache.get("model:chat").is_none());
    }

    #[test]
    fn test_cache_hit_positive() {
        let cache = FlowCache::new(60);
        cache.set("model:chat", Some(test_flow(42, r#"{"nodes":[]}"#)));
        let result = cache.get("model:chat");
        assert!(result.is_some());
        let inner = result.unwrap();
        assert!(inner.is_some());
        let flow = inner.unwrap();
        assert_eq!(flow.id, 42);
        assert_eq!(flow.flow_json, r#"{"nodes":[]}"#);
    }

    #[test]
    fn test_cache_hit_negative() {
        let cache = FlowCache::new(60);
        cache.set("model:chat", None);
        let result = cache.get("model:chat");
        assert!(result.is_some());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_cache_invalidate() {
        let cache = FlowCache::new(60);
        cache.set("model:chat", Some(test_flow(1, "{}")));
        cache.invalidate("model:chat");
        assert!(cache.get("model:chat").is_none());
    }

    #[test]
    fn test_cache_invalidate_all() {
        let cache = FlowCache::new(60);
        cache.set("a:chat", Some(test_flow(1, "{}")));
        cache.set("b:rag", Some(test_flow(2, "{}")));
        cache.invalidate_all();
        assert!(cache.get("a:chat").is_none());
        assert!(cache.get("b:rag").is_none());
    }

    #[test]
    fn test_cache_ttl_expired() {
        let cache = FlowCache::new(0);
        cache.set("model:chat", Some(test_flow(1, "{}")));
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert!(cache.get("model:chat").is_none());
    }

    #[test]
    fn test_cache_concurrent_access() {
        let cache = std::sync::Arc::new(FlowCache::new(60));
        let mut handles = vec![];

        for i in 0..10 {
            let cache_clone = cache.clone();
            let handle = std::thread::spawn(move || {
                let key = format!("model-{}:chat", i);
                cache_clone.set(&key, Some(test_flow(i as i64, "{}")));
            });
            handles.push(handle);
        }

        for h in handles {
            h.join().unwrap();
        }

        let mut read_handles = vec![];
        for i in 0..10 {
            let cache_clone = cache.clone();
            let handle = std::thread::spawn(move || {
                let key = format!("model-{}:chat", i);
                let result = cache_clone.get(&key);
                assert!(result.is_some(), "Klucz {} powinien byc w cache", key);
                let flow = result.unwrap().unwrap();
                assert_eq!(flow.id, i as i64);
            });
            read_handles.push(handle);
        }

        for h in read_handles {
            h.join().unwrap();
        }
    }

    #[test]
    fn test_cache_concurrent_read_write() {
        let cache = std::sync::Arc::new(FlowCache::new(60));
        cache.set("shared:key", Some(test_flow(99, "{}")));

        let mut handles = vec![];

        let cache_w = cache.clone();
        handles.push(std::thread::spawn(move || {
            for i in 0..100 {
                cache_w.set("shared:key", Some(test_flow(i, "{}")));
            }
        }));

        for _ in 0..5 {
            let cache_r = cache.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..100 {
                    let _ = cache_r.get("shared:key");
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        let result = cache.get("shared:key");
        assert!(result.is_some());
    }
}

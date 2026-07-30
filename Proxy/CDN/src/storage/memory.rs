use crate::{
    config::Config,
    storage::{CacheKey, CacheKind},
};
use axum::http::StatusCode;
use bytes::Bytes;
use moka::future::Cache;
use std::sync::Arc;

/// Cached response stored in memory.
#[derive(Clone)]
pub struct MemValue {
    pub status: StatusCode,
    pub headers: Vec<(String, String)>,
    pub body: Bytes,
}

/// Memory cache wrapper.
///
/// The memory cache is partitioned into two classes:
/// - MPDs: small, very short TTL
/// - Everything else: segments, init segments, chunk files, etc.
///
/// This avoids a high request-rate segment workload from evicting MPDs.
pub struct MemoryCache {
    pub cfg: Arc<Config>,
    mpd: Cache<CacheKey, Arc<MemValue>>,
    rest: Cache<CacheKey, Arc<MemValue>>,
}

impl MemoryCache {
    /// Creates a new memory cache according to the given config.
    pub fn new(cfg: &Config) -> Self {
        let cfg = Arc::new(cfg.clone());

        let total_cap = cfg.memory_max_bytes;
        let mpd_cap = cfg.mpd_memory_max_bytes.min(total_cap);
        let rest_cap = total_cap.saturating_sub(mpd_cap);

        let mpd = Cache::builder()
            .time_to_live(cfg.mpd_memory_ttl())
            .max_capacity(mpd_cap)
            .weigher(|_k, v: &Arc<MemValue>| (v.body.len().max(1)) as u32)
            .build();

        let rest = Cache::builder()
            .time_to_live(cfg.memory_ttl())
            .max_capacity(rest_cap)
            .weigher(|_k, v: &Arc<MemValue>| (v.body.len().max(1)) as u32)
            .build();

        Self { cfg, mpd, rest }
    }

    /// Returns a value from memory, if present.
    pub async fn get(&self, kind: CacheKind, key: CacheKey) -> Option<MemHit> {
        let v = match kind {
            CacheKind::Mpd => self.mpd.get(&key).await,
            CacheKind::Segment | CacheKind::Other => self.rest.get(&key).await,
        };
        v.map(|value| MemHit { value })
    }

    /// Inserts a value into the memory cache.
    pub async fn insert(&self, kind: CacheKind, key: CacheKey, value: MemValue) {
        // Disabled if TTL is 0 for the given class.
        match kind {
            CacheKind::Mpd if self.cfg.mpd_memory_ttl_secs == 0 => return,
            CacheKind::Segment | CacheKind::Other if self.cfg.memory_ttl_secs == 0 => return,
            _ => {}
        }

        let cache = match kind {
            CacheKind::Mpd => &self.mpd,
            CacheKind::Segment | CacheKind::Other => &self.rest,
        };
        cache.insert(key, Arc::new(value)).await;
    }
}

/// Memory cache hit.
pub struct MemHit {
    pub value: Arc<MemValue>,
}

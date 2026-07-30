pub mod disk;
pub mod inflight;
pub mod memory;

use crate::error::AppError;
use dashmap::{mapref::entry::Entry, DashMap};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub use disk::{DiskCache, DiskHit};
pub use inflight::{Inflight, InflightHead, InflightProgress, InflightRead};
pub use memory::MemoryCache;

/// Logical cache class used for TTL + budgeting decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheKind {
    /// MPD manifests.
    Mpd,
    /// Media segments, init segments, audio chunks, etc.
    Segment,
    /// Other objects (e.g. thumbnails, subtitles) that don't fit the above.
    Other,
}

/// Fixed-size cache key (hash bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CacheKey(pub [u8; 32]);

/// Request method class used only for temporary in-flight coalescing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InflightMethod {
    Get,
    Head,
}

/// In-flight requests must not coalesce methods with different body semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InflightKey {
    cache_key: CacheKey,
    method: InflightMethod,
}

impl InflightKey {
    pub fn new(cache_key: CacheKey, method: InflightMethod) -> Self {
        Self { cache_key, method }
    }
}

/// Coordinates memory+disk caches and prevents duplicate fills per key.
pub struct CacheManager {
    pub mem: MemoryCache,
    pub disk: DiskCache,
    inflight: DashMap<InflightKey, Arc<Inflight>>,
}

impl CacheManager {
    pub fn new(mem: MemoryCache, disk: DiskCache) -> Self {
        Self {
            mem,
            disk,
            inflight: DashMap::new(),
        }
    }

    /// Returns an existing inflight entry (followers use this).
    pub fn get_inflight(&self, key: InflightKey) -> Option<Arc<Inflight>> {
        self.inflight.get(&key).map(|v| v.clone())
    }

    /// Returns an inflight entry; `is_leader=true` only for the creator.
    pub fn get_or_create_inflight(&self, key: InflightKey) -> (Arc<Inflight>, bool) {
        match self.inflight.entry(key) {
            Entry::Occupied(o) => (o.get().clone(), false),
            Entry::Vacant(v) => {
                let inf = Arc::new(Inflight::new());
                v.insert(inf.clone());
                (inf, true)
            }
        }
    }

    /// Removes the inflight entry (leader calls at end, best-effort).
    pub fn remove_inflight(&self, key: InflightKey) {
        let _ = self.inflight.remove(&key);
    }

    /// Background disk sweeper loop.
    pub async fn disk_sweeper_loop(
        self: Arc<Self>,
        cancel: CancellationToken,
        stats: Arc<crate::stats::Stats>,
    ) -> Result<(), AppError> {
        self.disk.sweeper_loop(cancel, stats).await
    }
}

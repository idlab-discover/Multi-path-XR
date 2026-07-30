use serde::Serialize;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheBytesTier {
    Mem,
    Disk,
}

/// Snapshot of current statistics (stable JSON format).
#[derive(Debug, Clone, Serialize)]
pub struct StatsSnapshot {
    pub requests_total: u64,
    pub inflight_requests: u64,
    pub coalesced_requests_total: u64,
    pub follower_waits_total: u64,
    pub cache_hit_mem_total: u64,
    pub cache_hit_disk_total: u64,
    pub cache_miss_total: u64,
    pub origin_fetch_total: u64,
    pub origin_fetch_errors_total: u64,
    pub origin_bytes_total: u64,
    pub bytes_served_total: u64,
    pub cache_bytes_served_mem_total: u64,
    pub cache_bytes_served_disk_total: u64,
    pub proxy_first_forward_latency_us: u64,
    pub disk_evictions_total: u64,
    pub last_sweep_deleted_files: i64,
}

/// Thread-safe counters/gauges for instrumentation.
#[derive(Debug)]
pub struct Stats {
    requests_total: AtomicU64,
    inflight_requests: AtomicU64,
    coalesced_requests_total: AtomicU64,
    follower_waits_total: AtomicU64,
    cache_hit_mem_total: AtomicU64,
    cache_hit_disk_total: AtomicU64,
    cache_miss_total: AtomicU64,
    origin_fetch_total: AtomicU64,
    origin_fetch_errors_total: AtomicU64,
    origin_bytes_total: AtomicU64,
    bytes_served_total: AtomicU64,
    cache_bytes_served_mem_total: AtomicU64,
    cache_bytes_served_disk_total: AtomicU64,
    proxy_first_forward_latency_us: AtomicU64,
    disk_evictions_total: AtomicU64,
    last_sweep_deleted_files: AtomicI64,
}

impl Stats {
    /// Creates a new Stats instance with all counters at 0.
    pub fn new() -> Self {
        Self {
            requests_total: AtomicU64::new(0),
            inflight_requests: AtomicU64::new(0),
            coalesced_requests_total: AtomicU64::new(0),
            follower_waits_total: AtomicU64::new(0),
            cache_hit_mem_total: AtomicU64::new(0),
            cache_hit_disk_total: AtomicU64::new(0),
            cache_miss_total: AtomicU64::new(0),
            origin_fetch_total: AtomicU64::new(0),
            origin_fetch_errors_total: AtomicU64::new(0),
            origin_bytes_total: AtomicU64::new(0),
            bytes_served_total: AtomicU64::new(0),
            cache_bytes_served_mem_total: AtomicU64::new(0),
            cache_bytes_served_disk_total: AtomicU64::new(0),
            proxy_first_forward_latency_us: AtomicU64::new(0),
            disk_evictions_total: AtomicU64::new(0),
            last_sweep_deleted_files: AtomicI64::new(0),
        }
    }

    pub fn inc_requests(&self) {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
    }
    pub fn inc_inflight(&self) {
        self.inflight_requests.fetch_add(1, Ordering::Relaxed);
    }
    pub fn dec_inflight(&self) {
        self.inflight_requests.fetch_sub(1, Ordering::Relaxed);
    }
    pub fn inc_coalesced_requests(&self) {
        self.coalesced_requests_total
            .fetch_add(1, Ordering::Relaxed);
    }
    pub fn inc_follower_waits(&self) {
        self.follower_waits_total.fetch_add(1, Ordering::Relaxed);
    }
    pub fn inc_hit_mem(&self) {
        self.cache_hit_mem_total.fetch_add(1, Ordering::Relaxed);
    }
    pub fn inc_hit_disk(&self) {
        self.cache_hit_disk_total.fetch_add(1, Ordering::Relaxed);
    }
    pub fn inc_miss(&self) {
        self.cache_miss_total.fetch_add(1, Ordering::Relaxed);
    }
    pub fn inc_origin_fetch(&self) {
        self.origin_fetch_total.fetch_add(1, Ordering::Relaxed);
    }
    pub fn inc_origin_error(&self) {
        self.origin_fetch_errors_total
            .fetch_add(1, Ordering::Relaxed);
    }
    pub fn add_origin_bytes(&self, n: u64) {
        self.origin_bytes_total.fetch_add(n, Ordering::Relaxed);
    }
    pub fn add_bytes_served(&self, n: u64) {
        self.bytes_served_total.fetch_add(n, Ordering::Relaxed);
    }
    pub fn add_cache_bytes_served(&self, tier: CacheBytesTier, n: u64) {
        match tier {
            CacheBytesTier::Mem => {
                self.cache_bytes_served_mem_total
                    .fetch_add(n, Ordering::Relaxed);
            }
            CacheBytesTier::Disk => {
                self.cache_bytes_served_disk_total
                    .fetch_add(n, Ordering::Relaxed);
            }
        }
    }
    pub fn set_proxy_first_forward_latency_us(&self, n: u64) {
        self.proxy_first_forward_latency_us
            .store(n, Ordering::Relaxed);
    }
    pub fn add_disk_evictions(&self, n: u64) {
        self.disk_evictions_total.fetch_add(n, Ordering::Relaxed);
    }
    pub fn set_last_sweep_deleted_files(&self, n: i64) {
        self.last_sweep_deleted_files.store(n, Ordering::Relaxed);
    }

    /// Returns a consistent snapshot (best-effort, relaxed atomics).
    pub fn snapshot(&self) -> StatsSnapshot {
        StatsSnapshot {
            requests_total: self.requests_total.load(Ordering::Relaxed),
            inflight_requests: self.inflight_requests.load(Ordering::Relaxed),
            coalesced_requests_total: self.coalesced_requests_total.load(Ordering::Relaxed),
            follower_waits_total: self.follower_waits_total.load(Ordering::Relaxed),
            cache_hit_mem_total: self.cache_hit_mem_total.load(Ordering::Relaxed),
            cache_hit_disk_total: self.cache_hit_disk_total.load(Ordering::Relaxed),
            cache_miss_total: self.cache_miss_total.load(Ordering::Relaxed),
            origin_fetch_total: self.origin_fetch_total.load(Ordering::Relaxed),
            origin_fetch_errors_total: self.origin_fetch_errors_total.load(Ordering::Relaxed),
            origin_bytes_total: self.origin_bytes_total.load(Ordering::Relaxed),
            bytes_served_total: self.bytes_served_total.load(Ordering::Relaxed),
            cache_bytes_served_mem_total: self.cache_bytes_served_mem_total.load(Ordering::Relaxed),
            cache_bytes_served_disk_total: self
                .cache_bytes_served_disk_total
                .load(Ordering::Relaxed),
            proxy_first_forward_latency_us: self
                .proxy_first_forward_latency_us
                .load(Ordering::Relaxed),
            disk_evictions_total: self.disk_evictions_total.load(Ordering::Relaxed),
            last_sweep_deleted_files: self.last_sweep_deleted_files.load(Ordering::Relaxed),
        }
    }

    /// Exposes a small Prometheus text payload (counters/gauges only).
    pub fn to_prometheus_text(&self) -> String {
        let s = self.snapshot();
        format!(
            "requests_total {rt}
inflight_requests {ir}
coalesced_requests_total {cr}
follower_waits_total {fw}
cache_hit_mem_total {hm}
cache_hit_disk_total {hd}
cache_miss_total {cm}
origin_fetch_total {of}
origin_fetch_errors_total {oe}
origin_bytes_total {ob}
bytes_served_total {bs}
cache_bytes_served_total{{tier=\"mem\"}} {cbm}
cache_bytes_served_total{{tier=\"disk\"}} {cbd}
proxy_first_forward_latency_us {pff}
disk_evictions_total {de}
last_sweep_deleted_files {ls}
",
            rt = s.requests_total,
            ir = s.inflight_requests,
            cr = s.coalesced_requests_total,
            fw = s.follower_waits_total,
            hm = s.cache_hit_mem_total,
            hd = s.cache_hit_disk_total,
            cm = s.cache_miss_total,
            of = s.origin_fetch_total,
            oe = s.origin_fetch_errors_total,
            ob = s.origin_bytes_total,
            bs = s.bytes_served_total,
            cbm = s.cache_bytes_served_mem_total,
            cbd = s.cache_bytes_served_disk_total,
            pff = s.proxy_first_forward_latency_us,
            de = s.disk_evictions_total,
            ls = s.last_sweep_deleted_files,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_and_prometheus_text_include_proxy_4a_totals() {
        let stats = Stats::new();
        stats.inc_requests();
        stats.inc_inflight();
        stats.inc_coalesced_requests();
        stats.inc_follower_waits();
        stats.inc_hit_mem();
        stats.inc_hit_disk();
        stats.inc_miss();
        stats.inc_origin_fetch();
        stats.inc_origin_error();
        stats.add_origin_bytes(2_048);
        stats.add_bytes_served(4_096);
        stats.add_cache_bytes_served(CacheBytesTier::Mem, 1_024);
        stats.add_cache_bytes_served(CacheBytesTier::Disk, 3_072);
        stats.set_proxy_first_forward_latency_us(777);
        stats.add_disk_evictions(9);
        stats.set_last_sweep_deleted_files(4);

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.coalesced_requests_total, 1);
        assert_eq!(snapshot.follower_waits_total, 1);
        assert_eq!(snapshot.origin_bytes_total, 2_048);
        assert_eq!(snapshot.cache_bytes_served_mem_total, 1_024);
        assert_eq!(snapshot.cache_bytes_served_disk_total, 3_072);
        assert_eq!(snapshot.proxy_first_forward_latency_us, 777);

        let prometheus = stats.to_prometheus_text();
        assert!(prometheus.contains("coalesced_requests_total 1"));
        assert!(prometheus.contains("follower_waits_total 1"));
        assert!(prometheus.contains("origin_bytes_total 2048"));
        assert!(prometheus.contains("cache_bytes_served_total{tier=\"mem\"} 1024"));
        assert!(prometheus.contains("cache_bytes_served_total{tier=\"disk\"} 3072"));
        assert!(prometheus.contains("proxy_first_forward_latency_us 777"));
    }
}

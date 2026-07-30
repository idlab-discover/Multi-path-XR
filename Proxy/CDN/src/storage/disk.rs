use crate::{config::Config, error::AppError, stats::Stats, storage::CacheKey};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::fs;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// On-disk metadata (JSON).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DiskMeta {
    status: u16,
    headers: Vec<(String, String)>,
    expires_at_ms: u64,
    created_at_ms: u64,
}

/// Disk cache hit descriptor.
pub struct DiskHit {
    pub status: StatusCode,
    pub headers: Vec<(String, String)>,
    pub body_path: PathBuf,
}

pub struct DiskCache {
    cfg: Arc<Config>,
}

impl DiskCache {
    pub async fn new(cfg: &Config) -> Result<Self, AppError> {
        let cfg = Arc::new(cfg.clone());
        fs::create_dir_all(&cfg.cache_dir).await?;
        Ok(Self { cfg })
    }

    pub async fn get(&self, key: CacheKey) -> Result<Option<DiskHit>, AppError> {
        // Disk reads are permitted if *any* class has disk caching enabled.
        if self.cfg.disk_ttl_secs == 0 && self.cfg.mpd_disk_ttl_secs == 0 {
            return Ok(None);
        }

        let (meta_path, body_path) = self.paths_for_key(key);
        let meta_bytes = match fs::read(&meta_path).await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(AppError::Io(e)),
        };

        let meta: DiskMeta = match serde_json::from_slice(&meta_bytes) {
            Ok(m) => m,
            Err(_) => {
                // Corrupt meta: delete both files best-effort.
                let _ = fs::remove_file(&meta_path).await;
                let _ = fs::remove_file(&body_path).await;
                return Ok(None);
            }
        };

        if is_expired_ms(meta.expires_at_ms) {
            let _ = fs::remove_file(&meta_path).await;
            let _ = fs::remove_file(&body_path).await;
            return Ok(None);
        }

        let status = StatusCode::from_u16(meta.status).unwrap_or(StatusCode::OK); // safe fallback

        // Ensure body exists.
        if fs::metadata(&body_path).await.is_err() {
            let _ = fs::remove_file(&meta_path).await;
            return Ok(None);
        }

        Ok(Some(DiskHit {
            status,
            headers: meta.headers,
            body_path,
        }))
    }

    pub fn paths_for_key(&self, key: CacheKey) -> (PathBuf, PathBuf) {
        let hex = hex::encode(key.0);
        let shard1 = &hex[0..2];
        let shard2 = &hex[2..4];

        let dir = self.cfg.cache_dir.join(shard1).join(shard2);
        let body = dir.join(format!("{hex}.body"));
        let meta = dir.join(format!("{hex}.json"));
        (meta, body)
    }

    /// Background disk sweeper loop.
    ///
    /// This periodically deletes expired objects and (optionally) enforces a hard size cap.
    pub async fn sweeper_loop(
        &self,
        cancel: CancellationToken,
        stats: Arc<Stats>,
    ) -> Result<(), AppError> {
        let tick = Duration::from_secs(self.cfg.sweep_interval_secs.max(1));
        let max_disk = self.cfg.max_disk_bytes;

        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(tick) => {
                    let deleted = self.sweep_expired().await.unwrap_or(0);
                    stats.set_last_sweep_deleted_files(deleted);

                    if max_disk > 0 {
                        match self.enforce_disk_cap(max_disk).await {
                            Ok(evicted) => stats.add_disk_evictions(evicted),
                            Err(e) => warn!(error = %e, "disk cap enforcement failed"),
                        }
                    }

                    debug!(deleted, "disk sweep completed");
                }
            }
        }
        Ok(())
    }

    async fn sweep_expired(&self) -> Result<i64, AppError> {
        if self.cfg.disk_ttl_secs == 0 && self.cfg.mpd_disk_ttl_secs == 0 {
            return Ok(0);
        }
        let root = self.cfg.cache_dir.clone();
        sweep_dir(&root).await
    }

    async fn enforce_disk_cap(&self, max_bytes: u64) -> Result<u64, AppError> {
        // Best-effort: gather meta files, approximate by body sizes, remove oldest until under cap.
        let mut entries: Vec<(u64, PathBuf, PathBuf)> = Vec::new(); // (created_at_ms, meta, body)
        gather_meta_files(&self.cfg.cache_dir, &mut entries).await?;

        // compute total
        let mut total: u64 = 0;
        let mut sized: Vec<(u64, u64, PathBuf, PathBuf)> = Vec::new(); // (created_at_ms, size, meta, body)

        for (created_at_ms, meta, body) in entries {
            let sz = fs::metadata(&body).await.map(|m| m.len()).unwrap_or(0);
            total = total.saturating_add(sz);
            sized.push((created_at_ms, sz, meta, body));
        }

        if total <= max_bytes {
            return Ok(0);
        }

        // oldest first
        sized.sort_by_key(|(created_at_ms, _, _, _)| *created_at_ms);

        let mut evicted: u64 = 0;
        for (_created, sz, meta, body) in sized {
            if total <= max_bytes {
                break;
            }
            let _ = fs::remove_file(&meta).await;
            let _ = fs::remove_file(&body).await;
            total = total.saturating_sub(sz);
            evicted = evicted.saturating_add(1);
        }

        Ok(evicted)
    }

    pub async fn finalize_meta_atomic_with_ttl(
        &self,
        key: crate::storage::CacheKey,
        status: axum::http::StatusCode,
        headers: Vec<(String, String)>,
        ttl_secs: u64,
    ) -> Result<(), crate::error::AppError> {
        if ttl_secs == 0 {
            return Ok(());
        }

        let (meta_path, body_path) = self.paths_for_key(key);
        let Some(parent) = meta_path.parent() else {
            return Ok(());
        };

        create_cache_dir(parent, &body_path).await?;

        if !body_exists(&body_path).await? {
            return Ok(());
        }

        let now_ms = now_unix_ms();
        let expires_at_ms = now_ms.saturating_add(ttl_secs.saturating_mul(1000));

        let meta = DiskMeta {
            status: status.as_u16(),
            headers,
            expires_at_ms,
            created_at_ms: now_ms,
        };

        let tmp_meta = tmp_path(&meta_path);
        {
            use tokio::io::AsyncWriteExt;
            let mut f = match tokio::fs::File::create(&tmp_meta).await {
                Ok(f) => f,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    cleanup_unpublished(&tmp_meta, &body_path).await;
                    return Ok(());
                }
                Err(e) => return Err(AppError::Io(e)),
            };
            let meta_json = serde_json::to_vec(&meta)
                .map_err(|e| crate::error::AppError::Internal(format!("meta serialize: {e}")))?;
            f.write_all(&meta_json).await?;
            f.flush().await?;
        }

        if !body_exists(&body_path).await? {
            let _ = tokio::fs::remove_file(&tmp_meta).await;
            return Ok(());
        }

        match tokio::fs::rename(&tmp_meta, &meta_path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                cleanup_unpublished(&tmp_meta, &body_path).await;
                Ok(())
            }
            Err(e) => {
                let _ = tokio::fs::remove_file(&tmp_meta).await;
                Err(AppError::Io(e))
            }
        }
    }
}

async fn create_cache_dir(parent: &Path, body_path: &Path) -> Result<(), AppError> {
    match tokio::fs::create_dir_all(parent).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let _ = tokio::fs::remove_file(body_path).await;
            Ok(())
        }
        Err(e) => Err(AppError::Io(e)),
    }
}

async fn body_exists(body_path: &Path) -> Result<bool, AppError> {
    match tokio::fs::metadata(body_path).await {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(AppError::Io(e)),
    }
}

async fn cleanup_unpublished(tmp_meta: &Path, body_path: &Path) {
    let _ = tokio::fs::remove_file(tmp_meta).await;
    let _ = tokio::fs::remove_file(body_path).await;
}

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_millis() as u64
}

fn is_expired_ms(expires_at_ms: u64) -> bool {
    now_unix_ms() >= expires_at_ms
}

fn tmp_path(p: &Path) -> PathBuf {
    let mut name = p
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| OsString::from("meta"));
    name.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    p.with_file_name(name)
}

async fn sweep_dir(root: &Path) -> Result<i64, AppError> {
    let mut deleted: i64 = 0;
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let mut rd = match fs::read_dir(&dir).await {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(AppError::Io(e)),
        };

        while let Some(ent) = rd.next_entry().await? {
            let path = ent.path();
            let ft = ent.file_type().await?;
            if ft.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }

            let meta_bytes = match fs::read(&path).await {
                Ok(b) => b,
                Err(_) => continue,
            };
            let meta: DiskMeta = match serde_json::from_slice(&meta_bytes) {
                Ok(m) => m,
                Err(_) => {
                    let _ = fs::remove_file(&path).await;
                    deleted += 1;
                    continue;
                }
            };

            if is_expired_ms(meta.expires_at_ms) {
                let body = path.with_extension("body");
                let _ = fs::remove_file(&path).await;
                let _ = fs::remove_file(&body).await;
                deleted += 2;
            }
        }
    }

    Ok(deleted)
}

async fn gather_meta_files(
    root: &Path,
    out: &mut Vec<(u64, PathBuf, PathBuf)>,
) -> Result<(), AppError> {
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let mut rd = match fs::read_dir(&dir).await {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(AppError::Io(e)),
        };

        while let Some(ent) = rd.next_entry().await? {
            let path = ent.path();
            let ft = ent.file_type().await?;
            if ft.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }

            let meta_bytes = match fs::read(&path).await {
                Ok(b) => b,
                Err(_) => continue,
            };
            let meta: DiskMeta = match serde_json::from_slice(&meta_bytes) {
                Ok(m) => m,
                Err(_) => continue,
            };

            let body = path.with_extension("body");
            out.push((meta.created_at_ms, path, body));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CacheMode, KeyNormalizationMode, LogLevel};

    fn test_config(name: &str) -> Config {
        let cache_dir = std::env::temp_dir().join(format!(
            "cdn_proxy_disk_cache_{name}_{}_{}_{}",
            std::process::id(),
            now_unix_ms(),
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));

        Config {
            log_level: LogLevel::Info,
            listen_addr: "127.0.0.1:0".to_string(),
            origin_base_url: "http://example.test/".to_string(),
            cache_dir,
            memory_ttl_secs: 5,
            disk_ttl_secs: 60,
            memory_max_bytes: 64 * 1024 * 1024,
            max_object_bytes: 256 * 1024 * 1024,
            memory_object_max_bytes: 4 * 1024 * 1024,
            sweep_interval_secs: 30,
            max_disk_bytes: 0,
            cache_range_requests: false,
            cache_mode: CacheMode::Dash,
            origin_timeout_ms: 10_000,
            tls_cert_pem: None,
            tls_key_pem: None,
            mpd_memory_ttl_secs: 1,
            mpd_disk_ttl_secs: 1,
            mpd_memory_max_bytes: 8 * 1024 * 1024,
            key_normalization_mode: KeyNormalizationMode::None,
            key_query_whitelist: Vec::new(),
            key_query_blacklist: Vec::new(),
        }
    }

    #[tokio::test]
    async fn finalize_missing_body_is_noop() -> Result<(), AppError> {
        let cfg = test_config("missing_body");
        let cache = DiskCache::new(&cfg).await?;
        let key = CacheKey([1; 32]);

        cache
            .finalize_meta_atomic_with_ttl(key, StatusCode::OK, Vec::new(), 60)
            .await?;

        assert!(cache.get(key).await?.is_none());
        let _ = tokio::fs::remove_dir_all(&cfg.cache_dir).await;
        Ok(())
    }

    #[tokio::test]
    async fn finalize_publishes_meta_when_body_exists() -> Result<(), AppError> {
        let cfg = test_config("publish");
        let cache = DiskCache::new(&cfg).await?;
        let key = CacheKey([2; 32]);
        let (_, body_path) = cache.paths_for_key(key);

        tokio::fs::create_dir_all(body_path.parent().expect("body parent")).await?;
        tokio::fs::write(&body_path, b"body").await?;

        cache
            .finalize_meta_atomic_with_ttl(
                key,
                StatusCode::OK,
                vec![("content-type".to_string(), "text/plain".to_string())],
                60,
            )
            .await?;

        let hit = cache.get(key).await?.expect("disk hit");
        assert_eq!(hit.status, StatusCode::OK);
        assert_eq!(tokio::fs::read(&hit.body_path).await?, b"body");
        let _ = tokio::fs::remove_dir_all(&cfg.cache_dir).await;
        Ok(())
    }

    #[test]
    fn temp_meta_paths_are_unique() {
        let path = PathBuf::from("/tmp/cache/aa/bb/key.json");

        assert_ne!(tmp_path(&path), tmp_path(&path));
    }
}

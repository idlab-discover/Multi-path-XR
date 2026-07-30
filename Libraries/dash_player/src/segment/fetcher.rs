use bytes::Bytes;
use reqwest::{Client, StatusCode};
use std::error::Error;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::error;

/// Error type used by the fetcher; must be Send + Sync to be spawn-able.
pub type FetchError = Box<dyn Error + Send + Sync>;

const ORIGIN_SYNC_WARMUP_SAMPLES: u8 = 3;
const ORIGIN_SYNC_ACCEPT_RTT_MULTIPLIER: f64 = 2.0;
const ORIGIN_SYNC_ACCEPT_RTT_MARGIN_MS: f64 = 20.0;
const ORIGIN_SYNC_RTT_FLOOR_RISE_ALPHA: f64 = 0.05;
const ORIGIN_SYNC_MAX_APPLIED_AGE_MS: f64 = 10_000.0;

#[derive(Clone, Copy, Debug)]
struct OriginOffsetSample {
    offset_ms: f64,
    rtt_ms: f64,
    observed_client_ms: f64,
}

/**
 * Represents network timing information.
 * It tracks the clock offset and one-way latency between the client and server.
 */
pub struct NetTime {
    /// serving hop - client (ms). Positive means the serving hop is ahead.
    serving_hop_clock_offset_ms: Option<f64>,
    /// origin server - client (ms). Positive means the origin is ahead.
    origin_clock_offset_ms: Option<f64>,
    origin_clock_source_id: Option<String>,
    /// Decaying RTT floor for uncached origin-time probes.
    origin_rtt_floor_ms: Option<f64>,
    /// Client wall-clock when the currently applied origin offset was accepted.
    origin_last_accepted_client_ms: Option<f64>,
    /// Lowest-RTT sample seen while bootstrapping origin sync.
    origin_warmup_best_sample: Option<OriginOffsetSample>,
    /// Number of uncached origin-time probes collected for the current warmup burst.
    origin_warmup_sample_count: u8,
    /// EWMA of client->server one-way latency (ms)
    one_way_cs_ms: f64,
    /// EWMA smoothing
    alpha: f64,
}

impl NetTime {
    pub fn new(alpha: f64) -> Self {
        Self {
            serving_hop_clock_offset_ms: None,
            origin_clock_offset_ms: None,
            origin_clock_source_id: None,
            origin_rtt_floor_ms: None,
            origin_last_accepted_client_ms: None,
            origin_warmup_best_sample: None,
            origin_warmup_sample_count: 0,
            one_way_cs_ms: 12.0,
            alpha,
        }
    }

    fn update_offset(slot: &mut Option<f64>, sample_ms: f64, alpha: f64) {
        let next = match *slot {
            Some(current_ms) => alpha * sample_ms + (1.0 - alpha) * current_ms,
            None => sample_ms,
        };
        *slot = Some(next);
    }

    fn clock_offset_sample_ms(
        now_client_ms: f64,
        ttfb_s: f64,
        server_wait_ms: Option<u64>,
        server_now_ms: u128,
    ) -> f64 {
        let network_rtt_ms = ((ttfb_s * 1000.0) - server_wait_ms.unwrap_or(0) as f64).max(0.0);
        server_now_ms as f64 + (network_rtt_ms / 2.0) - now_client_ms
    }

    fn origin_probe_rtt_ms(ttfb_s: f64) -> f64 {
        let rtt_ms = ttfb_s * 1000.0;
        if rtt_ms.is_finite() {
            rtt_ms.clamp(0.0, 60_000.0)
        } else {
            60_000.0
        }
    }

    fn note_origin_rtt_sample(&mut self, sample_rtt_ms: f64) {
        let next_floor_ms = match self.origin_rtt_floor_ms {
            Some(current_floor_ms) if sample_rtt_ms <= current_floor_ms => sample_rtt_ms,
            Some(current_floor_ms) => {
                current_floor_ms
                    + (sample_rtt_ms - current_floor_ms) * ORIGIN_SYNC_RTT_FLOOR_RISE_ALPHA
            }
            None => sample_rtt_ms,
        };
        self.origin_rtt_floor_ms = Some(next_floor_ms);
    }

    fn origin_rtt_acceptance_limit_ms(&self) -> Option<f64> {
        self.origin_rtt_floor_ms.map(|floor_ms| {
            (floor_ms * ORIGIN_SYNC_ACCEPT_RTT_MULTIPLIER)
                .max(floor_ms + ORIGIN_SYNC_ACCEPT_RTT_MARGIN_MS)
        })
    }

    fn accept_origin_sample(
        &mut self,
        sample: OriginOffsetSample,
        clock_source_id: Option<String>,
    ) {
        Self::update_offset(
            &mut self.origin_clock_offset_ms,
            sample.offset_ms,
            self.alpha,
        );
        self.origin_last_accepted_client_ms = Some(sample.observed_client_ms);
        if clock_source_id.is_some() {
            self.origin_clock_source_id = clock_source_id;
        }
    }

    fn observe_origin_warmup_sample(
        &mut self,
        sample: OriginOffsetSample,
        clock_source_id: Option<String>,
    ) -> bool {
        let replace_best = self
            .origin_warmup_best_sample
            .is_none_or(|best_sample| sample.rtt_ms < best_sample.rtt_ms);
        if replace_best {
            self.origin_warmup_best_sample = Some(sample);
        }

        self.origin_warmup_sample_count = self.origin_warmup_sample_count.saturating_add(1);
        if self.origin_warmup_sample_count < ORIGIN_SYNC_WARMUP_SAMPLES {
            return false;
        }

        let accepted = if let Some(best_sample) = self.origin_warmup_best_sample.take() {
            self.accept_origin_sample(best_sample, clock_source_id);
            true
        } else {
            false
        };
        self.origin_warmup_sample_count = 0;
        accepted
    }

    /// Call this after each segment response to track the currently serving hop.
    /// now_client_ms: coarse wall-clock in ms at header arrival
    pub fn observe_serving_hop(
        &mut self,
        now_client_ms: f64,
        ttfb_s: f64,
        server_wait_ms: Option<u64>,
        serving_hop_now_ms: Option<u128>,
    ) {
        if let Some(serving_hop_now_ms) = serving_hop_now_ms {
            let offset_ms = Self::clock_offset_sample_ms(
                now_client_ms,
                ttfb_s,
                server_wait_ms,
                serving_hop_now_ms,
            );
            Self::update_offset(&mut self.serving_hop_clock_offset_ms, offset_ms, self.alpha);
        }

        // TTFB includes cs + server_wait + sc. Approx one-way cs ≈ (TTFB - wait)/2
        if let Some(wait) = server_wait_ms {
            let ttfb_ms = ttfb_s * 1000.0;
            let est_one_way = ((ttfb_ms - wait as f64).max(0.0)) / 2.0;
            // Clamp to reasonable bounds to kill outliers
            let est_one_way = est_one_way.clamp(0.2, 300.0);
            self.one_way_cs_ms = self.alpha * est_one_way + (1.0 - self.alpha) * self.one_way_cs_ms;
        }
    }

    #[inline]
    pub fn observe_origin(
        &mut self,
        now_client_ms: f64,
        ttfb_s: f64,
        origin_now_ms: u128,
        clock_source_id: Option<String>,
    ) -> bool {
        let sample = OriginOffsetSample {
            offset_ms: Self::clock_offset_sample_ms(now_client_ms, ttfb_s, None, origin_now_ms),
            rtt_ms: Self::origin_probe_rtt_ms(ttfb_s),
            observed_client_ms: now_client_ms,
        };
        let accepted = if self.origin_clock_offset_ms.is_none() {
            self.observe_origin_warmup_sample(sample, clock_source_id)
        } else {
            let acceptance_limit_ms = self
                .origin_rtt_acceptance_limit_ms()
                .unwrap_or(f64::INFINITY);
            if sample.rtt_ms <= acceptance_limit_ms {
                self.accept_origin_sample(sample, clock_source_id);
                true
            } else {
                false
            }
        };
        self.note_origin_rtt_sample(sample.rtt_ms);
        accepted
    }

    #[inline]
    pub fn serving_hop_clock_offset_ms(&self) -> Option<f64> {
        self.serving_hop_clock_offset_ms
    }

    #[inline]
    pub fn origin_clock_offset_ms(&self) -> Option<f64> {
        self.origin_clock_offset_ms
    }

    #[inline]
    pub fn origin_clock_source_id(&self) -> Option<&str> {
        self.origin_clock_source_id.as_deref()
    }

    #[inline]
    pub fn usable_origin_clock_offset_ms(&self, now_client_ms: f64) -> Option<f64> {
        let offset_ms = self.origin_clock_offset_ms?;
        let last_accepted_client_ms = self.origin_last_accepted_client_ms?;
        let age_ms = (now_client_ms - last_accepted_client_ms).max(0.0);
        if age_ms <= ORIGIN_SYNC_MAX_APPLIED_AGE_MS {
            Some(offset_ms)
        } else {
            None
        }
    }

    #[inline]
    pub fn one_way_cs_ms(&self) -> f64 {
        self.one_way_cs_ms
    }
}

/**
 * Represents the early bias in network timing.
 * It tracks how long the server takes to process the request internally.
 */
pub struct EarlyBias {
    bias_ms: f64,
    alpha: f64,
}

impl EarlyBias {
    pub fn new(alpha: f64) -> Self {
        Self {
            bias_ms: 0.0,
            alpha,
        }
    }
    pub fn observe(&mut self, backend_wait_ms: Option<u64>) {
        if let Some(w) = backend_wait_ms {
            let w = (w as f64).clamp(0.0, 200.0);
            self.bias_ms = self.alpha * w + (1.0 - self.alpha) * self.bias_ms;
        }
    }
    pub fn value(&self) -> f64 {
        self.bias_ms
    }
}

/// Cache outcome as reported by the nearest proxy/CDN hop (best-effort).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheStatus {
    HitMem,
    HitDisk,
    Hit,
    Miss,
    Inflight,
    Bypass,
    Unknown,
}

impl CacheStatus {
    pub fn from_x_cache(raw: &str) -> Self {
        let token = raw
            .split(|c: char| c == ',' || c == ';' || c.is_whitespace())
            .next()
            .unwrap_or("");

        if token.eq_ignore_ascii_case("mem") {
            Self::HitMem
        } else if token.eq_ignore_ascii_case("disk") {
            Self::HitDisk
        } else if token.eq_ignore_ascii_case("hit") {
            Self::Hit
        } else if token.eq_ignore_ascii_case("miss") {
            Self::Miss
        } else if token.eq_ignore_ascii_case("inflight") {
            Self::Inflight
        } else if token.eq_ignore_ascii_case("bypass") {
            Self::Bypass
        } else {
            Self::Unknown
        }
    }
}

pub struct SegmentDownload {
    pub bytes: Bytes,
    /// Start -> headers received
    pub ttfb_s: f64, // Time to first byte
    /// First-body-byte -> last-body-byte
    pub body_s: f64,
    /// Start -> last-body-byte
    pub total_s: f64,
    /// Server-reported internal wait (ms), if provided
    pub server_wait_ms: Option<u64>,
    /// Serving-hop-reported current time (ms), if provided
    pub serving_hop_now_ms: Option<u128>,
    /// Client wall-clock at header arrival (ms since UNIX epoch)
    pub header_arrival_client_ms: Option<u128>,
    pub cache_status: CacheStatus,
}

pub struct OriginTimeSignal {
    pub ttfb_s: f64,
    pub origin_now_ms: u128,
    pub origin_receive_us: Option<u64>,
    pub origin_send_us: Option<u64>,
    pub clock_source_id: Option<String>,
    pub header_arrival_client_ms: u128,
}

/// Downloads a segment and returns (bytes, download_duration)
/// Retries a few times with exponential backoff if needed.
pub async fn fetch_segment(client: &Client, url: &str) -> Result<SegmentDownload, FetchError> {
    const MAX_RETRIES: usize = 0;
    const BASE_DELAY_MS: u64 = 500;

    for attempt in 0..=MAX_RETRIES {
        let start_total = Instant::now();
        let result = client.get(url).send().await;

        match result {
            Ok(response) => {
                if response.status().is_success() {
                    // TTFB (headers received)
                    let ttfb_s = start_total.elapsed().as_secs_f64();
                    let header_arrival_client_ms = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .ok()
                        .map(|duration| duration.as_millis());

                    // Optional server-internal wait time for diagnostics
                    let server_wait_ms = response
                        .headers()
                        .get("x-backend-wait-ms")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.parse::<u64>().ok());

                    let serving_hop_now_ms = response
                        .headers()
                        .get("x-backend-now-ms")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.parse::<u128>().ok());

                    let cache_status = {
                        let mut last = CacheStatus::Unknown;
                        for v in response.headers().get_all("x-cache").iter() {
                            if let Ok(s) = v.to_str() {
                                last = CacheStatus::from_x_cache(s);
                            }
                        }
                        last
                    };

                    // === Measure first-byte -> last-byte ===
                    let bytes = response.bytes().await?;

                    let total_s = start_total.elapsed().as_secs_f64();
                    let body_s = total_s - ttfb_s;

                    // Safety against division-by-zero upstream
                    let body_s = if body_s <= 0.0 { f64::EPSILON } else { body_s };

                    return Ok(SegmentDownload {
                        bytes,
                        ttfb_s,
                        body_s,
                        total_s,
                        server_wait_ms,
                        serving_hop_now_ms,
                        header_arrival_client_ms,
                        cache_status,
                    });
                } else if response.status() == StatusCode::NOT_FOUND {
                    // 404: don't retry
                    return Err(format!("404 Not Found: {url}").into());
                } else {
                    error!("Warning: Received {} from {}", response.status(), url);
                }
            }
            Err(e) => {
                error!("Warning: Fetch failed (attempt {}): {}", attempt + 1, e);
            }
        }
        #[allow(clippy::absurd_extreme_comparisons)]
        if attempt + 1 < MAX_RETRIES {
            let delay = Duration::from_millis(BASE_DELAY_MS * 2u64.pow(attempt as u32));
            tokio::time::sleep(delay).await;
        }
    }

    Err(format!(
        "Failed to fetch segment after {} attempts: {}",
        MAX_RETRIES + 1,
        url
    )
    .into())
}

pub async fn fetch_origin_time_signal(
    client: &Client,
    url: &str,
) -> Result<OriginTimeSignal, FetchError> {
    let start_total = Instant::now();
    let response = client
        .get(url)
        .header("Cache-Control", "no-cache")
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(format!(
            "origin time signal failed with {}: {url}",
            response.status()
        )
        .into());
    }

    let ttfb_s = start_total.elapsed().as_secs_f64();
    let header_arrival_client_ms = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();

    let origin_now_ms = response
        .headers()
        .get("x-origin-now-ms")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u128>().ok())
        .ok_or_else(|| format!("origin time signal missing x-origin-now-ms: {url}"))?;
    let origin_receive_us = response
        .headers()
        .get("x-origin-receive-us")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());
    let origin_send_us = response
        .headers()
        .get("x-origin-send-us")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());
    let clock_source_id = response
        .headers()
        .get("x-clock-source-id")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);

    let _ = response.bytes().await?;

    Ok(OriginTimeSignal {
        ttfb_s,
        origin_now_ms,
        origin_receive_us,
        origin_send_us,
        clock_source_id,
        header_arrival_client_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn net_time_observe_estimates_network_latency_and_offsets() {
        let mut net_time = NetTime::new(0.25);
        net_time.observe_serving_hop(1_000.0, 0.040, Some(10), Some(1_020));
        net_time.observe_origin(1_000.0, 0.020, 1_015, None);
        net_time.observe_origin(2_000.0, 0.020, 2_015, None);
        net_time.observe_origin(3_000.0, 0.020, 3_015, None);

        assert!((net_time.one_way_cs_ms() - 12.75).abs() < f64::EPSILON);
        assert_eq!(net_time.serving_hop_clock_offset_ms(), Some(35.0));
        assert_eq!(net_time.origin_clock_offset_ms(), Some(25.0));
    }

    #[test]
    fn net_time_origin_sync_warmup_prefers_the_lowest_rtt_sample() {
        let mut net_time = NetTime::new(0.25);

        assert!(!net_time.observe_origin(1_000.0, 0.120, 1_020, None));
        assert!(!net_time.observe_origin(2_000.0, 0.010, 2_020, None));
        assert!(net_time.observe_origin(3_000.0, 0.080, 3_020, None));

        assert_eq!(net_time.origin_clock_offset_ms(), Some(25.0));
        assert_eq!(net_time.usable_origin_clock_offset_ms(2_500.0), Some(25.0));
    }

    #[test]
    fn net_time_origin_sync_rejects_slow_outliers_and_expires_stale_alignment() {
        let mut net_time = NetTime::new(0.25);

        net_time.observe_origin(1_000.0, 0.100, 1_020, None);
        net_time.observe_origin(2_000.0, 0.010, 2_020, None);
        net_time.observe_origin(3_000.0, 0.080, 3_020, None);

        assert_eq!(net_time.origin_clock_offset_ms(), Some(25.0));
        assert!(!net_time.observe_origin(4_000.0, 0.200, 4_020, None));
        assert_eq!(net_time.origin_clock_offset_ms(), Some(25.0));
        assert_eq!(net_time.usable_origin_clock_offset_ms(11_000.0), Some(25.0));
        assert_eq!(net_time.usable_origin_clock_offset_ms(12_100.0), None);
    }
}

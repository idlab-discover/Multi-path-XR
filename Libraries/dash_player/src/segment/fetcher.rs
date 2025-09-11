use bytes::Bytes;
use reqwest::{Client, StatusCode};
use tracing::error;
use std::time::{Duration, Instant};
use std::error::Error;

/// Error type used by the fetcher; must be Send + Sync to be spawn-able.
pub type FetchError = Box<dyn Error + Send + Sync>;

pub struct BandwidthEstimator {
    ewma: f64,
    initialized: bool,
    alpha: f64,
}

impl BandwidthEstimator {
    pub fn new(alpha: f64) -> Self {
        Self { ewma: 0.0, initialized: false, alpha }
    }

    /**
     * Records the number of bytes downloaded and the time taken in seconds.
     */
    pub fn record(&mut self, bytes: usize, duration_s: f64) {
        // If the duration is zero, we can't compute a sample
        if duration_s == 0.0 {
            // Just ignore this sample
            return;
        } else if duration_s < 0.0 {
            // Negative duration is invalid
            error!("Warning: Negative duration observed in bandwidth recording: {}", duration_s);
            return;
        }

        let sample = (bytes as f64 * 8.0) / duration_s;
        self.ewma = if self.initialized {
            self.alpha * sample + (1.0 - self.alpha) * self.ewma
        } else {
            self.initialized = true;
            sample   // first sample
        };
    }

    /**
     * Returns the estimated bandwidth in bits per second.
     * If no samples are recorded, returns 50 Mbps.
     */
    pub fn estimate(&self) -> f64 {
        if self.initialized { self.ewma } else { 50_000_000.0 }
    }
}

/**
 * Represents network timing information.
 * It tracks the clock offset and one-way latency between the client and server.
 */
pub struct NetTime {
    /// server - client (ms). Positive means server is ahead.
    clock_offset_ms: f64,
    /// EWMA of client->server one-way latency (ms)
    one_way_cs_ms: f64,
    /// EWMA smoothing
    alpha: f64,
}

impl NetTime {
    pub fn new(alpha: f64) -> Self {
        Self { clock_offset_ms: 0.0, one_way_cs_ms: 12.0, alpha }
    }

    /// Call this after each segment response.
    /// now_client_ms: monotonic or coarse wall-clock in ms at header arrival
    pub fn observe(&mut self, now_client_ms: f64, ttfb_s: f64, server_wait_ms: Option<u64>, server_now_ms: Option<u128>) {
        if let Some(sn) = server_now_ms {
            let offset = sn as f64 - now_client_ms;
            self.clock_offset_ms = self.alpha * offset + (1.0 - self.alpha) * self.clock_offset_ms;
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
    pub fn clock_offset_ms(&self) -> f64 { self.clock_offset_ms }

    #[inline]
    pub fn one_way_cs_ms(&self) -> f64 { self.one_way_cs_ms }
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
    pub fn new(alpha: f64) -> Self { Self { bias_ms: 0.0, alpha } }
    pub fn observe(&mut self, backend_wait_ms: Option<u64>) {
        if let Some(w) = backend_wait_ms {
            let w = (w as f64).clamp(0.0, 200.0);
            self.bias_ms = self.alpha * w + (1.0 - self.alpha) * self.bias_ms;
        }
    }
    pub fn value(&self) -> f64 { self.bias_ms }
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
    /// Server-reported current time (ms), if provided
    pub server_now_ms: Option<u128>,
}

/// Downloads a segment and returns (bytes, download_duration)
/// Retries a few times with exponential backoff if needed.
pub async fn fetch_segment(
    client: &Client,
    url: &str,
) -> Result<SegmentDownload, FetchError> {
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

                    // Optional server-internal wait time for diagnostics
                    let server_wait_ms = response
                        .headers()
                        .get("X-Backend-Wait-ms")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.parse::<u64>().ok());

                    let server_now_ms = response
                        .headers()
                        .get("X-Server-Now-ms")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.parse::<u128>().ok());

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
                        server_now_ms,
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

    Err(format!("Failed to fetch segment after {} attempts: {}", MAX_RETRIES + 1, url).into())
}

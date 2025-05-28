use bytes::Bytes;
use reqwest::{Client, StatusCode};
use tracing::error;
use std::time::{Duration, Instant};

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

/// Downloads a segment and returns (bytes, download_duration)
/// Retries a few times with exponential backoff if needed.
pub async fn fetch_segment(
    client: &Client,
    url: &str,
) -> Result<(Bytes, f64), Box<dyn std::error::Error>> {
    const MAX_RETRIES: usize = 0;
    const BASE_DELAY_MS: u64 = 500;

    for attempt in 0..=MAX_RETRIES {
        let start = Instant::now();
        let result = client.get(url).send().await;

        match result {
            Ok(response) => {
                if response.status().is_success() {
                    let bytes = response.bytes().await?;
                    let duration_secs = start.elapsed().as_secs_f64();
                    return Ok((bytes, duration_secs));
                } else if response.status() == StatusCode::NOT_FOUND {
                    // 404: don't retry
                    return Err(format!("404 Not Found: {}", url).into());
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

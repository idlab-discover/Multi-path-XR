use crate::mpd::MpdMetadata;
use crate::segment::fetcher::{BandwidthEstimator, fetch_segment};
use crate::DashEvent;
use chrono::{DateTime, Utc};
use reqwest::Client;
use tracing::debug;
use std::collections::HashSet;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use regex::Regex;
use once_cell::sync::Lazy;

pub type SegmentCallback = Arc<dyn Fn(DashEvent) + Send + Sync>;

// This regex matches the $Number format in DASH segment URLs.
static RE_NUMBER_FROM_URL: Lazy<Regex> = Lazy::new(|| Regex::new(r"\$Number(?::%0(\d+)d|%0(\d+)d)?\$").unwrap());


pub struct DashPlayer {
    mpd_url: String,
    client: Client,
    callback: SegmentCallback,
    mpd_data: Arc<RwLock<MpdMetadata>>,
    media_cache: Arc<Mutex<HashSet<String>>>,
    init_cache: Arc<Mutex<HashSet<String>>>,
    cancellation_token: Arc<CancellationToken>,
    target_latency: Arc<Mutex<Duration>>,
    fetching_enabled: Arc<AtomicBool>,
}

impl DashPlayer {
    pub async fn new(url: &str, callback: SegmentCallback) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let client = Client::new();
        let mpd_text = client.get(url).send().await?.text().await?;
        let mpd_data = crate::mpd::parser::parse_mpd(&mpd_text)?;

        Ok(Self {
            mpd_url: url.to_string(),
            client,
            callback,
            mpd_data: Arc::new(RwLock::new(mpd_data)),
            media_cache: Arc::new(Mutex::new(HashSet::new())),
            init_cache: Arc::new(Mutex::new(HashSet::new())),
            cancellation_token: Arc::new(CancellationToken::new()),
            target_latency: Arc::new(Mutex::new(Duration::from_secs_f64(3.0))),
            fetching_enabled: Arc::new(AtomicBool::new(true)),
        })
    }

    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
        let mpd_data = self.mpd_data.read().await.clone();

        //info!("{mpd_data:?}");

        // We need to spawn one task per adaptation set
        for adaptation in &mpd_data.adaptation_sets {
            self.spawn_segment_fetcher(
                adaptation.clone(),
                mpd_data.availability_start_time,
                mpd_data.time_shift_buffer_depth.unwrap_or(f64::INFINITY),
                self.fetching_enabled.clone(),
            ).await;
        }
        Ok(())
    }

    pub fn stop(&self) {
        // First, force stop the segment fetchers
        // This is just a safety measure, in case the cancellation token is not respected
        self.set_fetching_enabled(false);
        // Now we can cancel the task
        self.cancellation_token.cancel();
    }

    pub async fn refresh_mpd(&self) {
        match self.client.get(&self.mpd_url).send().await {
            Ok(resp) => match resp.text().await {
                Ok(text) => match crate::mpd::parser::parse_mpd(&text) {
                    Ok(updated) => {
                        *self.mpd_data.write().await = updated;
                        (self.callback)(DashEvent::Info("MPD refreshed".to_string()));
                    }
                    Err(e) => (self.callback)(DashEvent::Warning(format!("MPD parse error: {e}"))),
                },
                Err(e) => (self.callback)(DashEvent::Warning(format!("Failed to read MPD: {e}"))),
            },
            Err(e) => (self.callback)(DashEvent::Warning(format!("Failed to fetch MPD: {e}"))),
        }
    }

    pub fn set_fetching_enabled(&self, enabled: bool) {
        self.fetching_enabled.store(enabled, Ordering::Relaxed);
    }

    pub async fn set_target_latency(&self, latency: f64) {
        let mut target_latency = self.target_latency.lock().await;
        *target_latency = Duration::from_secs_f64(latency);
    }

    pub async fn get_target_latency(&self) -> f64 {
        let target_latency = self.target_latency.lock().await;
        target_latency.as_secs_f64()
    }

    async fn spawn_segment_fetcher(
        &self,
        adaptation: crate::mpd::AdaptationSet,
        availability_start_time: DateTime<Utc>,
        time_shift_buffer: f64,
        fetching_enabled: Arc<AtomicBool>,
    ) {
        let base_url = self.mpd_url.rsplit_once('/').map(|(base, _)| base).unwrap_or("").to_string();
        let callback = self.callback.clone();
        let media_cache = self.media_cache.clone();
        let init_cache = self.init_cache.clone();
        let client = self.client.clone();
        let cancellation_token = self.cancellation_token.clone();
        let target_latency = self.target_latency.clone();

        tokio::spawn(async move {
            const MAX_IN_FLIGHT: usize = 10;     // cap concurrent fetches
            let  fetch_ahead_ms: u64 = 30;      // How many ms our loop will fetch ahead of playback

            let estimator = Arc::new(Mutex::new(BandwidthEstimator::new(0.25)));
            let reps = &adaptation.representations;
            if reps.is_empty() {
                callback(DashEvent::Warning("No representations found".to_string()));
                return;
            }

            let content_type = adaptation.content_type.clone();

            // In-flight fetch tasks (we cap by its len, pruning finished ones)
            let mut inflight: Vec<tokio::task::JoinHandle<()>> = Vec::new();

            let mut segment_number: u64 = 0;

            tokio::select! {
                // Check for cancellation
                _ = cancellation_token.cancelled() => {
                    callback(DashEvent::Info("Segment fetcher cancelled.".to_string()));
                }
                _ = async {
                    while !cancellation_token.is_cancelled() {
                        let loop_start = Instant::now(); 
                        inflight.retain(|h| !h.is_finished());

                        // Check if we are at the maximum number of in-flight fetches, if so, wait a bit
                        while inflight.len() >= MAX_IN_FLIGHT {
                            // Remove finished tasks
                            inflight.retain(|h| !h.is_finished());
                            sleep(Duration::from_millis(5)).await;
                        }

                        // TODO:: update all the calculations that need to be calculated such that we can run our loop X ms ahead of time (= fetch_ahead_ms)
                        // I think that we must overwrite the  available_at, current_latency, wait_time, earliest_allowed stuff, but I'm not certain. It might be possible that we need to update other stuff as well.

                        let est_bw = { estimator.lock().await.estimate() };
                        // This selects the best representation based on the estimated bandwidth
                        let selected = select_representation(reps, est_bw);
                        let seg_duration = selected.segment_duration;
                        let seg_start_time = segment_number as f64 * seg_duration;
                        let uptime = Utc::now().signed_duration_since(availability_start_time).to_std().unwrap_or_default().as_secs_f64();

                        let target_latency_seconds = {
                            target_latency.lock().await.as_secs_f64()
                        };

                        let live_edge = uptime;
                        let earliest_allowed = (live_edge - time_shift_buffer).max(0.0);
                        //info!("Segment {}: {seg_start_time}, {uptime}, {earliest_allowed}", segment_number);
                        if seg_start_time < earliest_allowed {
                            debug!("Segment {} is not available anymore, skipping to next segment", segment_number);
                            segment_number += 1;
                            continue;
                        }

                        let ato = selected.availability_time_offset.unwrap_or(0.0);
                        let atc = selected.availability_time_complete.unwrap_or(true);
        
                        let segment_wallclock_time = availability_start_time + chrono::Duration::from_std(Duration::from_secs_f64(seg_start_time)).unwrap();
                        let available_at = if atc {
                            segment_wallclock_time
                        } else {
                            let offset = seg_duration - ato;
                            if offset >= 0.0 {
                                segment_wallclock_time + chrono::Duration::from_std(Duration::from_secs_f64(offset)).unwrap()
                            } else {
                                segment_wallclock_time - chrono::Duration::from_std(Duration::from_secs_f64(offset * -1.0)).unwrap()
                            }
                        };
        
                        if Utc::now() < available_at {
                            // Calculate how long to wait until the segment is available
                            let wait_time = available_at.signed_duration_since(Utc::now()).to_std().unwrap_or_default();
                            let wait_time_ms = wait_time.as_millis();
                            if wait_time_ms > 0 {
                                //debug!("Waiting for {} ms until segment {} is available", wait_time_ms, segment_number);
                                sleep(wait_time).await;
                            }
                        }

                        let current_latency = {
                            let mut diff = uptime - seg_start_time;
                            if diff < 0.0 { // Clamp to zero, the segment is not available yet
                                diff = 0.0;
                            }
                            if atc {
                                Duration::from_secs_f64(diff)
                            } else {
                                // If the segment is not complete, we need to adjust the latency
                                diff -= ato;
                                if diff < 0.0 { // Clamp to zero, the segment is not available yet
                                    diff = 0.0;
                                }
                                Duration::from_secs_f64(diff)
                            }
                        };
                        let latency_diff = {
                            current_latency.as_secs_f64() - target_latency_seconds
                        };
                        // Proportional gain tuned for small durations and aggressive latency correction
                        // Higher value for quicker catch-up, lower for smoother
                        let k_p = 1.2;
                        let playback_rate = adjust_playback_rate(latency_diff, k_p);

                        if !fetching_enabled.load(Ordering::Relaxed) {
                            let sn_clone = segment_number.clone();
                            let wait_time_ms = fetch_ahead_ms;
                            let cb = callback.clone();
                            let handle = tokio::spawn(async move {
                                // Wait the fetch_ahead_ms time, to undo the prefetching of this loop
                                // That way, we simulate the network latency and potential waiting at the server until the segment is available
                                sleep(Duration::from_millis(wait_time_ms)).await;
                                cb(DashEvent::EmptySegment {
                                    segment_number: sn_clone,
                                });
                            });
                            inflight.push(handle);

                            segment_number += 1;
                            // length of one playback interval at the *current* rate
                            let target_interval = if playback_rate > 1.0 {
                                seg_duration / playback_rate
                            } else {
                                // We don't want to slow down the fetching, when we slow down the playback.
                                // If we start the next loop iteration too early, there is still a check with a wait action to account for this.
                                // So capping the target interval to the segment duration is not a problem.
                                seg_duration
                            };
                            // Time it took to complete this iteration (including the download)
                            let elapsed = loop_start.elapsed().as_secs_f64();

                            if elapsed < target_interval {
                                sleep(Duration::from_secs_f64(target_interval - elapsed)).await;
                            } else {
                                tokio::task::yield_now().await; // Yield to allow other tasks to run
                            }
                            continue;
                        }

                        let segment_url = format!(
                            "{}/{}",
                            base_url,
                            replace_number_format(
                                &selected.media
                                    .replace("$Time$", &((segment_number as f64 * selected.timescale as f64).round() as u64).to_string())
                                    .replace("$RepresentationID$", &selected.id),
                                segment_number)
                        );

                        {
                            // Prevent downloading the same segment multiple times
                            let mut downloaded = media_cache.lock().await;
                            if downloaded.contains(&segment_url) {
                                // Go to the next segment
                                segment_number += 1;

                                let target_interval = if playback_rate > 1.0 {
                                    seg_duration / playback_rate
                                } else {
                                    // We don't want to slow down the fetching, when we slow down the playback.
                                    // If we start the next loop iteration too early, there is still a check with a wait action to account for this.
                                    // So capping the target interval to the segment duration is not a problem.
                                    seg_duration
                                };
                                // Time it took to complete this iteration (including the download)
                                let elapsed = loop_start.elapsed().as_secs_f64();

                                if elapsed < target_interval {
                                    sleep(Duration::from_secs_f64(target_interval - elapsed)).await;
                                } else {
                                    tokio::task::yield_now().await; // Yield to allow other tasks to run
                                }
                                continue;
                            }
                            // From now on, we will assume that the segment is downloaded
                            downloaded.insert(segment_url.clone());
                        }

                        {
                            // If we have not downloaded the initialization segment for this representation yet
                            // then we will do so now
                            let mut inits = init_cache.lock().await;
                            let init_key = format!("{}::{}", selected.id, selected.initialization);
                            if !inits.contains(&init_key) {
                                let init_url = format!("{}/{}", base_url, selected.initialization);
                                // The fetching of the init segment will be done in this loop, not as a separate task
                                // Just to make the logic a bit simpler, while being ok performance-wise
                                match fetch_segment(&client, &init_url).await {
                                    Ok(segment_download) => {
                                        let init_data = segment_download.bytes;
                                        let dur = if segment_download.server_wait_ms.is_some() {
                                            segment_download.total_s - (segment_download.server_wait_ms.unwrap_or(0) as f64 / 1000.0)
                                        } else {
                                            segment_download.total_s
                                        };

                                        let length = init_data.len();
                                        callback(DashEvent::Segment {
                                            data: init_data,
                                            content_type: content_type.clone(),
                                            representation_id: selected.id.clone(),
                                            segment_number: 0,
                                            duration: 0.0,
                                            url: init_url,
                                            playback_rate,
                                        });
                                        inits.insert(init_key);
                                        { estimator.lock().await.record(length, dur); };
                                    }
                                    Err(e) => {
                                        callback(DashEvent::DownloadError {
                                            url: init_url,
                                            reason: format!("{e}"),
                                        });
                                    }
                                }
                            }
                        }

                        let est_clone = estimator.clone();
                        let cb = callback.clone();
                        let client_clone = client.clone();
                        let ct = content_type.clone();
                        let rep_id = selected.id.clone();
                        let seg_url = segment_url.clone();
                        let media_cache = media_cache.clone();
                        let handle = tokio::spawn(async move {
                            match fetch_segment(&client_clone, &seg_url).await {
                                Ok(segment_download) => {
                                    let media_data = segment_download.bytes;
                                    let dur = if segment_download.server_wait_ms.is_some() {
                                        segment_download.total_s - (segment_download.server_wait_ms.unwrap_or(0) as f64 / 1000.0)
                                    } else {
                                        segment_download.total_s
                                    };
                                    // info!("Estimated Bandwidth was: {}, rate: {}", est_bw, playback_rate);
                                    let length = media_data.len();
                                    cb(DashEvent::Segment {
                                        data: media_data,
                                        content_type: ct.clone(),
                                        representation_id: rep_id.clone(),
                                        segment_number,
                                        duration: seg_duration,
                                        url: seg_url.clone(),
                                        playback_rate,
                                    });
                                    { est_clone.lock().await.record(length, dur); };
                                    // Also put the segment in the cache
                                    { media_cache.lock().await.insert(seg_url.clone()); };
                                }
                                Err(e) => {
                                    cb(DashEvent::DownloadError {
                                        url: seg_url.clone(),
                                        reason: format!("{e}"),
                                    });
                                }
                            }
                        });
                        inflight.push(handle);

                        segment_number += 1;

                        // length of one playback interval at the *current* rate
                        let target_interval = if playback_rate > 1.0 {
                            seg_duration / playback_rate
                        } else {
                            // We don't want to slow down the fetching, when we slow down the playback.
                            // If we start the next loop iteration too early, there is still a check with a wait action to account for this.
                            // So capping the target interval to the segment duration is not a problem.
                            seg_duration
                        };

                        // Time it took to complete this iteration (including the download)
                        let elapsed = loop_start.elapsed().as_secs_f64();

                        if elapsed < target_interval {
                            sleep(Duration::from_secs_f64(target_interval - elapsed)).await;
                        } else {
                            tokio::task::yield_now().await; // Yield to allow other tasks to run
                        }
                    }
                    
                    callback(DashEvent::Info("Segment fetcher cancelled the loop.".to_string()));
                } => {}
            }
        });
    }
}

fn select_representation<'a>(reps: &'a [crate::mpd::Representation], mut est_bw: f64) -> &'a crate::mpd::Representation {
    // Reduce the estimated bandwidth by 5% to account for overhead
    est_bw *= 0.95;
    reps.iter()
        .reduce(|a, b| {
            // When no data has been received yet or the bandwidth is too low
            // then we will use the lowest bandwidth representation
            let a_under = a.bandwidth as f64 <= est_bw;
            let b_under = b.bandwidth as f64 <= est_bw;
            match (a_under, b_under) {
                // both under: take the higher bandwidth
                (true, true) => if a.bandwidth > b.bandwidth { a } else { b },
                // both over: take the lower bandwidth
                (false, false) => if a.bandwidth < b.bandwidth { a } else { b },
                (true, false) => a,
                (false, true) => b,
            }
        })
        .unwrap_or(&reps[0])
}

fn adjust_playback_rate(latency_diff: f64, k_p: f64) -> f64 {
    // Allow a small dead zone to avoid jitter
    let dead_zone = 0.01;
    if latency_diff.abs() < dead_zone {
        1.0
    } else {
        let adjustment = (latency_diff * k_p).clamp(-0.2, 1.5);
        (1.0 + adjustment).clamp(0.8, 2.5)
    }
}

fn replace_number_format(template: &str, segment_number: u64) -> String {
    // It's more efficient to make the regex static, so it's compiled only once.
    // Use `lazy_static` or `once_cell` for this.
    let re = &RE_NUMBER_FROM_URL;
    re.replace_all(template, |caps: &regex::Captures| {
        if let Some(width) = caps.get(1).or_else(|| caps.get(2)) {
            format!("{:0width$}", segment_number, width = width.as_str().parse::<usize>().unwrap_or(1))
        } else {
            segment_number.to_string()
        }
    }).to_string()
}

use crate::mpd::MpdMetadata;
use crate::segment::fetcher::{BandwidthEstimator, fetch_segment};
use crate::DashEvent;
use chrono::{DateTime, Utc};
use reqwest::Client;
use tracing::{debug, info};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use regex::Regex;

pub type SegmentCallback = Arc<dyn Fn(DashEvent) + Send + Sync>;

pub struct DashPlayer {
    mpd_url: String,
    client: Client,
    callback: SegmentCallback,
    mpd_data: Arc<RwLock<MpdMetadata>>,
    media_cache: Arc<Mutex<HashSet<String>>>,
    init_cache: Arc<Mutex<HashSet<String>>>,
    cancellation_token: Arc<CancellationToken>,
    target_latency: Arc<Mutex<Duration>>,
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
        })
    }

    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
        let mpd_data = self.mpd_data.read().await.clone();

        //info!("{mpd_data:?}");

        // We need to spawn one task per adaptation set
        for adaptation in &mpd_data.adaptation_sets {
            self.spawn_segment_fetcher(adaptation.clone(), mpd_data.availability_start_time, mpd_data.time_shift_buffer_depth.unwrap_or(f64::INFINITY)).await;
        }
        Ok(())
    }

    pub fn stop(&self) {
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

    pub async fn set_target_latency(&self, latency: f64) {
        let mut target_latency = self.target_latency.lock().await;
        *target_latency = Duration::from_secs_f64(latency);
    }

    pub async fn get_target_latency(&self) -> f64 {
        let target_latency = self.target_latency.lock().await;
        target_latency.as_secs_f64()
    }

    async fn spawn_segment_fetcher(&self, adaptation: crate::mpd::AdaptationSet, availability_start_time: DateTime<Utc>, time_shift_buffer: f64) {
        let base_url = self.mpd_url.rsplit_once('/').map(|(base, _)| base).unwrap_or("").to_string();
        let callback = self.callback.clone();
        let media_cache = self.media_cache.clone();
        let init_cache = self.init_cache.clone();
        let client = self.client.clone();
        let cancellation_token = self.cancellation_token.clone();
        let target_latency = self.target_latency.clone();

        tokio::spawn(async move {
            let mut estimator = BandwidthEstimator::new(0.25);
            let reps = &adaptation.representations;
            if reps.is_empty() {
                callback(DashEvent::Warning("No representations found".to_string()));
                return;
            }

            let mut segment_pointer: u64 = 0;

            loop {
                let loop_start = Instant::now(); 
                tokio::select! {
                    // Check for cancellation
                    _ = cancellation_token.cancelled() => {
                        callback(DashEvent::Info("Segment fetcher stopped.".to_string()));
                        break;
                    }
                    _ = async {
                        let est_bw = estimator.estimate();
                        // This selects the best representation based on the estimated bandwidth
                        let selected = select_representation(reps, est_bw);
                        let seg_duration = selected.segment_duration;
                        let seg_start_time = segment_pointer as f64 * seg_duration;
                        let uptime = Utc::now().signed_duration_since(availability_start_time).to_std().unwrap_or_default().as_secs_f64();

                        let target_latency_seconds = {
                            target_latency.lock().await.as_secs_f64()
                        };

                        let live_edge = uptime;
                        let earliest_allowed = (live_edge - time_shift_buffer).max(0.0);
                        //info!("Segment {}: {seg_start_time}, {uptime}, {earliest_allowed}", segment_pointer);
                        if seg_start_time < earliest_allowed {
                            debug!("Segment {} is not available anymore, skipping to next segment", segment_pointer);
                            segment_pointer += 1;
                            return;
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
                                info!("Waiting for {} ms until segment {} is available", wait_time_ms, segment_pointer);
                                sleep(wait_time).await;
                            }
                        }

                        let current_latency = {
                            let mut diff = uptime - seg_start_time;
                            if diff < 0.0 {
                                diff = 0.0;
                            }
                            if atc {
                                Duration::from_secs_f64(diff)
                            } else {
                                // If the segment is not complete, we need to adjust the latency
                                diff -= ato;
                                if diff < 0.0 {
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
                
                        /*
                        info!(
                            "Estimated bandwidth: {:.2} bps, Latency: {:.2} s, Playback rate: {:.2}",
                            estimator.estimate(),
                            current_latency.as_secs_f64(),
                            playback_rate
                        );
                        */

                        let segment_url = format!(
                            "{}/{}",
                            base_url,
                            replace_number_format(
                                &selected.media
                                    .replace("$Time$", &((segment_pointer as f64 * selected.timescale as f64).round() as u64).to_string())
                                    .replace("$RepresentationID$", &selected.id),
                                segment_pointer)
                        );

                        {
                            // Prevent downloading the same segment multiple times
                            let mut downloaded = media_cache.lock().await;
                            if downloaded.contains(&segment_url) {
                                segment_pointer += 1;
                                sleep(Duration::from_secs_f64(seg_duration / playback_rate)).await;
                                //info!("Segment {} already downloaded, skipping", segment_pointer);
                                return;
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
                                // info!("Downloading initialization segment: {}", init_url);
                                match fetch_segment(&client, &init_url).await {
                                    Ok((init_data, dur)) => {
                                        let length = init_data.len();
                                        callback(DashEvent::Segment {
                                            data: init_data,
                                            content_type: adaptation.content_type.clone(),
                                            representation_id: selected.id.clone(),
                                            segment_number: 0,
                                            duration: 0.0,
                                            url: init_url,
                                            playback_rate,
                                        });
                                        estimator.record(length, dur);
                                        inits.insert(init_key);
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

                        match fetch_segment(&client, &segment_url).await {
                            Ok((media_data, dur)) => {
                                // info!("Estimated Bandwidth was: {}, rate: {}", est_bw, playback_rate);
                                let length = media_data.len();
                                callback(DashEvent::Segment {
                                    data: media_data,
                                    content_type: adaptation.content_type.clone(),
                                    representation_id: selected.id.clone(),
                                    segment_number: segment_pointer,
                                    duration: seg_duration,
                                    url: segment_url.clone(),
                                    playback_rate,
                                });
                                estimator.record(length, dur);
                            }
                            Err(e) => {
                                callback(DashEvent::DownloadError {
                                    url: segment_url.clone(),
                                    reason: format!("{e}"),
                                });
                            }
                        }

                        segment_pointer += 1;

                        // length of one playback interval at the *current* rate
                        let target_interval = seg_duration / playback_rate;
                        // Time it took to complete this iteration (including the download)
                        let elapsed = loop_start.elapsed().as_secs_f64();

                        if elapsed < target_interval {
                            sleep(Duration::from_secs_f64(target_interval - elapsed)).await;
                        }
                    } => {}
                }
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
    let re = Regex::new(r"\$Number(?::%0(\d+)d|%0(\d+)d)?\$").unwrap();
    re.replace_all(template, |caps: &regex::Captures| {
        if let Some(width) = caps.get(1).or_else(|| caps.get(2)) {
            format!("{:0width$}", segment_number, width = width.as_str().parse::<usize>().unwrap_or(1))
        } else {
            segment_number.to_string()
        }
    }).to_string()
}

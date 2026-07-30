use crate::abr::{
    build_dash_abr, observation_from_segment_download,
    whole_request_throughput_sample_from_segment_download,
};
use crate::mpd::MpdMetadata;
use crate::segment::fetcher::{fetch_origin_time_signal, fetch_segment, NetTime, SegmentDownload};
use crate::{DashAbrStats, DashEvent, DashNetworkStats, DashPlayerTimingStats};
use abr_core::{AbrConfig, AbrController, AbrMode, AbrModeHandle, AbrSelectionDecision};
use chrono::{DateTime, Utc};
use once_cell::sync::Lazy;
use regex::Regex;
use reqwest::Client;
use std::collections::HashSet;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

pub type SegmentCallback = Arc<dyn Fn(DashEvent) + Send + Sync>;

fn bps_to_u64(value: f64) -> u64 {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }

    value.round().clamp(0.0, u64::MAX as f64) as u64
}

fn build_dash_abr_stats(
    decision: &AbrSelectionDecision,
    last_throughput_sample_bps: Option<f64>,
    last_whole_request_throughput_sample_bps: Option<f64>,
    requested_representation_bitrate_bps: u64,
) -> DashAbrStats {
    DashAbrStats {
        estimated_bandwidth_bps: bps_to_u64(decision.estimated_bandwidth_bps),
        bandwidth_budget_bps: bps_to_u64(decision.diagnostics.bandwidth_budget_bps),
        risk_adjusted_bandwidth_budget_bps: bps_to_u64(
            decision.diagnostics.risk_adjusted_bandwidth_budget_bps,
        ),
        last_throughput_sample_bps: last_throughput_sample_bps.map_or(0, bps_to_u64),
        last_whole_request_throughput_sample_bps: last_whole_request_throughput_sample_bps
            .map_or(0, bps_to_u64),
        requested_representation_bitrate_bps,
    }
}

fn observe_dash_network_stats(
    net_time: &mut NetTime,
    segment_download: &SegmentDownload,
) -> DashNetworkStats {
    if let Some(header_arrival_client_ms) = segment_download.header_arrival_client_ms {
        net_time.observe_serving_hop(
            header_arrival_client_ms as f64,
            segment_download.ttfb_s,
            segment_download.server_wait_ms,
            segment_download.serving_hop_now_ms,
        );
    }

    let estimated_one_way_latency_ms = segment_download
        .server_wait_ms
        .map(|_| net_time.one_way_cs_ms());
    let estimated_rtt_ms = estimated_one_way_latency_ms.map(|one_way| one_way * 2.0);
    let serving_hop_clock_offset_ms = if segment_download.serving_hop_now_ms.is_some()
        && segment_download.header_arrival_client_ms.is_some()
    {
        net_time.serving_hop_clock_offset_ms()
    } else {
        None
    };

    DashNetworkStats {
        ttfb_ms: segment_download.ttfb_s * 1000.0,
        server_wait_ms: segment_download.server_wait_ms,
        estimated_one_way_latency_ms,
        estimated_rtt_ms,
        serving_hop_clock_offset_ms,
        origin_clock_offset_ms: net_time.origin_clock_offset_ms(),
        origin_clock_source_id: net_time.origin_clock_source_id().map(ToOwned::to_owned),
    }
}

fn apply_clock_offset(now: DateTime<Utc>, offset_ms: Option<f64>) -> DateTime<Utc> {
    let offset_us = (offset_ms.unwrap_or(0.0) * 1000.0)
        .round()
        .clamp(i64::MIN as f64, i64::MAX as f64) as i64;

    now + chrono::Duration::microseconds(offset_us)
}

async fn aligned_utc_now(net_time: &Arc<Mutex<NetTime>>) -> DateTime<Utc> {
    let now = Utc::now();
    let now_client_ms = now.timestamp_millis() as f64;
    let origin_clock_offset_ms = {
        net_time
            .lock()
            .await
            .usable_origin_clock_offset_ms(now_client_ms)
    };
    apply_clock_offset(now, origin_clock_offset_ms)
}

async fn sync_origin_time_once(
    client: &Client,
    origin_time_signal_url: &str,
    net_time: &Arc<Mutex<NetTime>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let origin_time_signal = fetch_origin_time_signal(client, origin_time_signal_url).await?;
    let mut net_time = net_time.lock().await;
    net_time.observe_origin(
        origin_time_signal.header_arrival_client_ms as f64,
        origin_time_signal.ttfb_s,
        origin_time_signal.origin_now_ms,
        origin_time_signal.clock_source_id,
    );
    Ok(())
}

// This regex matches the $Number format in DASH segment URLs.
static RE_NUMBER_FROM_URL: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\$Number(?::%0(\d+)d|%0(\d+)d)?\$").unwrap());
const LIVE_EDGE_REANCHOR_THRESHOLD_SEGMENTS: u64 = 2;

#[derive(Debug, Default)]
pub struct DashCacheStats {
    hit_mem: std::sync::atomic::AtomicU64,
    hit_disk: std::sync::atomic::AtomicU64,
    hit: std::sync::atomic::AtomicU64,
    miss: std::sync::atomic::AtomicU64,
    inflight: std::sync::atomic::AtomicU64,
    bypass: std::sync::atomic::AtomicU64,
    unknown: std::sync::atomic::AtomicU64,
}

impl DashCacheStats {
    pub fn observe(&self, s: crate::segment::fetcher::CacheStatus) {
        use crate::segment::fetcher::CacheStatus::*;
        let c = match s {
            HitMem => &self.hit_mem,
            HitDisk => &self.hit_disk,
            Hit => &self.hit,
            Miss => &self.miss,
            Inflight => &self.inflight,
            Bypass => &self.bypass,
            Unknown => &self.unknown,
        };
        c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    #[allow(dead_code)]
    pub fn get_stats(&self) -> (u64, u64, u64, u64, u64, u64, u64) {
        (
            self.hit_mem.load(Ordering::Relaxed),
            self.hit_disk.load(Ordering::Relaxed),
            self.hit.load(Ordering::Relaxed),
            self.miss.load(Ordering::Relaxed),
            self.inflight.load(Ordering::Relaxed),
            self.bypass.load(Ordering::Relaxed),
            self.unknown.load(Ordering::Relaxed),
        )
    }
}

pub struct DashPlayer {
    mpd_url: String,
    origin_time_signal_url: String,
    client: Client,
    callback: SegmentCallback,
    mpd_data: Arc<RwLock<MpdMetadata>>,
    media_cache: Arc<Mutex<HashSet<String>>>,
    init_cache: Arc<Mutex<HashSet<String>>>,
    cancellation_token: Arc<CancellationToken>,
    target_latency: Arc<Mutex<Duration>>,
    fetching_enabled: Arc<AtomicBool>,
    cache_stats: Arc<DashCacheStats>,
    abr_mode: AbrModeHandle,
    net_time: Arc<Mutex<NetTime>>,
}

impl DashPlayer {
    pub async fn new(
        url: &str,
        callback: SegmentCallback,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let client = Client::new();
        let mpd_text = client.get(url).send().await?.text().await?;
        let mpd_data = crate::mpd::parser::parse_mpd(&mpd_text)?;
        let origin_time_signal_url = url
            .rsplit_once('/')
            .map(|(base, _)| format!("{base}/origin-time"))
            .unwrap_or_else(|| format!("{url}/origin-time"));

        Ok(Self {
            mpd_url: url.to_string(),
            origin_time_signal_url,
            client,
            callback,
            mpd_data: Arc::new(RwLock::new(mpd_data)),
            media_cache: Arc::new(Mutex::new(HashSet::new())),
            init_cache: Arc::new(Mutex::new(HashSet::new())),
            cancellation_token: Arc::new(CancellationToken::new()),
            target_latency: Arc::new(Mutex::new(Duration::from_secs_f64(3.0))),
            fetching_enabled: Arc::new(AtomicBool::new(true)),
            cache_stats: Arc::new(DashCacheStats::default()),
            abr_mode: AbrModeHandle::new(AbrMode::Advanced),
            net_time: Arc::new(Mutex::new(NetTime::new(0.25))),
        })
    }

    pub fn set_abr_mode_handle(&mut self, abr_mode: AbrModeHandle) {
        self.abr_mode = abr_mode;
    }

    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
        if let Err(err) =
            sync_origin_time_once(&self.client, &self.origin_time_signal_url, &self.net_time).await
        {
            (self.callback)(DashEvent::Warning(format!(
                "Initial origin time sync failed: {err}"
            )));
        }

        self.spawn_origin_time_sync();

        let mpd_data = self.mpd_data.read().await.clone();

        //info!("{mpd_data:?}");

        // We need to spawn one task per adaptation set
        for adaptation in &mpd_data.adaptation_sets {
            self.spawn_segment_fetcher(
                adaptation.clone(),
                mpd_data.availability_start_time,
                mpd_data.time_shift_buffer_depth.unwrap_or(f64::INFINITY),
                self.fetching_enabled.clone(),
            )
            .await;
        }
        Ok(())
    }

    fn spawn_origin_time_sync(&self) {
        let client = self.client.clone();
        let origin_time_signal_url = self.origin_time_signal_url.clone();
        let callback = self.callback.clone();
        let cancellation_token = self.cancellation_token.clone();
        let net_time = self.net_time.clone();

        tokio::spawn(async move {
            let mut consecutive_failures: u64 = 0;

            while !cancellation_token.is_cancelled() {
                if let Err(err) =
                    sync_origin_time_once(&client, &origin_time_signal_url, &net_time).await
                {
                    consecutive_failures = consecutive_failures.saturating_add(1);
                    if consecutive_failures == 1 || consecutive_failures.is_multiple_of(10) {
                        callback(DashEvent::Warning(format!(
                            "Origin time sync failed ({}): {err}",
                            consecutive_failures
                        )));
                    }
                } else {
                    consecutive_failures = 0;
                }

                tokio::select! {
                    _ = cancellation_token.cancelled() => break,
                    _ = sleep(Duration::from_millis(500)) => {}
                }
            }
        });
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

    pub fn get_cache_stats(&self) -> (u64, u64, u64, u64, u64, u64, u64) {
        self.cache_stats.get_stats()
    }

    async fn spawn_segment_fetcher(
        &self,
        adaptation: crate::mpd::AdaptationSet,
        availability_start_time: DateTime<Utc>,
        time_shift_buffer: f64,
        fetching_enabled: Arc<AtomicBool>,
    ) {
        let base_url = self
            .mpd_url
            .rsplit_once('/')
            .map(|(base, _)| base)
            .unwrap_or("")
            .to_string();
        let callback = self.callback.clone();
        let media_cache = self.media_cache.clone();
        let init_cache = self.init_cache.clone();
        let client = self.client.clone();
        let cancellation_token = self.cancellation_token.clone();
        let target_latency = self.target_latency.clone();
        let cache_stats = self.cache_stats.clone();
        let abr_mode = self.abr_mode.clone();
        let net_time = self.net_time.clone();

        tokio::spawn(async move {
            const MAX_IN_FLIGHT: usize = 10; // cap concurrent fetches
            let fetch_ahead_ms: u64 = 30; // How many ms our loop will fetch ahead of playback
            let abr_started_at = Instant::now();

            let reps = &adaptation.representations;
            if reps.is_empty() {
                callback(DashEvent::Warning("No representations found".to_string()));
                return;
            }

            let mut active_abr_mode = abr_mode.get();
            let abr = Arc::new(Mutex::new(build_dash_abr(reps, active_abr_mode)));

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

                        let abr_decision = {
                            let mut abr = abr.lock().await;
                            sync_abr_mode(&mut abr, &mut active_abr_mode, &abr_mode);
                            abr.decide_pending()
                        };
                        let selected_index = abr_decision
                            .selected_quality_ids
                            .first()
                            .map(|quality_id| quality_id.as_index())
                            .unwrap_or(0);
                        let selected = &reps[selected_index];
                        let requested_representation_bitrate_bps = selected.bandwidth;
                        let seg_duration = selected.segment_duration;
                        let now = aligned_utc_now(&net_time).await;
                        let uptime = now
                            .signed_duration_since(availability_start_time)
                            .to_std()
                            .unwrap_or_default()
                            .as_secs_f64();

                        let target_latency_seconds = {
                            target_latency.lock().await.as_secs_f64()
                        };

                        let live_edge = uptime;
                        let desired_segment_number =
                            live_edge_segment_number(live_edge, target_latency_seconds, seg_duration);
                        if desired_segment_number
                            > segment_number.saturating_add(LIVE_EDGE_REANCHOR_THRESHOLD_SEGMENTS)
                        {
                            segment_number = desired_segment_number;
                        }

                        let earliest_allowed = if live_edge > time_shift_buffer {
                            live_edge - time_shift_buffer
                        } else {
                            0.0
                        };
                        let seg_start_time = segment_number as f64 * seg_duration;
                        //info!("Segment {}: {seg_start_time}, {uptime}, {earliest_allowed}", segment_number);
                        if seg_start_time < earliest_allowed {
                            //debug!("Segment {} is not available anymore, skipping to next segment", segment_number);
                            segment_number =
                                segment_number_for_position(earliest_allowed, seg_duration)
                                    .max(segment_number.saturating_add(1));
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
                                segment_wallclock_time - chrono::Duration::from_std(Duration::from_secs_f64(-offset)).unwrap()
                            }
                        };

                        let now = aligned_utc_now(&net_time).await;
                        if now < available_at {
                            // Calculate how long to wait until the segment is available
                            let wait_time = available_at
                                .signed_duration_since(now)
                                .to_std()
                                .unwrap_or_default();
                            let wait_time_ms = wait_time.as_millis();
                            if wait_time_ms > 0 {
                                //debug!("Waiting for {} ms until segment {} is available", wait_time_ms, segment_number);
                                sleep(wait_time).await;
                            }
                        }

                        let latency_uptime = aligned_utc_now(&net_time)
                            .await
                            .signed_duration_since(availability_start_time)
                            .to_std()
                            .unwrap_or_default()
                            .as_secs_f64();
                        let current_latency = {
                            let mut diff = latency_uptime - seg_start_time;
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
                        let timing_stats = DashPlayerTimingStats {
                            current_latency_ms: current_latency.as_secs_f64() * 1000.0,
                            fetch_loop_lateness_ms: latency_diff.max(0.0) * 1000.0,
                            segment_number_vs_clock_delta: segment_number_delta(
                                segment_number,
                                desired_segment_number,
                            ),
                        };
                        let playback_headroom_s = latency_diff.max(0.0);
                        // Proportional gain tuned for small durations and aggressive latency correction
                        // Higher value for quicker catch-up, lower for smoother
                        let k_p = 1.2;
                        let playback_rate = adjust_playback_rate(latency_diff, k_p);

                        if !fetching_enabled.load(Ordering::Relaxed) {
                            let sn_clone = segment_number;
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
                                        let network_stats = {
                                            let mut net_time = net_time.lock().await;
                                            observe_dash_network_stats(&mut net_time, &segment_download)
                                        };
                                        let observation = observation_from_segment_download(
                                            &segment_download,
                                            &network_stats,
                                            0.0,
                                            0.0,
                                            abr_started_at.elapsed().as_millis() as u64,
                                        );
                                        let abr_stats = build_dash_abr_stats(
                                            &abr_decision,
                                            observation.throughput_sample_bps,
                                            Some(whole_request_throughput_sample_from_segment_download(
                                                &segment_download,
                                            )),
                                            requested_representation_bitrate_bps,
                                        );
                                        let init_data = segment_download.bytes;
                                        callback(DashEvent::Segment {
                                            data: init_data,
                                            content_type: content_type.clone(),
                                            representation_id: selected.id.clone(),
                                            segment_number: 0,
                                            duration: 0.0,
                                            url: init_url,
                                            playback_rate,
                                            network_stats,
                                            abr_stats,
                                            timing_stats,
                                        });
                                        inits.insert(init_key);
                                        abr.lock().await.observe(observation);
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

                        let abr_clone = abr.clone();
                        let cb = callback.clone();
                        let client_clone = client.clone();
                        let ct = content_type.clone();
                        let rep_id = selected.id.clone();
                        let seg_url = segment_url.clone();
                        let abr_decision = abr_decision.clone();
                        let media_cache = media_cache.clone();
                        let cache_stats = cache_stats.clone();
                        let net_time = net_time.clone();
                        let handle = tokio::spawn(async move {
                            match fetch_segment(&client_clone, &seg_url).await {
                                Ok(segment_download) => {
                                    let network_stats = {
                                        let mut net_time = net_time.lock().await;
                                        observe_dash_network_stats(&mut net_time, &segment_download)
                                    };
                                    let observation = observation_from_segment_download(
                                        &segment_download,
                                        &network_stats,
                                        seg_duration,
                                        playback_headroom_s,
                                        abr_started_at.elapsed().as_millis() as u64,
                                    );
                                    let abr_stats = build_dash_abr_stats(
                                        &abr_decision,
                                        observation.throughput_sample_bps,
                                        Some(whole_request_throughput_sample_from_segment_download(
                                            &segment_download,
                                        )),
                                        requested_representation_bitrate_bps,
                                    );
                                    let media_data = segment_download.bytes;
                                    // info!("Estimated Bandwidth was: {}, rate: {}", est_bw, playback_rate);
                                    cb(DashEvent::Segment {
                                        data: media_data,
                                        content_type: ct.clone(),
                                        representation_id: rep_id.clone(),
                                        segment_number,
                                        duration: seg_duration,
                                        url: seg_url.clone(),
                                        playback_rate,
                                        network_stats,
                                        abr_stats,
                                        timing_stats,
                                    });
                                    abr_clone.lock().await.observe(observation);
                                    // Also put the segment in the cache
                                    { media_cache.lock().await.insert(seg_url.clone()); };

                                    cache_stats.observe(segment_download.cache_status);
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

fn sync_abr_mode(abr: &mut AbrController, active_mode: &mut AbrMode, abr_mode: &AbrModeHandle) {
    let requested_mode = abr_mode.get();
    if requested_mode == *active_mode {
        return;
    }

    abr.update_config(AbrConfig::for_mode(requested_mode));
    *active_mode = requested_mode;
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

fn live_edge_segment_number(
    live_edge_s: f64,
    target_latency_s: f64,
    segment_duration_s: f64,
) -> u64 {
    segment_number_for_position(
        (live_edge_s - target_latency_s).max(0.0),
        segment_duration_s,
    )
}

fn segment_number_for_position(position_s: f64, segment_duration_s: f64) -> u64 {
    if !position_s.is_finite() || !segment_duration_s.is_finite() || segment_duration_s <= 0.0 {
        return 0;
    }

    (position_s / segment_duration_s)
        .floor()
        .max(0.0)
        .min(u64::MAX as f64) as u64
}

fn segment_number_delta(current: u64, clock_derived: u64) -> i64 {
    let delta = current as i128 - clock_derived as i128;
    delta.clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

fn replace_number_format(template: &str, segment_number: u64) -> String {
    // It's more efficient to make the regex static, so it's compiled only once.
    // Use `lazy_static` or `once_cell` for this.
    let re = &RE_NUMBER_FROM_URL;
    re.replace_all(template, |caps: &regex::Captures| {
        if let Some(width) = caps.get(1).or_else(|| caps.get(2)) {
            format!(
                "{:0width$}",
                segment_number,
                width = width.as_str().parse::<usize>().unwrap_or(1)
            )
        } else {
            segment_number.to_string()
        }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use crate::abr::build_dash_abr;
    use crate::mpd::Representation;
    use abr_core::{AbrMode, AbrModeHandle};
    use chrono::{Duration, Utc};

    use super::{apply_clock_offset, bps_to_u64, build_dash_abr_stats, sync_abr_mode};

    fn representation(id: &str, bandwidth: u64) -> Representation {
        Representation {
            id: id.to_string(),
            bandwidth,
            initialization: format!("init-{id}"),
            media: format!("seg-{id}-$Number$"),
            segment_duration: 1.0,
            timescale: 1,
            uses_segment_time: false,
            has_template: true,
            availability_time_offset: None,
            availability_time_complete: None,
            presentation_time_offset: None,
            segment_timeline: None,
        }
    }

    #[test]
    fn dash_simple_mode_matches_current_selection_behavior() {
        let reps = vec![
            representation("low", 4_000_000),
            representation("mid", 8_000_000),
            representation("high", 12_000_000),
        ];

        let mut abr = build_dash_abr(&reps, AbrMode::Simple);
        abr.observe_estimated_bandwidth_bps(10_000_000.0);

        let decision = abr.decide_pending();

        assert_eq!(decision.selected_quality_ids[0].as_index(), 1);
        assert_eq!(decision.allowed_qualities[0].id.as_index(), 1);
        assert_eq!(decision.allowed_qualities[1].id.as_index(), 0);
    }

    #[test]
    fn apply_clock_offset_shifts_time_by_origin_offset() {
        let now = Utc::now();

        assert_eq!(
            apply_clock_offset(now, Some(25.0)),
            now + Duration::milliseconds(25)
        );
        assert_eq!(
            apply_clock_offset(now, Some(-7.0)),
            now - Duration::milliseconds(7)
        );
    }

    #[test]
    fn sync_abr_mode_updates_existing_abr_config_from_shared_handle() {
        let reps = vec![
            representation("low", 4_000_000),
            representation("mid", 8_000_000),
            representation("high", 12_000_000),
        ];
        let handle = AbrModeHandle::new(AbrMode::Simple);
        let mut active_mode = handle.get();
        let mut abr = build_dash_abr(&reps, active_mode);

        handle.set(AbrMode::Advanced);
        sync_abr_mode(&mut abr, &mut active_mode, &handle);

        assert_eq!(active_mode, AbrMode::Advanced);
        assert_eq!(abr.decide(false).diagnostics.mode, AbrMode::Advanced);
    }

    #[test]
    fn build_dash_abr_stats_uses_existing_selection_decision_diagnostics() {
        let reps = vec![
            representation("low", 4_000_000),
            representation("mid", 8_000_000),
            representation("high", 12_000_000),
        ];
        let mut abr = build_dash_abr(&reps, AbrMode::Simple);
        abr.observe_estimated_bandwidth_bps(10_000_000.0);

        let decision = abr.decide_pending();
        let stats = build_dash_abr_stats(
            &decision,
            Some(6_500_000.25),
            Some(4_200_000.75),
            reps[1].bandwidth,
        );

        assert_eq!(
            stats.estimated_bandwidth_bps,
            bps_to_u64(decision.estimated_bandwidth_bps)
        );
        assert_eq!(
            stats.bandwidth_budget_bps,
            bps_to_u64(decision.diagnostics.bandwidth_budget_bps)
        );
        assert_eq!(
            stats.risk_adjusted_bandwidth_budget_bps,
            bps_to_u64(decision.diagnostics.risk_adjusted_bandwidth_budget_bps)
        );
        assert_eq!(stats.last_throughput_sample_bps, 6_500_000);
        assert_eq!(stats.last_whole_request_throughput_sample_bps, 4_200_001);
        assert_eq!(
            stats.requested_representation_bitrate_bps,
            reps[1].bandwidth
        );
    }
}

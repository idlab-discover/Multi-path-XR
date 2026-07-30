// handlers/scheduler.rs

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::{Query, State};
use axum::Json;
use bytes::Bytes;
use memmap2::MmapOptions;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tracing::{debug, error, info, instrument, warn};
use uuid::Uuid;

use crate::generators::GeneratorName;
use crate::handlers::datasets::get_pc_files;
use crate::processing::ProcessingPipeline;
use crate::services::stream_manager::StreamManager;
use crate::timing::{
    frame_offset_duration, scheduler_lateness_gauge, sleep_until_and_record_lateness,
};
use crate::types::{AppState, EgressProtocolType, StreamPayloadFormat};

use shared_utils::types::SpatialFrameData;

#[derive(Clone)]
enum DatasetFrameBacking {
    MemoryMapped(Bytes),
    FileReadFallback,
}

#[derive(Clone)]
struct DatasetFrameSource {
    path: PathBuf,
    backing: DatasetFrameBacking,
}

impl DatasetFrameSource {
    fn open(path: PathBuf) -> Self {
        let backing = match File::open(&path) {
            Ok(file) => match file.metadata() {
                Ok(metadata) if metadata.len() == 0 => {
                    DatasetFrameBacking::MemoryMapped(Bytes::new())
                }
                Ok(_) => {
                    // SAFETY: Dataset files are treated as immutable for the lifetime of an
                    // experiment. The read-only mapping owns its mapping independently of `file`.
                    match unsafe { MmapOptions::new().map(&file) } {
                        Ok(mapping) => {
                            DatasetFrameBacking::MemoryMapped(Bytes::from_owner(mapping))
                        }
                        Err(error) => {
                            warn!(
                                "Failed to memory-map dataset frame {}, using file reads: {}",
                                path.display(),
                                error
                            );
                            DatasetFrameBacking::FileReadFallback
                        }
                    }
                }
                Err(error) => {
                    warn!(
                        "Failed to inspect dataset frame {}, using file reads: {}",
                        path.display(),
                        error
                    );
                    DatasetFrameBacking::FileReadFallback
                }
            },
            Err(error) => {
                warn!(
                    "Failed to open dataset frame {} for memory mapping, using file reads: {}",
                    path.display(),
                    error
                );
                DatasetFrameBacking::FileReadFallback
            }
        };

        Self { path, backing }
    }

    fn load(&self) -> std::io::Result<Bytes> {
        match &self.backing {
            DatasetFrameBacking::MemoryMapped(bytes) => Ok(bytes.clone()),
            DatasetFrameBacking::FileReadFallback => std::fs::read(&self.path).map(Bytes::from),
        }
    }

    fn mapped_len(&self) -> Option<usize> {
        match &self.backing {
            DatasetFrameBacking::MemoryMapped(bytes) => Some(bytes.len()),
            DatasetFrameBacking::FileReadFallback => None,
        }
    }
}

struct DatasetFrameSources {
    frames: Vec<DatasetFrameSource>,
}

impl DatasetFrameSources {
    fn open(paths: impl IntoIterator<Item = PathBuf>) -> Self {
        let frames: Vec<_> = paths.into_iter().map(DatasetFrameSource::open).collect();
        let mapped_frames = frames
            .iter()
            .filter(|frame| frame.mapped_len().is_some())
            .count();
        let mapped_bytes = frames
            .iter()
            .filter_map(DatasetFrameSource::mapped_len)
            .sum::<usize>();

        info!(
            "Memory-mapped {}/{} dataset frames ({:.2} GiB virtual address space); mapped pages remain reclaimable by the OS",
            mapped_frames,
            frames.len(),
            mapped_bytes as f64 / 1024_f64.powi(3)
        );

        Self { frames }
    }

    fn len(&self) -> usize {
        self.frames.len()
    }

    fn get(&self, index: usize) -> Option<&DatasetFrameSource> {
        self.frames.get(index)
    }
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct JobRequest {
    pub dataset: Option<String>,
    pub ply_folder: Option<String>,
    pub fps: u32,
    pub presentation_time_offset: u64,
    pub should_loop: bool,
    pub priority: Option<u8>,
    pub egress_protocol: EgressProtocolType,
    pub stream_id: Option<String>,
    pub input_format: Option<StreamPayloadFormat>,
    // Additional fields for generator-based jobs can be added here
    pub generator_name: Option<GeneratorName>,
}

#[derive(Serialize, Debug)]
pub struct JobResponse {
    pub id: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<JobRequest>,
}

#[instrument(skip_all)]
pub async fn start_transmission_job(
    Query(params): Query<JobRequest>,
    State(app_state): State<AppState>,
) -> Json<JobResponse> {
    let params_clone = params.clone();
    // Validate parameters
    if params_clone.dataset.is_none() && params_clone.generator_name.is_none() {
        return Json(JobResponse {
            id: "".to_string(),
            message: "Either dataset or generator_name must be provided".to_string(),
            params: None,
        });
    }

    let job_id = Uuid::new_v4().to_string();
    let (tx, rx) = oneshot::channel();

    app_state
        .active_jobs
        .write()
        .await
        .insert(job_id.clone(), tx);

    let stream_id = params_clone
        .stream_id
        .clone()
        .unwrap_or_else(|| format!("job_{job_id}"));

    // Update stream settings based on job parameters
    let mut settings = app_state.stream_manager.get_stream_settings(&stream_id);
    settings.priority = params_clone.priority.unwrap_or(0);
    settings.egress_protocols = vec![params_clone.egress_protocol.clone()];
    settings.presentation_time_offset = Some(params_clone.presentation_time_offset);
    if let Some(input_format) = params_clone.input_format {
        settings.input_format = input_format;
    }
    app_state.stream_manager.update_stream_settings(settings);

    let processing_pipeline = app_state.processing_pipeline.clone();
    let stream_manager = app_state.stream_manager.clone();

    if let Some(dataset) = params_clone.dataset.clone() {
        // Dataset-based job
        let ply_folder = params_clone.ply_folder.clone().unwrap_or_default();

        let job_id_clone = job_id.clone();
        let thread_name = format!("JOB_D_{job_id_clone}");
        thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                run_dataset_job(
                    job_id_clone,
                    stream_id.clone(),
                    dataset,
                    ply_folder,
                    params_clone,
                    processing_pipeline,
                    stream_manager,
                    rx,
                )
            })
            .expect("Failed to spawn dataset job thread");
    } else {
        let job_id_clone = job_id.clone();
        let thread_name = format!("JOB_G_{job_id_clone}");
        thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                run_generator_job(
                    job_id_clone,
                    stream_id.clone(),
                    params_clone,
                    processing_pipeline,
                    stream_manager,
                    rx,
                )
            })
            .expect("Failed to spawn generator job thread");
    }

    let message = format!("Job started with ID {job_id}");
    info!("{}", message);

    Json(JobResponse {
        id: job_id.clone(),
        message,
        params: Some(params),
    })
}

#[allow(clippy::too_many_arguments)]
#[instrument(skip_all)]
fn run_dataset_job(
    job_id: String,
    stream_id: String,
    dataset: String,
    pc_folder: String,
    params: JobRequest,
    processing_pipeline: Arc<ProcessingPipeline>,
    stream_manager: Arc<StreamManager>,
    mut stop_signal: oneshot::Receiver<()>,
) {
    info!(
        "Starting dataset job with ID {} and stream ID {}",
        job_id, stream_id
    );

    let fps = params.fps;
    let presentation_time_offset = params.presentation_time_offset;
    let should_loop = params.should_loop;
    let scheduler_lateness = scheduler_lateness_gauge(&format!("dataset:{stream_id}"));

    // Split the folder on '/', take the last one, split on '_', get the first part
    let extension = pc_folder
        .split('/')
        .next_back()
        .unwrap_or(&pc_folder)
        .split('_')
        .next()
        .unwrap_or(&pc_folder)
        .to_string()
        .to_lowercase();

    let pc_files = get_pc_files(&dataset, &pc_folder, &extension);

    if pc_files.is_empty() {
        warn!(
            "No PC files found in dataset: {}, pc_folder: {}",
            dataset, pc_folder
        );
        return;
    }

    info!(
        "Dataset job {} started with {} PC files",
        job_id,
        pc_files.len()
    );

    let dataset_root = Path::new("../Datasets").join(&dataset).join(&pc_folder);
    let frame_sources =
        DatasetFrameSources::open(pc_files.iter().map(|file| dataset_root.join(file)));
    let start_time = Instant::now() + Duration::from_micros(presentation_time_offset);
    let frame_index = Arc::new(Mutex::new(0));

    loop {
        let index = {
            let mut idx = frame_index.lock().unwrap();
            let current_index = *idx;
            *idx += 1;
            current_index
        };

        // Check for stop signal
        if stop_signal.try_recv().is_ok() {
            info!("Dataset job {} stopped", job_id);
            break;
        }

        // Handle looping
        if !should_loop && index >= frame_sources.len() {
            info!("Dataset job {} completed", job_id);
            break;
        }

        // Get the current mapped frame by modulus if looping is enabled.
        let frame_source = frame_sources
            .get(index % frame_sources.len())
            .expect("dataset frame source must exist")
            .clone();

        let emit_time = start_time + frame_offset_duration(index as u64, fps);
        if !sleep_until_and_record_lateness(emit_time, &scheduler_lateness).is_zero() {
            // Scheduler is running behind, skip frame
            warn!(
                "Scheduler is running behind ({} ms), skipping frame {}",
                Instant::now().duration_since(emit_time).as_millis(),
                index
            );
            continue;
        }

        // Load and process the frame
        let thread_pool = processing_pipeline.thread_pool.clone();
        let processing_pipeline_clone = processing_pipeline.clone();
        let stream_manager_clone = stream_manager.clone();
        let stream_id_clone = stream_id.clone();
        thread_pool.spawn(move || {
            load_and_process_frame(
                frame_source,
                processing_pipeline_clone,
                stream_manager_clone,
                stream_id_clone,
            );
        });
    }
}

/// Loads or borrows a mapped dataset frame and pushes it to the decoder.
#[instrument(skip_all, fields(stream_id = %stream_id, filepath = %frame_source.path.display()))]
fn load_and_process_frame(
    frame_source: DatasetFrameSource,
    processing_pipeline: Arc<ProcessingPipeline>,
    stream_manager: Arc<StreamManager>,
    stream_id: String,
) {
    let raw_data = match frame_source.load() {
        Ok(data) => data,
        Err(e) => {
            error!(
                "Failed to read file {}: {:?}",
                frame_source.path.display(),
                e
            );
            return;
        }
    };

    // Push the frame to the decoder
    processing_pipeline.push_bytes_to_decoder(raw_data, stream_manager, stream_id);
}

#[instrument(skip_all)]
fn run_generator_job(
    job_id: String,
    stream_id: String,
    params: JobRequest,
    processing_pipeline: Arc<ProcessingPipeline>,
    stream_manager: Arc<StreamManager>,
    mut stop_signal: oneshot::Receiver<()>,
) {
    info!("Starting generator job with ID {}", job_id);

    // Placeholder implementation
    // You can replace this with actual generator logic
    let fps = params.fps;
    let presentation_time_offset = params.presentation_time_offset;
    let scheduler_lateness = scheduler_lateness_gauge(&format!("generator:{stream_id}"));

    let start_time = Instant::now() + Duration::from_micros(presentation_time_offset);
    let mut frame_index = 0;

    loop {
        // Check for stop signal
        if stop_signal.try_recv().is_ok() {
            info!("Generator job {} stopped", job_id);
            break;
        }

        let emit_time = start_time + frame_offset_duration(frame_index as u64, fps);
        if !sleep_until_and_record_lateness(emit_time, &scheduler_lateness).is_zero() {
            // Scheduler is running behind, skip frame
            warn!(
                "Scheduler is running behind ({} ms), skipping frame {}",
                Instant::now().duration_since(emit_time).as_millis(),
                frame_index
            );
            frame_index += 1;
            continue;
        }

        // Generate the spatial frame (placeholder)
        let spatial_frame = match params.generator_name {
            Some(GeneratorName::Cube) => crate::generators::generate_shaded_cube_points_frame(
                46,
                15.0,
                [1.0, 1.0, 1.0],
                45.0,
            ),
            _ => crate::generators::generate_basic_points_frame(),
        };

        // If there are no primitives, skip the frame
        if spatial_frame.is_empty() {
            debug!(
                "Empty spatial frame generated, skipping frame {}",
                frame_index
            );
            frame_index += 1;
            continue;
        }

        // Set the presentation time
        let since_the_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards");
        let current_time = since_the_epoch.as_micros() as u64;

        let spatial_frame = SpatialFrameData {
            presentation_time: current_time + presentation_time_offset,
            ..spatial_frame
        };

        // Process the frame
        let processing_pipeline_clone = processing_pipeline.clone();
        let stream_manager_clone = stream_manager.clone();
        let stream_id_clone = stream_id.clone();

        let thread_pool = processing_pipeline.thread_pool.clone();
        thread_pool.spawn(move || {
            processing_pipeline_clone.process_frame(
                spatial_frame,
                stream_manager_clone,
                stream_id_clone,
            );
        });

        frame_index += 1;
    }
}

#[instrument(skip_all)]
pub async fn stop_transmission_job(
    Query(params): Query<StopJobRequest>,
    State(app_state): State<AppState>,
) -> Json<JobResponse> {
    let job_id = params.job_id.clone();
    if let Some(tx) = app_state.active_jobs.write().await.remove(&job_id) {
        let _ = tx.send(());
        Json(JobResponse {
            id: job_id,
            message: "Job stopped".to_string(),
            params: None,
        })
    } else {
        Json(JobResponse {
            id: job_id,
            message: "Job not found".to_string(),
            params: None,
        })
    }
}

#[instrument(skip_all)]
pub async fn stop_all_jobs(State(app_state): State<AppState>) -> Json<JobResponse> {
    let jobs = app_state
        .active_jobs
        .write()
        .await
        .drain()
        .collect::<Vec<_>>();
    for (job_id, tx) in jobs {
        let _ = tx.send(());
        info!("Stopped job {}", job_id);
    }

    Json(JobResponse {
        id: "".to_string(),
        message: "All jobs stopped".to_string(),
        params: None,
    })
}

#[derive(Deserialize, Debug)]
pub struct StopJobRequest {
    pub job_id: String,
}

#[cfg(test)]
mod tests {
    use super::DatasetFrameSource;
    use std::fs;
    use std::io::Write;

    #[test]
    fn mapped_dataset_frame_reuses_the_same_bytes_without_copying() {
        let path = std::env::temp_dir().join(format!(
            "pc-server-dataset-frame-{}.bin",
            uuid::Uuid::new_v4()
        ));
        let expected = b"reusable dataset frame";
        {
            let mut file = fs::File::create(&path).unwrap();
            file.write_all(expected).unwrap();
        }

        let source = DatasetFrameSource::open(path.clone());
        assert_eq!(source.mapped_len(), Some(expected.len()));

        let first = source.load().unwrap();
        let second = source.load().unwrap();
        assert_eq!(first.as_ref(), expected);
        assert_eq!(second.as_ref(), expected);
        assert_eq!(first.as_ptr(), second.as_ptr());

        drop(first);
        drop(second);
        drop(source);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn empty_dataset_frame_is_supported_without_mapping_failure() {
        let path = std::env::temp_dir().join(format!(
            "pc-server-empty-dataset-frame-{}.bin",
            uuid::Uuid::new_v4()
        ));
        fs::File::create(&path).unwrap();

        let source = DatasetFrameSource::open(path.clone());
        assert_eq!(source.mapped_len(), Some(0));
        assert!(source.load().unwrap().is_empty());

        drop(source);
        fs::remove_file(path).unwrap();
    }
}

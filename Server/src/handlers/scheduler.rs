// handlers/scheduler.rs

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::{Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tracing::{debug, error, info, instrument, warn};
use uuid::Uuid;

use crate::generators::GeneratorName;
use crate::handlers::datasets::get_pc_files;
use crate::processing::ProcessingPipeline;
use crate::services::stream_manager::StreamManager;
use crate::types::{AppState, EgressProtocolType};

use shared_utils::types::PointCloudData;

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

    app_state.active_jobs.write().await.insert(job_id.clone(), tx);

    let stream_id = params_clone
        .stream_id
        .clone()
        .unwrap_or_else(|| format!("job_{}", job_id));

    // Update stream settings based on job parameters
    let mut settings = app_state.stream_manager.get_stream_settings(&stream_id);
    settings.priority = params_clone.priority.unwrap_or(0);
    settings.egress_protocols = vec![params_clone.egress_protocol.clone()];
    settings.presentation_time_offset = Some(params_clone.presentation_time_offset);
    app_state.stream_manager.update_stream_settings(settings);

    let processing_pipeline = app_state.processing_pipeline.clone();
    let stream_manager = app_state.stream_manager.clone();

    if let Some(dataset) = params_clone.dataset.clone() {
        // Dataset-based job
        let ply_folder = params_clone.ply_folder.clone().unwrap_or_default();

        let job_id_clone = job_id.clone();
        thread::spawn(move || {
            run_dataset_job(
                job_id_clone,
                stream_id.clone(),
                dataset,
                ply_folder,
                params_clone,
                processing_pipeline,
                stream_manager,
                rx
            )
        });
    } else {
        let job_id_clone = job_id.clone();
        thread::spawn(move || {
            run_generator_job(
                job_id_clone,
                stream_id.clone(),
                params_clone,
                processing_pipeline,
                stream_manager,
                rx,
            )
        }); 
    }

    Json(JobResponse {
        id: job_id.clone(),
        message: format!("Job started with ID {}", job_id),
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
    info!("Starting dataset job with ID {}", job_id);

    let fps = params.fps;
    let interval = 1_000_000 / fps as u64; // In microseconds
    let presentation_time_offset = params.presentation_time_offset;
    let should_loop = params.should_loop;

    // Split the folder on '/', take the last one, split on '_', get the first part
    let extension = pc_folder
        .split('/')
        .last()
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

    info!("Dataset job {} started with {} PC files", job_id, pc_files.len());

    let start_time = Instant::now() + Duration::from_millis(presentation_time_offset);
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
        if !should_loop && index >= pc_files.len() {
            info!("Dataset job {} completed", job_id);
            break;
        }

        // Get the current file by modulus if looping is enabled
        let file = &pc_files[index % pc_files.len()];
        let filepath = format!("../Datasets/{}/{}/{}", dataset, pc_folder, file);

        // Calculate the emit time
        let emit_time = start_time + Duration::from_micros(interval * index as u64);
        let now = Instant::now();
        if emit_time > now {
            thread::sleep(emit_time - now);
        } else {
            // Scheduler is running behind, skip frame
            warn!(
                "Scheduler is running behind ({} ms), skipping frame {}",
                (now - emit_time).as_millis(),
                index
            );
            continue;
        }

        // Load and process the frame
        let thread_pool = processing_pipeline.thread_pool.clone();
        let filepath_clone = filepath.clone();
        let processing_pipeline_clone = processing_pipeline.clone();
        let stream_manager_clone = stream_manager.clone();
        let stream_id_clone = stream_id.clone();
        thread_pool.spawn(move || {
            load_and_process_frame(
                filepath_clone,
                processing_pipeline_clone,
                stream_manager_clone,
                stream_id_clone,
            );
        });
    }
}

/// Loads a PLY file and processes it by pushing the frame to the decoder.
#[instrument(skip_all, fields(stream_id = %stream_id, filepath = %filepath))]
fn load_and_process_frame(
    filepath: String,
    processing_pipeline: Arc<ProcessingPipeline>,
    stream_manager: Arc<StreamManager>,
    stream_id: String,
) {
    // Load the PLY file
    let raw_data = match std::fs::read(&filepath) {
        Ok(data) => data,
        Err(e) => {
            error!("Failed to read file {}: {:?}", filepath, e);
            return;
        }
    };

    // Push the frame to the decoder
    processing_pipeline.push_to_decoder(raw_data, stream_manager, stream_id);
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
    let interval = 1_000_000 / fps as u64; // In microseconds
    let presentation_time_offset = params.presentation_time_offset;

    let start_time = Instant::now() + Duration::from_millis(presentation_time_offset);
    let mut frame_index = 0;

    loop {
        // Check for stop signal
        if stop_signal.try_recv().is_ok() {
            info!("Generator job {} stopped", job_id);
            break;
        }

        // Calculate the emit time
        let emit_time = start_time + Duration::from_micros(interval * frame_index as u64);
        let now = Instant::now();
        if emit_time > now {
            thread::sleep(emit_time - now);
        } else {
            // Scheduler is running behind, skip frame
            warn!(
                "Scheduler is running behind ({} ms), skipping frame {}",
                (now - emit_time).as_millis(),
                frame_index
            );
            frame_index += 1;
            continue;
        }

        // Generate the point cloud (placeholder)
        let point_cloud = match params.generator_name {
            Some(GeneratorName::Cube) => {
                crate::generators::generate_shaded_cube_point_cloud(
                    46,
                    15.0,
                    [1.0, 1.0, 1.0],
                    45.0
                )
            },
            _ => crate::generators::generate_basic_point_cloud(),
            
        };

        // If there are no points, skip the frame
        if point_cloud.points.is_empty() {
            debug!("Empty point cloud generated, skipping frame {}", frame_index);
            frame_index += 1;
            continue;
        }

        // Set the presentation time
        let since_the_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards");
        let current_time = since_the_epoch.as_micros() as u64;

        let point_cloud = PointCloudData {
            presentation_time: current_time + presentation_time_offset,
            ..point_cloud
        };

        // Process the frame
        let processing_pipeline_clone = processing_pipeline.clone();
        let stream_manager_clone = stream_manager.clone();
        let stream_id_clone = stream_id.clone();

        let thread_pool = processing_pipeline.thread_pool.clone();
        thread_pool.spawn(move || {
            processing_pipeline_clone.process_frame(point_cloud, stream_manager_clone, stream_id_clone);
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
            params: None
        })
    } else {
        Json(JobResponse {
            id: job_id,
            message: "Job not found".to_string(),
            params: None
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

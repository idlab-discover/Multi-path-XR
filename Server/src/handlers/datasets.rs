use axum::extract::Query;
use axum::Json;
use serde::{Deserialize, Serialize};
use tracing::instrument;
use std::fs;
use std::path::Path;

#[derive(Serialize, Debug)]
pub struct Dataset {
    name: String,
    ply_folders: Vec<String>,
    dra_folders: Vec<String>,
}

#[derive(Serialize, Debug)]
pub struct DatasetList {
    datasets: Vec<Dataset>,
}

#[derive(Serialize, Debug)]
pub struct PcFileList {
    files: Vec<String>,
}

#[derive(Deserialize, Debug)]
pub struct PcFileQuery {
    dataset: String,
    pc_folder: String,
}


#[instrument(skip_all)]
pub async fn list_datasets() -> Json<DatasetList> {
    let datasets_path = Path::new("../Datasets");
    let mut datasets = Vec::new();

    // Read the ../Datasets directory
    if let Ok(entries) = fs::read_dir(datasets_path) {
        for entry in entries.flatten() {
            let path = entry.path();
            // Check if it's a directory
            if path.is_dir() {
                if let Some(folder_name) = path.file_name().and_then(|name| name.to_str()) {
                    // For each dataset folder, look for Ply_* subfolders
                    let mut ply_folders = Vec::new();
                    let mut dra_folders = Vec::new();
                    if let Ok(sub_entries) = fs::read_dir(&path) {
                        for sub_entry in sub_entries.flatten() {
                            let sub_path = sub_entry.path();
                            if sub_path.is_dir() {
                                if let Some(sub_folder_name) = sub_path.file_name().and_then(|name| name.to_str()) {
                                    if sub_folder_name.starts_with("Ply_") {
                                        ply_folders.push(sub_folder_name.to_string());
                                    } else if sub_folder_name.starts_with("Dra_") {
                                        dra_folders.push(sub_folder_name.to_string());
                                    }
                                }
                            }
                        }
                    }
                    datasets.push(Dataset {
                        name: folder_name.to_string(),
                        ply_folders,
                        dra_folders,
                    });
                }
            }
        }
    }

    Json(DatasetList { datasets })
}

// Helper function to list all .ply files in a given dataset and ply_folder

#[instrument(skip_all)]
pub fn get_pc_files(dataset: &str, pc_folder: &str, file_extension: &str) -> Vec<String> {
    let pc_folder_path = Path::new("../Datasets").join(dataset).join(pc_folder);
    let mut pc_files = Vec::new();

    // Check if the folder exists
    if !pc_folder_path.exists() {
        return pc_files;
    }

    if let Ok(entries) = fs::read_dir(&pc_folder_path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().unwrap_or_default() == file_extension {
                if let Some(filename) = path.file_name().and_then(|name| name.to_str()) {
                    pc_files.push(filename.to_string());
                }
            }
        }
    }

    // Sort the files by name
    pc_files.sort();
    pc_files
}

// Function to list all .ply files via an API endpoint
#[instrument(skip_all)]
pub async fn list_ply_files(Query(params): Query<PcFileQuery>) -> Json<PcFileList> {
    let pc_files = get_pc_files(&params.dataset, &params.pc_folder, "ply");
    Json(PcFileList { files: pc_files })
}

// Function to list all .dra files via an API endpoint
#[instrument(skip_all)]
pub async fn list_dra_files(Query(params): Query<PcFileQuery>) -> Json<PcFileList> {
    let pc_files = get_pc_files(&params.dataset, &params.pc_folder, "dra");
    Json(PcFileList { files: pc_files })
}
use rand::{distributions::WeightedIndex, prelude::Distribution, seq::SliceRandom, Rng};
use shared_utils::types::Point3D;
use tracing::{instrument, debug};


#[instrument(skip_all)]
#[allow(dead_code)]
pub fn random_sampling<T: Clone>(data: &[T], sample_rate: f64) -> Vec<T> {
    let mut rng = rand::thread_rng();
    data.iter()
        .filter(|_| rng.gen_bool(sample_rate))
        .cloned()
        .collect()
}

/// Efficient random sampling to select exactly `target_count` elements from `data`
/// using a reservoir sampling-inspired algorithm.
#[instrument(skip_all)]
pub fn exact_random_sampling<T: Clone>(data: &[T], target_count: usize) -> Vec<T> {
    assert!(target_count <= data.len(), "Target count cannot exceed the number of input points");
    debug!("Performing exact random sampling with target count: {}", target_count);

    let mut rng = rand::thread_rng();
    let mut indices: Vec<usize> = Vec::with_capacity(target_count);

    let mut n = target_count; // Remaining slots to fill
    let mut data_len = data.len(); // Remaining elements in the input data

    for (index, _) in data.iter().enumerate() {
        let p: f64 = rng.gen(); // Generate a random number in the range [0, 1)
        if (data_len as f64 * p) <= n as f64 {
            indices.push(index);
            n -= 1;
            if n == 0 {
                break; // Stop early if the required sample size is reached
            }
        }
        data_len -= 1;
    }

    // Collect sampled elements using the selected indices
    indices.into_iter().map(|i| data[i].clone()).collect()
}

/// Biased random sampling to select `target_count` elements from `data`
/// based on their proximity to specified regions of interest (ROIs).
/// The closer a point is to the ROIs, the higher its chance of being selected.
/// The `roi_weight` parameter controls the influence of the ROIs on the sampling.
/// The `radius` parameter defines the distance within which points are considered
/// to be influenced by the ROIs.
#[instrument(skip_all)]
#[allow(dead_code)]
pub fn biased_exact_random_sampling(
    data: &[Point3D],
    target_count: usize,
    roi_centers: &[Point3D],
    radius: f32,
    roi_weight: f32,
) -> Vec<Point3D> {
    assert!(
        target_count <= data.len(),
        "Target count cannot exceed the number of input points"
    );

    // Precompute squared radius for efficiency
    let radius_sq = radius * radius;

    // Assign weights based on distance to ROI centers
    let weights: Vec<f64> = data
        .iter()
        .map(|point| {
            let mut influence = 0.0;

            for center in roi_centers {
                let dx = point.x - center.x;
                let dy = point.y - center.y;
                let dz = point.z - center.z;
                let dist_sq = dx * dx + dy * dy + dz * dz;

                if dist_sq < radius_sq {
                    // The closer the point, the higher the influence
                    let factor = 1.0 - (dist_sq / radius_sq);
                    influence += factor as f64;
                }
            }

            // Base weight is 1.0, influence adds roi_weight proportionally
            1.0 + influence * roi_weight as f64
        })
        .collect();

    // Create a weighted index for selection
    let mut rng = rand::thread_rng();
    let dist = WeightedIndex::new(&weights).expect("Invalid weights");

    let mut selected = std::collections::HashSet::with_capacity(target_count);
    while selected.len() < target_count {
        let idx = dist.sample(&mut rng);
        selected.insert(idx);
    }

    selected.into_iter().map(|i| data[i].clone()).collect()
}


// Split `pc` into disjoint sub‑clouds whose sizes follow `percentages`.
///
/// * `percentages`: each value 0‑100; their sum **must not exceed 100**  
/// * Returns `Vec<PointCloudData>` in the same order as `percentages`
///
/// Example: with 100 k points and `[50, 30, 10]` you get three clouds
/// containing 50 k, 30 k and 10 k points; 10 k points remain unused.
pub fn partition_by_percentages<T: Clone>(
    data: &[T],
    percentages: &[u8],
) -> Result<Vec<Vec<T>>, &'static str> {
    if percentages.iter().any(|&p| p > 100) {
        return Err("Each percentage must be in 0‑100");
    }
    let sum: u32 = percentages.iter().map(|&p| p as u32).sum();
    if sum > 100 {
        return Err("Sum of percentages must not exceed 100");
    }

    let n_items = data.len();
    if n_items == 0 {
        return Ok(Vec::new());
    }

    // ---- 1) shuffle point indices ----------------------------------------
    let mut indices: Vec<usize> = (0..n_items).collect();
    indices.shuffle(&mut rand::thread_rng());

    // ---- 2) carve the shuffled list according to the requested shares ----
    let mut offset = 0usize;
    let mut buckets = Vec::with_capacity(percentages.len());

    for &pct in percentages {
        let take = (pct as usize * n_items) / 100;
        let slice_end = offset + take;

        let mut bucket = Vec::with_capacity(take);

        for &idx in &indices[offset..slice_end] {
            bucket.push(data[idx].clone());
        }

        buckets.push(bucket);
        offset = slice_end;
    }

    Ok(buckets)
}

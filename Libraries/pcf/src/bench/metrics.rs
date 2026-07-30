use spatial_utils::point::Point3D;

pub struct Fidelity {
    pub rmse: Option<f64>,
    pub psnr_y: Option<f64>,
}

#[inline]
pub fn rmse_index_aligned(a: &[Point3D], b: &[Point3D]) -> f64 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }
    let mut s = 0.0f64;
    for i in 0..n {
        let dx = (a[i].x - b[i].x) as f64;
        let dy = (a[i].y - b[i].y) as f64;
        let dz = (a[i].z - b[i].z) as f64;
        s += dx * dx + dy * dy + dz * dz;
    }
    (s / (n as f64)).sqrt()
}

#[inline]
pub fn psnr_y(a: &[Point3D], b: &[Point3D]) -> f64 {
    // ITU BT.601 luma: Y = 0.299R + 0.587G + 0.114B
    let n = a.len().min(b.len());
    if n == 0 {
        return f64::INFINITY;
    }
    let mut mse = 0.0f64;
    for i in 0..n {
        let ya = 0.299 * (a[i].r as f64) + 0.587 * (a[i].g as f64) + 0.114 * (a[i].b as f64);
        let yb = 0.299 * (b[i].r as f64) + 0.587 * (b[i].g as f64) + 0.114 * (b[i].b as f64);
        let d = ya - yb;
        mse += d * d;
    }
    mse /= n as f64;
    if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * ((255.0 * 255.0) / mse).log10()
    }
}

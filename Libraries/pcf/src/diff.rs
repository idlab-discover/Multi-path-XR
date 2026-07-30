use spatial_utils::point::Point3D;

pub enum DeltaKind {
    None,         // I-only
    IndexAligned, // P = current - prev (element-wise residual)
}

pub trait DeltaCodec<P> {
    type Scratch: Default;

    fn compute_residuals_index_aligned<'a>(
        prev: &'a [P],
        curr: &'a [P],
        scratch: &'a mut Self::Scratch,
    ) -> Option<&'a [P]>;

    fn apply_residuals_index_aligned(
        base: &mut [P],
        residuals: &[P],
    ) -> Result<(), Box<dyn std::error::Error>>;
}

pub struct NoDelta;

impl<P> DeltaCodec<P> for NoDelta {
    type Scratch = ();

    #[inline]
    fn compute_residuals_index_aligned<'a>(
        _prev: &'a [P],
        _curr: &'a [P],
        _scratch: &'a mut Self::Scratch,
    ) -> Option<&'a [P]> {
        None
    }

    #[inline]
    fn apply_residuals_index_aligned(
        _base: &mut [P],
        _residuals: &[P],
    ) -> Result<(), Box<dyn std::error::Error>> {
        Err("delta frames are unsupported for this PCF spatial type".into())
    }
}

#[derive(Default)]
pub struct Point3DDeltaScratch {
    deltas: Vec<Point3D>,
}

pub type DeltaScratch = Point3DDeltaScratch;

pub struct Point3DDelta;

impl DeltaCodec<Point3D> for Point3DDelta {
    type Scratch = Point3DDeltaScratch;

    #[inline]
    fn compute_residuals_index_aligned<'a>(
        prev: &'a [Point3D],
        curr: &'a [Point3D],
        scratch: &'a mut Self::Scratch,
    ) -> Option<&'a [Point3D]> {
        scratch.deltas.clear();
        let n = prev.len().min(curr.len());
        scratch.deltas.reserve(n);
        for i in 0..n {
            let a = &prev[i];
            let b = &curr[i];
            scratch.deltas.push(Point3D {
                x: b.x - a.x,
                y: b.y - a.y,
                z: b.z - a.z,
                r: b.r.wrapping_sub(a.r),
                g: b.g.wrapping_sub(a.g),
                b: b.b.wrapping_sub(a.b),
            });
        }
        Some(&scratch.deltas)
    }

    #[inline]
    fn apply_residuals_index_aligned(
        base: &mut [Point3D],
        residuals: &[Point3D],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let n = base.len().min(residuals.len());
        for i in 0..n {
            let d = &residuals[i];
            let o = &mut base[i];
            o.x += d.x;
            o.y += d.y;
            o.z += d.z;
            o.r = o.r.wrapping_add(d.r);
            o.g = o.g.wrapping_add(d.g);
            o.b = o.b.wrapping_add(d.b);
        }
        Ok(())
    }
}

#[inline]
pub fn compute_residuals_index_aligned<'a>(
    prev: &'a [Point3D],
    curr: &'a [Point3D],
    scratch: &'a mut DeltaScratch,
) -> &'a [Point3D] {
    Point3DDelta::compute_residuals_index_aligned(prev, curr, scratch).unwrap_or(&[])
}

#[inline]
pub fn apply_residuals_index_aligned(base: &mut [Point3D], residuals: &[Point3D]) {
    let _ = Point3DDelta::apply_residuals_index_aligned(base, residuals);
}

use std::io::Write;

pub struct Csv {
    file: std::fs::File,
}
impl Csv {
    pub fn create(path: &str) -> std::io::Result<Self> {
        let f = std::fs::File::create(path)?;
        Ok(Self { file: f })
    }
    pub fn header(&mut self) -> std::io::Result<()> {
        writeln!(self.file, "dataset,sweep,streams,frames,i_frames,p_frames,points_total,bytes_total,overhead_pct,avg_I,avg_P,enc_p50_ms,enc_p95_ms,dec_p50_ms,dec_p95_ms,e2e_p50_ms,e2e_p95_ms,throughput_mbps,throughput_mpts,rmse,psnr_y")?;
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    pub fn row(
        &mut self,
        dataset: &str,
        sweep: &str,
        streams: u32,
        frames: u64,
        i_count: u64,
        p_count: u64,
        points_total: u64,
        bytes_total: u64,
        overhead_pct: f64,
        avg_i: f64,
        avg_p: f64,
        enc_p50: f64,
        enc_p95: f64,
        dec_p50: f64,
        dec_p95: f64,
        e2e_p50: f64,
        e2e_p95: f64,
        mbps: f64,
        mpts: f64,
        rmse: Option<f64>,
        psnr: Option<f64>,
    ) -> std::io::Result<()> {
        writeln!(self.file, "{dataset},{sweep},{streams},{frames},{i_count},{p_count},{points_total},{bytes_total},{overhead_pct:.3},{avg_i:.1},{avg_p:.1},{enc_p50:.3},{enc_p95:.3},{dec_p50:.3},{dec_p95:.3},{e2e_p50:.3},{e2e_p95:.3},{mbps:.3},{mpts:.3},{},{},",
            rmse.map(|v| format!("{v:.6}")).unwrap_or_default(),
            psnr.map(|v| if v.is_infinite() { "inf".into() } else { format!("{v:.3}") }).unwrap_or_default()
        )
    }
}

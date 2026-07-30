use serde::Deserialize;
use spatial_codecs::{bench::config::ResampleSpec, encoder::EncodingParams};

#[derive(Deserialize, Clone)]
pub struct BenchConfig {
    pub datasets: Datasets,
    #[serde(default)]
    pub pcf: Pcf,
    #[serde(default)]
    pub sweeps: Vec<Sweep>,
    #[serde(default)]
    pub mode: Mode,
    #[serde(default)]
    pub warmup: bool,
    #[serde(default)]
    pub progress: bool,
}

#[derive(Deserialize, Clone)]
pub struct Datasets {
    pub roots: Vec<String>, // folders of frames (PLY or already-decoded points via your loader)
    #[serde(default)]
    pub limit: Option<usize>, // limit # frames per dataset
    #[serde(default)]
    pub resample: Option<ResampleSpec>,
}

#[derive(Deserialize, Clone)]
pub struct Pcf {
    #[serde(default)]
    pub streams: u32, // parallel streams (logically)
    #[serde(default)]
    pub delta: DeltaKindCfg,
    #[serde(default)]
    pub gop: u32, // I-frame every N
    #[serde(default)]
    pub mtu: usize, // chunk MTU
    #[serde(default)]
    pub fidelity: FidelityCfg, // optional error metrics
}
impl Default for Pcf {
    fn default() -> Self {
        Self {
            streams: 1,
            delta: DeltaKindCfg::IndexAligned,
            gop: 60,
            mtu: 1200,
            fidelity: FidelityCfg::default(),
        }
    }
}

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
#[derive(Default)]
pub enum DeltaKindCfg {
    None,
    #[default]
    IndexAligned,
}

#[derive(Deserialize, Clone)]
pub struct FidelityCfg {
    #[serde(default)]
    pub rmse: bool,
    #[serde(default)]
    pub psnr_y: bool,
    #[serde(default)]
    pub d1_symmetric: bool, // heavy; off by default
}
impl Default for FidelityCfg {
    fn default() -> Self {
        Self {
            rmse: true,
            psnr_y: true,
            d1_symmetric: false,
        }
    }
}

#[derive(Deserialize, Clone)]
pub struct Sweep {
    pub name: String,
    pub params: EncodingParams, // inner codec + wrapper (Zstd/LZ4/…)
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "kebab-case")]
#[derive(Default)]
pub enum Mode {
    #[default]
    Accuracy,
    Throughput,
}

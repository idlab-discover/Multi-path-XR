use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbrMode {
    Simple,
    Balanced,
    Advanced,
}

impl AbrMode {
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Simple => 0,
            Self::Balanced => 1,
            Self::Advanced => 2,
        }
    }

    pub const fn as_u32(self) -> u32 {
        self.as_u8() as u32
    }

    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Simple),
            1 => Some(Self::Balanced),
            2 => Some(Self::Advanced),
            _ => None,
        }
    }

    pub const fn from_u32(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Simple),
            1 => Some(Self::Balanced),
            2 => Some(Self::Advanced),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AbrModeHandle {
    mode: Arc<AtomicU8>,
}

impl AbrModeHandle {
    pub fn new(mode: AbrMode) -> Self {
        Self {
            mode: Arc::new(AtomicU8::new(mode.as_u8())),
        }
    }

    pub fn get(&self) -> AbrMode {
        AbrMode::from_u8(self.mode.load(Ordering::Relaxed)).unwrap_or(AbrMode::Advanced)
    }

    pub fn set(&self, mode: AbrMode) {
        self.mode.store(mode.as_u8(), Ordering::Relaxed);
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AbrConfig {
    pub mode: AbrMode,
    pub bandwidth_alpha: f64,
    pub bandwidth_overhead_fraction: f64,
    pub default_bandwidth_bps: f64,
    pub selection_confirmation_samples: u8,
    pub enable_hysteresis: bool,
    pub upswitch_hysteresis_factor: f64,
    pub downswitch_hysteresis_factor: f64,
    pub enable_hold_down: bool,
    pub min_upswitch_interval_ms: u64,
    pub enable_buffer_guard: bool,
    pub min_buffer_for_upswitch_s: f64,
}

impl AbrConfig {
    pub fn for_mode(mode: AbrMode) -> Self {
        let mut config = Self {
            mode,
            bandwidth_alpha: 0.25,
            bandwidth_overhead_fraction: 0.05,
            default_bandwidth_bps: 50_000_000.0,
            selection_confirmation_samples: 2,
            enable_hysteresis: false,
            upswitch_hysteresis_factor: 1.0,
            downswitch_hysteresis_factor: 1.0,
            enable_hold_down: false,
            min_upswitch_interval_ms: 0,
            enable_buffer_guard: false,
            min_buffer_for_upswitch_s: 0.0,
        };

        match mode {
            AbrMode::Simple => {}
            AbrMode::Balanced => {
                config.enable_hysteresis = true;
                config.upswitch_hysteresis_factor = 1.10;
                config.downswitch_hysteresis_factor = 0.95;
                config.enable_buffer_guard = true;
                config.min_buffer_for_upswitch_s = 0.050;
            }
            AbrMode::Advanced => {
                config.enable_hysteresis = true;
                config.upswitch_hysteresis_factor = 1.15;
                config.downswitch_hysteresis_factor = 0.90;
                config.enable_hold_down = true;
                config.min_upswitch_interval_ms = 500;
                config.enable_buffer_guard = true;
                config.min_buffer_for_upswitch_s = 0.050;
            }
        }

        config
    }
}

impl Default for AbrConfig {
    fn default() -> Self {
        Self::for_mode(AbrMode::Simple)
    }
}

#[cfg(test)]
mod tests {
    use super::{AbrMode, AbrModeHandle};

    #[test]
    fn abr_mode_round_trips_through_raw_values() {
        for mode in [AbrMode::Simple, AbrMode::Balanced, AbrMode::Advanced] {
            assert_eq!(AbrMode::from_u8(mode.as_u8()), Some(mode));
            assert_eq!(AbrMode::from_u32(mode.as_u32()), Some(mode));
        }
    }

    #[test]
    fn abr_mode_handle_updates_shared_value() {
        let handle = AbrModeHandle::new(AbrMode::Advanced);
        let cloned = handle.clone();

        cloned.set(AbrMode::Simple);

        assert_eq!(handle.get(), AbrMode::Simple);
    }
}

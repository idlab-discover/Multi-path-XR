//! Minimal Virtual Wall manager for **existing** experiments.
//!
//! This crate is a "no SLICES CLI" replacement for the original `virtual-wall` crate.
//! You provide a GENI/Emulab **manifest RSpec** (and optionally a previously persisted
//! `state.json` produced by the full manager). The manager then:
//!
//! - parses nodes and links,
//! - resolves SSH login targets,
//! - executes remote commands over SSH,
//! - manages SSH tunnels (`-L` / `-R`) with lifecycle tracking.
//!
//! It does **not** provision resources, inject cloud-init, or manage experiment lifecycle.

pub mod config;
pub mod error;
pub mod manager;
pub mod rspec;
pub mod ssh;
pub mod state;
pub mod tunnels;

pub use config::{HostKeyChecking, VirtualWallConfig, VirtualWallConfigFile};
pub use error::{Result, VirtualWallError};
pub use manager::{StartOptions, StartSummary, VirtualWallManager};
pub use tunnels::{TunnelDirection, TunnelEndpoint, TunnelInfo, TunnelRequest};

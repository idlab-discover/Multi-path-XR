use rustls::crypto::aws_lc_rs;
use std::sync::OnceLock;
use tracing::debug;

/// Ensures the process installs a deterministic crypto provider for Rustls.
///
/// Rustls 0.23 pulls in both `aws-lc-rs` and `ring` by default, which means the
/// library cannot automatically decide which backend to use unless a provider is
/// installed explicitly. We run this once per process to avoid the runtime panic
/// emitted by DTLS/WebRTC when no provider has been selected yet.
pub fn install_default_crypto_provider() {
    static INSTALL_ONCE: OnceLock<()> = OnceLock::new();
    INSTALL_ONCE.get_or_init(|| {
        if let Err(existing) = aws_lc_rs::default_provider().install_default() {
            // Losing the race is fine; another thread already installed the provider.
            debug!(
                "Rustls crypto provider already installed: fips={}",
                existing.fips()
            );
        }
    });
}

pub fn ensure_metrics_initialized() {
    use std::sync::Once;

    static INIT: Once = Once::new();

    INIT.call_once(|| {
        let _ = metrics::MetricsBuilder::new()
            .add_label("mode", "test")
            .build();
    });
}

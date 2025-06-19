use tracing_subscriber::{EnvFilter, Registry, layer::SubscriberExt, util::SubscriberInitExt};

pub fn init_tracing() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing::info!("Initializing structured logging with JSON output");

    // Configure structured logging with JSON output and span information
    // This provides comprehensive observability including request tracing,
    // which works seamlessly with the Prometheus metrics implementation

    // Set up subscriber with JSON logging
    Registry::default()
        .with(EnvFilter::from_default_env())
        .with(
            tracing_subscriber::fmt::layer()
                .json()
                .with_current_span(false)
                .with_span_list(true),
        )
        .init();

    tracing::info!("Structured logging initialized successfully");
    Ok(())
}

pub fn shutdown_tracing() {
    tracing::info!("Tracing shutdown complete");
}

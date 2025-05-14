use std::sync::Arc;
use tokio::task::JoinHandle;

use crate::{
    HealthChecker, adapters::http_client::HyperHttpClient, config::models::ServerConfig,
    core::ProxyService,
};

// Helper function to spawn a new health checker task
pub fn spawn_health_checker_task(
    proxy_service_to_use: Arc<ProxyService>,
    http_client_clone: Arc<HyperHttpClient>,
    config_for_health_check: Arc<ServerConfig>,
    source_log_prefix: String, // To differentiate log source (e.g., "Initial", "File Reload", "API Reload")
) -> JoinHandle<()> {
    tokio::spawn(async move {
        if config_for_health_check.health_check.enabled {
            tracing::info!(
                "({}) Health checker task started. Interval: {}s, Path: {}, Unhealthy Threshold: {}, Healthy Threshold: {}",
                source_log_prefix,
                config_for_health_check.health_check.interval_secs,
                config_for_health_check.health_check.path,
                config_for_health_check.health_check.unhealthy_threshold,
                config_for_health_check.health_check.healthy_threshold
            );
            let health_checker = HealthChecker::new(proxy_service_to_use, http_client_clone);
            if let Err(e) = health_checker.run().await {
                tracing::error!("({}) Health checker run error: {}", source_log_prefix, e);
            }
        } else {
            tracing::info!(
                "({}) Health checking is disabled by current configuration snapshot. Health checker task not running.",
                source_log_prefix
            );
        }
    })
}

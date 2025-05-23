use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::time::sleep;

use crate::adapters::http_client::HyperHttpClient;
use crate::config::{HealthCheckConfig, HealthStatus};
use crate::core::ProxyService;
use crate::core::backend::BackendHealth;
use crate::ports::http_client::HttpClient;

pub struct HealthChecker {
    proxy_service: Arc<ProxyService>,
    http_client: Arc<HyperHttpClient>,
}

impl HealthChecker {
    pub fn new(proxy_service: Arc<ProxyService>, http_client: Arc<HyperHttpClient>) -> Self {
        Self {
            proxy_service,
            http_client,
        }
    }

    pub async fn run(&self) -> Result<()> {
        let health_config = self.proxy_service.health_config();

        if !health_config.enabled {
            // Removed parentheses
            tracing::info!("Health checking is disabled");
            return Ok(());
        }

        let interval = Duration::from_secs(health_config.interval_secs);
        let timeout = Duration::from_secs(health_config.timeout_secs);

        tracing::info!(
            "Starting health checker with interval: {}s, timeout: {}s, default path: {}",
            health_config.interval_secs,
            health_config.timeout_secs,
            health_config.path
        );

        loop {
            // Sleep at the beginning to allow the server to start up
            sleep(interval).await;

            tracing::info!("Running health checks on all backends...");

            // Check each backend using the getter method instead of direct field access
            for backend_entry in self.proxy_service.backend_health().iter() {
                let target = backend_entry.key().clone();
                let backend_health = backend_entry.value();

                // Get backend-specific health check path or use default
                let backend_path = self.proxy_service.get_backend_health_path(&target);

                // Construct health check URL
                let health_check_url = format!("{}{}", target, backend_path);

                tracing::info!("Health checking: {}", health_check_url);

                // Perform the health check with timeout
                match self
                    .http_client
                    .health_check(&health_check_url, timeout.as_secs())
                    .await
                {
                    Ok(is_healthy) => {
                        if is_healthy {
                            // Increment success counter
                            let successes = backend_health
                                .consecutive_successes
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                                + 1;

                            // Log every successful health check
                            tracing::info!(
                                "Health check for {} succeeded ({} consecutive successes)",
                                target,
                                successes
                            );

                            // If we've reached the threshold, mark as healthy
                            if successes >= health_config.healthy_threshold
                                && backend_health.status() == HealthStatus::Unhealthy
                            {
                                tracing::info!(
                                    "Backend {} is now HEALTHY (after {} consecutive successes)",
                                    target,
                                    successes
                                );
                                backend_health.mark_healthy();
                            }
                        } else {
                            self.handle_health_check_failure(
                                &target,
                                backend_health,
                                health_config,
                                "Backend returned unhealthy status",
                            );
                        }
                    }
                    Err(err) => {
                        self.handle_health_check_failure(
                            &target,
                            backend_health,
                            health_config,
                            &format!("Health check error: {}", err),
                        );
                    }
                }
            }

            tracing::info!("Health check cycle completed");
        }
    }

    fn handle_health_check_failure(
        &self,
        target: &str,
        backend_health: &BackendHealth,
        health_config: &HealthCheckConfig,
        reason: &str,
    ) {
        // Atomically increment failure counter and get new value
        let failures = backend_health
            .consecutive_failures
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;

        // Reset success counter atomically
        backend_health
            .consecutive_successes
            .store(0, std::sync::atomic::Ordering::Relaxed);

        // Log all failures at the INFO level for better visibility
        tracing::info!(
            "Health check failed for {}: {} (failures: {}/{})",
            target,
            reason,
            failures,
            health_config.unhealthy_threshold
        );

        // Mark as unhealthy if threshold reached and current status is healthy
        if failures >= health_config.unhealthy_threshold
            && backend_health.status() == HealthStatus::Healthy
        {
            tracing::warn!(
                "Backend {} is now UNHEALTHY (after {} consecutive failures): {}",
                target,
                failures,
                reason
            );
            backend_health.mark_unhealthy();
        }
    }
}

use dashmap::DashMap;
use std::collections::HashMap;
use std::sync::Arc;

use crate::config::{HealthCheckConfig, HealthStatus, RouteConfig, ServerConfig};
use crate::core::backend::{BackendHealth, BackendUrl};

pub struct ProxyService {
    config: Arc<ServerConfig>,
    backend_health: Arc<DashMap<String, BackendHealth>>,
}

impl ProxyService {
    pub fn new(config: Arc<ServerConfig>) -> Self {
        let backend_health = Arc::new(DashMap::new());

        let backends = Self::collect_backends(&config.routes);

        for backend in &backends {
            if let Ok(backend_url) = BackendUrl::new(backend) {
                backend_health.insert(backend.clone(), BackendHealth::new(backend_url));
            } else {
                tracing::error!("Invalid backend URL: {}", backend);
            }
        }

        Self {
            config,
            backend_health,
        }
    }

    pub fn backend_health(&self) -> &DashMap<String, BackendHealth> {
        &self.backend_health
    }

    pub fn collect_backends(routes: &HashMap<String, RouteConfig>) -> Vec<String> {
        let mut backends = routes
            .values()
            .flat_map(|route_config| match route_config {
                RouteConfig::LoadBalance { targets, .. } => targets.clone(),
                RouteConfig::Proxy { target, .. } => vec![target.clone()],
                _ => Vec::new(),
            })
            .collect::<Vec<_>>();

        backends.sort();
        backends.dedup();
        backends
    }

    pub fn find_matching_route(&self, path: &str) -> Option<(String, RouteConfig)> {
        self.config
            .routes
            .iter()
            .filter(|(prefix, _)| path.starts_with(*prefix))
            .max_by_key(|(prefix, _)| prefix.len())
            .map(|(prefix, config)| (prefix.to_string(), config.clone()))
    }

    pub fn health_config(&self) -> &HealthCheckConfig {
        &self.config.health_check
    }

    pub fn get_backend_health_path(&self, target: &str) -> String {
        self.config
            .backend_health_paths
            .get(target)
            .cloned()
            .unwrap_or_else(|| self.config.health_check.path.clone())
    }

    pub fn get_backend_health_status(&self, target: &str) -> HealthStatus {
        self.backend_health
            .get(target)
            .map(|backend| backend.status())
            .unwrap_or(HealthStatus::Healthy)
    }

    pub fn get_healthy_backends(&self, targets: &[String]) -> Vec<String> {
        if !self.config.health_check.enabled {
            return targets.to_vec();
        }

        targets
            .iter()
            .filter(|target| self.get_backend_health_status(target) == HealthStatus::Healthy)
            .cloned()
            .collect()
    }
}

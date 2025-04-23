use std::collections::HashMap;
use std::sync::Arc;
use dashmap::DashMap;

use crate::config::{HealthCheckConfig, HealthStatus, RouteConfig, ServerConfig};
use crate::core::backend::BackendHealth;

pub struct ProxyService {
    pub config: Arc<ServerConfig>,
    pub backend_health: Arc<DashMap<String, BackendHealth>>,
    pub counter: std::sync::atomic::AtomicUsize,
}

impl ProxyService {
    pub fn new(config: Arc<ServerConfig>) -> Self {
        // Initialize backend health tracking
        let backend_health = Arc::new(DashMap::new());
        
        // Collect all backend targets
        let backends = Self::collect_backends(&config.routes);
        
        // Initialize health status for all backends
        for backend in &backends {
            backend_health.insert(backend.clone(), BackendHealth::new(backend.clone()));
        }

        Self {
            config,
            backend_health,
            counter: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    // Helper to collect all backends from route configuration
    pub fn collect_backends(routes: &HashMap<String, RouteConfig>) -> Vec<String> {
        let mut backends = Vec::new();
        
        for route_config in routes.values() {
            match route_config {
                RouteConfig::LoadBalance { targets, .. } => {
                    backends.extend(targets.clone());
                }
                RouteConfig::Proxy { target } => {
                    backends.push(target.clone());
                }
                _ => {}
            }
        }
        
        // Deduplicate backends
        backends.sort();
        backends.dedup();
        backends
    }

    // Find matching route for a path
    pub fn find_matching_route(&self, path: &str) -> Option<(String, RouteConfig)> {
        let mut matched_route = None;
        let mut matched_prefix = "";

        for (route_prefix, route_config) in &self.config.routes {
            if path.starts_with(route_prefix) && route_prefix.len() > matched_prefix.len() {
                matched_route = Some(route_config.clone());
                matched_prefix = route_prefix;
            }
        }
        
        matched_route.map(|route| (matched_prefix.to_string(), route))
    }

    // Get health config
    pub fn health_config(&self) -> &HealthCheckConfig {
        &self.config.health_check
    }
    
    // Get backend-specific health path or default
    pub fn get_backend_health_path(&self, target: &str) -> String {
        self.config.backend_health_paths
            .get(target)
            .cloned()
            .unwrap_or_else(|| self.config.health_check.path.clone())
    }
    
    // Get the health status of a backend
    pub fn get_backend_health_status(&self, target: &str) -> HealthStatus {
        if let Some(backend) = self.backend_health.get(target) {
            backend.status()
        } else {
            // If no health info exists, assume healthy
            HealthStatus::Healthy
        }
    }
    
    // Get filtered healthy backends
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
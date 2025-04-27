use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ServerConfig {
    pub listen_addr: String,
    pub routes: HashMap<String, RouteConfig>,
    #[serde(default)]
    pub tls: Option<TlsConfig>,
    #[serde(default)]
    pub health_check: HealthCheckConfig,
    #[serde(default)]
    pub backend_health_paths: HashMap<String, String>,
}

impl ServerConfig {
    /// Create a new server configuration builder
    pub fn builder() -> ServerConfigBuilder {
        ServerConfigBuilder::default()
    }
}

/// Builder for ServerConfig to allow for cleaner configuration creation
#[derive(Default)]
pub struct ServerConfigBuilder {
    listen_addr: Option<String>,
    routes: HashMap<String, RouteConfig>,
    tls: Option<TlsConfig>,
    health_check: Option<HealthCheckConfig>,
    backend_health_paths: HashMap<String, String>,
}

impl ServerConfigBuilder {
    /// Set the listen address
    pub fn listen_addr(mut self, addr: impl Into<String>) -> Self {
        self.listen_addr = Some(addr.into());
        self
    }

    /// Add a route with the given path prefix and configuration
    pub fn route(mut self, path_prefix: impl Into<String>, config: RouteConfig) -> Self {
        self.routes.insert(path_prefix.into(), config);
        self
    }

    /// Set TLS configuration
    pub fn tls(mut self, cert_path: impl Into<String>, key_path: impl Into<String>) -> Self {
        self.tls = Some(TlsConfig {
            cert_path: cert_path.into(),
            key_path: key_path.into(),
        });
        self
    }

    /// Set health check configuration
    pub fn health_check(mut self, config: HealthCheckConfig) -> Self {
        self.health_check = Some(config);
        self
    }

    /// Add a backend-specific health check path
    pub fn backend_health_path(mut self, backend: impl Into<String>, path: impl Into<String>) -> Self {
        self.backend_health_paths.insert(backend.into(), path.into());
        self
    }

    /// Build the final ServerConfig
    pub fn build(self) -> Result<ServerConfig, String> {
        let listen_addr = self.listen_addr.ok_or_else(|| "listen_addr is required".to_string())?;
        
        if self.routes.is_empty() {
            return Err("At least one route must be configured".to_string());
        }
        
        Ok(ServerConfig {
            listen_addr,
            routes: self.routes,
            tls: self.tls,
            health_check: self.health_check.unwrap_or_default(),
            backend_health_paths: self.backend_health_paths,
        })
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TlsConfig {
    pub cert_path: String,
    pub key_path: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct HealthCheckConfig {
    pub enabled: bool,
    pub interval_secs: u64,
    pub timeout_secs: u64,
    pub path: String,
    pub unhealthy_threshold: u32,
    pub healthy_threshold: u32,
}

impl Default for HealthCheckConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_secs: 10,
            timeout_secs: 2,
            path: "/health".to_string(),
            unhealthy_threshold: 3,
            healthy_threshold: 2,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum RouteConfig {
    #[serde(rename = "static")]
    Static { root: String },
    #[serde(rename = "redirect")]
    Redirect {
        target: String,
        status_code: Option<u16>,
    },
    #[serde(rename = "proxy")]
    Proxy { target: String },
    #[serde(rename = "load_balance")]
    LoadBalance {
        targets: Vec<String>,
        strategy: LoadBalanceStrategy,
    },
}

impl RouteConfig {
    /// Create a static file serving route
    pub fn static_files(root: impl Into<String>) -> Self {
        RouteConfig::Static { root: root.into() }
    }
    
    /// Create a redirect route
    pub fn redirect(target: impl Into<String>, status_code: Option<u16>) -> Self {
        RouteConfig::Redirect { 
            target: target.into(),
            status_code,
        }
    }
    
    /// Create a proxy route to a single backend
    pub fn proxy(target: impl Into<String>) -> Self {
        RouteConfig::Proxy { target: target.into() }
    }
    
    /// Create a load balanced route with multiple backends
    pub fn load_balance(targets: Vec<String>, strategy: LoadBalanceStrategy) -> Self {
        RouteConfig::LoadBalance { targets, strategy }
    }
    
    /// Create a load balanced route builder
    pub fn load_balancer() -> LoadBalancerBuilder {
        LoadBalancerBuilder::default()
    }
}

/// Builder for load balanced routes
#[derive(Default)]
pub struct LoadBalancerBuilder {
    targets: Vec<String>,
    strategy: Option<LoadBalanceStrategy>,
}

impl LoadBalancerBuilder {
    /// Add a target backend
    pub fn target(mut self, target: impl Into<String>) -> Self {
        self.targets.push(target.into());
        self
    }
    
    /// Add multiple target backends
    pub fn targets(mut self, targets: impl IntoIterator<Item = impl Into<String>>) -> Self {
        for target in targets {
            self.targets.push(target.into());
        }
        self
    }
    
    /// Set the load balancing strategy to round robin
    pub fn round_robin(mut self) -> Self {
        self.strategy = Some(LoadBalanceStrategy::RoundRobin);
        self
    }
    
    /// Set the load balancing strategy to random
    pub fn random(mut self) -> Self {
        self.strategy = Some(LoadBalanceStrategy::Random);
        self
    }
    
    /// Build the load balanced route
    pub fn build(self) -> Result<RouteConfig, String> {
        if self.targets.is_empty() {
            return Err("At least one target must be specified for load balancing".to_string());
        }
        
        let strategy = self.strategy.unwrap_or(LoadBalanceStrategy::RoundRobin);
        
        Ok(RouteConfig::LoadBalance {
            targets: self.targets,
            strategy,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LoadBalanceStrategy {
    #[serde(rename = "round_robin")]
    RoundRobin,
    #[serde(rename = "random")]
    Random,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HealthStatus {
    #[serde(rename = "healthy")]
    Healthy,
    #[serde(rename = "unhealthy")]
    Unhealthy,
}

impl std::fmt::Display for HealthStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HealthStatus::Healthy => write!(f, "healthy"),
            HealthStatus::Unhealthy => write!(f, "unhealthy"),
        }
    }
}
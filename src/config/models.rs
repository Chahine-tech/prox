use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct HeaderActions {
    #[serde(default)]
    pub add: HashMap<String, String>,
    #[serde(default)]
    pub remove: Vec<String>,
    #[serde(default)]
    pub condition: Option<RequestCondition>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct BodyActions {
    #[serde(default)]
    pub set_text: Option<String>, // Set the entire body to this text
    #[serde(default)]
    pub set_json: Option<serde_json::Value>, // Set the entire body to this JSON value
    #[serde(default)]
    pub condition: Option<RequestCondition>,
    // Future enhancements:
    // pub add_json_fields: HashMap<String, serde_json::Value>,
    // pub remove_json_fields: Vec<String>,
    // pub transform_script: Option<String>, // For more complex transformations
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RequestCondition {
    #[serde(default)]
    pub path_matches: Option<String>, // Regex to match the request path
    #[serde(default)]
    pub method_is: Option<String>, // Exact match for request method (e.g., "GET", "POST")
    #[serde(default)]
    pub has_header: Option<HeaderCondition>,
    // Potentially add more conditions: client_ip_is, query_param_is, etc.
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct HeaderCondition {
    pub name: String,
    pub value_matches: Option<String>, // Regex to match header value
}

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
    pub fn backend_health_path(
        mut self,
        backend: impl Into<String>,
        path: impl Into<String>,
    ) -> Self {
        self.backend_health_paths
            .insert(backend.into(), path.into());
        self
    }

    /// Build the final ServerConfig
    pub fn build(self) -> Result<ServerConfig, String> {
        let listen_addr = self
            .listen_addr
            .ok_or_else(|| "listen_addr is required".to_string())?;

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

fn default_status_code() -> u16 {
    429
}

fn default_message() -> String {
    "Too Many Requests".to_string()
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RateLimitBy {
    Ip,
    Header,
    Route,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RateLimitAlgorithm {
    TokenBucket,
    FixedWindow, // Added: For Fixed Window algorithm
    SlidingWindow, // Added: For Sliding Window algorithm
                 // In the future, other algorithms like FixedWindow or SlidingWindow could be added here.
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MissingKeyPolicy {
    Allow,
    Deny,
}

fn default_on_missing_key() -> MissingKeyPolicy {
    MissingKeyPolicy::Allow
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RateLimitConfig {
    pub by: RateLimitBy,
    #[serde(default)]
    pub header_name: Option<String>, // Should be Some if by == Header
    pub requests: u64,
    pub period: String, // Parsed by humantime, e.g., "1s", "5m", "1h"
    #[serde(default = "default_status_code")]
    pub status_code: u16,
    #[serde(default = "default_message")]
    pub message: String,
    #[serde(default = "default_rate_limit_algorithm")] // Changed: Default to TokenBucket
    pub algorithm: RateLimitAlgorithm, // Changed: Made non-optional
    #[serde(default = "default_on_missing_key")]
    pub on_missing_key: MissingKeyPolicy,
}

fn default_rate_limit_algorithm() -> RateLimitAlgorithm {
    // Added: Default function
    RateLimitAlgorithm::TokenBucket
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "type")] // Added: Use the 'type' field in YAML to determine the enum variant
#[serde(rename_all = "snake_case")] // Added: Match snake_case YAML keys (e.g., "load_balance") to PascalCase enum variants (e.g., LoadBalance)
pub enum RouteConfig {
    Static {
        // Assuming 'root: String' exists here
        root: String, // Ensure this field is present
        rate_limit: Option<RateLimitConfig>,
        // No header manipulation for static routes in this iteration
    },
    Redirect {
        // Assuming 'target: String' and 'status_code: Option<u16>' exist here
        target: String,           // Ensure this field is present
        status_code: Option<u16>, // Ensure this field is present
        rate_limit: Option<RateLimitConfig>,
        // No header or body manipulation for redirect routes
    },
    Proxy {
        target: String,
        path_rewrite: Option<String>,
        rate_limit: Option<RateLimitConfig>,
        #[serde(default)]
        request_headers: Option<HeaderActions>,
        #[serde(default)]
        response_headers: Option<HeaderActions>,
        #[serde(default)]
        request_body: Option<BodyActions>,
        #[serde(default)]
        response_body: Option<BodyActions>,
    },
    LoadBalance {
        targets: Vec<String>,
        strategy: LoadBalanceStrategy,
        path_rewrite: Option<String>,
        rate_limit: Option<RateLimitConfig>,
        #[serde(default)]
        request_headers: Option<HeaderActions>,
        #[serde(default)]
        response_headers: Option<HeaderActions>,
        #[serde(default)]
        request_body: Option<BodyActions>,
        #[serde(default)]
        response_body: Option<BodyActions>,
    },
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
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

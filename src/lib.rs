/// Prox - A configurable HTTP reverse proxy
///
/// This crate provides a reverse proxy server with features like:
/// - Static file serving
/// - HTTP/HTTPS support
/// - Load balancing
/// - Health checking
/// - Path-based routing
// Re-export public modules with explicit visibility controls
pub mod config;
pub mod ports;

// These modules are implementation details and should not be directly used by users
pub(crate) mod adapters;
pub(crate) mod core;

// Re-export the specific types needed by the binary crate
pub use crate::adapters::http::server::HyperServer;
pub use crate::adapters::health_checker::HealthChecker;
pub use crate::adapters::file_system::TowerFileSystem;
pub use crate::adapters::http_client::HyperHttpClient;
pub use crate::core::ProxyService;
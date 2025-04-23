use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber;
use tokio;

mod adapters;
mod config;
mod core;
mod ports;

use crate::adapters::health_checker::HealthChecker;
use crate::adapters::http::HyperServer;
use crate::adapters::{HyperHttpClient, TowerFileSystem};
use crate::config::load_config;
use crate::core::ProxyService;
use crate::ports::{file_system::FileSystem, http_client::HttpClient, http_server::HttpServer};

#[derive(Parser, Debug)]
#[clap(author, version, about)]
struct Args {
    #[clap(short, long, default_value = "config.yaml")]
    config: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    // Parse command line arguments
    let args = Args::parse();

    // Load configuration
    tracing::info!("Loading configuration from {}", args.config);
    let config = match load_config(&args.config).await {
        Ok(config) => config,
        Err(err) => {
            tracing::error!("Failed to load config: {}", err);
            return Err(err);
        }
    };

    // Create shared configuration
    let config = Arc::new(config);
    
    // Create adapters
    let http_client: Arc<dyn HttpClient> = Arc::new(HyperHttpClient::new());
    let file_system: Arc<dyn FileSystem> = Arc::new(TowerFileSystem::new());
    
    // Create the proxy service
    let proxy_service = ProxyService::new(config.clone());
    let proxy_service_arc = Arc::new(proxy_service);
    
    // Create the HTTP server
    let server = HyperServer::with_dependencies(
        proxy_service_arc.clone(),
        config.clone(),
        http_client.clone(),
        file_system.clone(),
    );
    
    // Log configured routes
    for (prefix, route) in &config.routes {
        tracing::info!("Configured route: {} -> {:?}", prefix, route);
    }

    // Start health checker if health checking is enabled
    if config.health_check.enabled {
        let health_checker = HealthChecker::new(
            proxy_service_arc.clone(),
            http_client.clone(),
        );
        
        // Start health checker in a separate task
        tokio::spawn(async move {
            if let Err(e) = health_checker.run().await {
                tracing::error!("Health checker error: {}", e);
            }
        });
        
        // Start server
        tracing::info!("Starting server on {}", config.listen_addr);
        println!("Server listening on {}", config.listen_addr);
        server.run().await?;
    } else {
        // Start server without health checking
        tracing::info!("Starting server on {} (health checking disabled)", config.listen_addr);
        println!("Server listening on {}", config.listen_addr);
        server.run().await?;
    }

    Ok(())
}

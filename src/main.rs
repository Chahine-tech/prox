use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;

// Import directly from crate root where they are re-exported
use prox::{
    HealthChecker,
    HyperHttpClient,
    HyperServer,
    ProxyService,
    TowerFileSystem,
    config::loader::load_config,
    // Remove direct imports of traits if concrete types are used for instantiation
    // ports::{file_system::FileSystem, http_client::HttpClient, http_server::HttpServer}
    ports::http_server::HttpServer, // HttpServer trait is still needed for server.run()
};

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
    let config = load_config(&args.config)
        .await
        .with_context(|| format!("Failed to load config from {}", args.config))?;

    // Create shared configuration
    let config = Arc::new(config);

    // Create adapters with concrete types
    let http_client: Arc<HyperHttpClient> = Arc::new(HyperHttpClient::new());
    let file_system: Arc<TowerFileSystem> = Arc::new(TowerFileSystem::new());

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
        let health_checker = HealthChecker::new(proxy_service_arc.clone(), http_client.clone());

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
        tracing::info!(
            "Starting server on {} (health checking disabled)",
            config.listen_addr
        );
        println!("Server listening on {}", config.listen_addr);
        server.run().await?;
    }

    Ok(())
}

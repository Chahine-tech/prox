use std::sync::Arc;
use std::sync::RwLock;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use notify::{RecursiveMode, Watcher};
use std::path::Path;
use tokio::sync::{Mutex as TokioMutex, mpsc};

use prox::{
    HealthChecker, HyperHttpClient, ProxyService, TowerFileSystem, UnifiedServer,
    config::loader::load_config, config::models::ServerConfig, tracing_setup,
    utils::graceful_shutdown::GracefulShutdown,
};

#[derive(Parser, Debug)]
#[clap(author, version, about)]
struct Args {
    #[clap(subcommand)]
    command: Option<Commands>,

    #[clap(short, long, default_value = "config.yaml")]
    config: String,
}

#[derive(Parser, Debug)]
enum Commands {
    /// Validate configuration file
    Validate {
        /// Configuration file to validate
        #[clap(short, long, default_value = "config.yaml")]
        config: String,
    },
    /// Start the proxy server (default)
    Serve {
        /// Configuration file to use
        #[clap(short, long, default_value = "config.yaml")]
        config: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Determine the command to run
    let (command, config_path) = match args.command {
        Some(Commands::Validate { config }) => ("validate", config),
        Some(Commands::Serve { config }) => ("serve", config),
        None => ("serve", args.config), // Default to serve with config from args
    };

    match command {
        "validate" => {
            return validate_config_command(&config_path).await;
        }
        "serve" => {
            // Continue with normal server startup
        }
        _ => unreachable!(),
    }

    // Install the crypto provider first thing.
    // Get the aws-lc-rs provider instance.
    let provider = rustls::crypto::aws_lc_rs::default_provider();
    if let Err(e) = rustls::crypto::CryptoProvider::install_default(provider) {
        // If install_default fails, it might be because a provider (possibly aws-lc-rs itself)
        // was already installed, perhaps concurrently or by another part of the application.
        // We log this as a warning. Rustls will panic later if no provider is available
        // when it's needed (e.g. creating TLS configs).
        tracing::warn!(
            "CryptoProvider::install_default for aws-lc-rs reported an error: {:?}. \
            This can happen if a provider was already installed. \
            The application will proceed; ensure a crypto provider is effectively available.",
            e
        );
    } else {
        tracing::info!("Successfully installed aws-lc-rs as the default crypto provider.");
    }

    // Configure tracing_subscriber for JSON output with OpenTelemetry
    tracing_setup::init_tracing().expect("Failed to initialize tracing with OpenTelemetry");

    tracing::info!("Loading initial configuration from {config_path}");
    let initial_server_config_data: ServerConfig = load_config(&config_path)
        .await
        .with_context(|| format!("Failed to load initial config from {config_path}"))?;

    let initial_config_arc = Arc::new(initial_server_config_data);
    let config_holder = Arc::new(RwLock::new(initial_config_arc.clone()));

    let http_client: Arc<HyperHttpClient> = Arc::new(HyperHttpClient::new());
    let file_system: Arc<TowerFileSystem> = Arc::new(TowerFileSystem::new());

    let initial_proxy_service = Arc::new(ProxyService::new(
        config_holder
            .read()
            .map_err(|e| anyhow::anyhow!("Failed to acquire config read lock: {}", e))?
            .clone(),
    ));
    let proxy_service_holder = Arc::new(RwLock::new(initial_proxy_service.clone()));

    // Health Checker Management
    let health_checker_handle_arc_mutex =
        Arc::new(TokioMutex::new(None::<tokio::task::JoinHandle<()>>));

    {
        // Scope for initial health checker start
        let mut handle_guard = health_checker_handle_arc_mutex.lock().await;
        let current_config = config_holder
            .read()
            .map_err(|e| anyhow::anyhow!("Failed to acquire config read lock: {}", e))?
            .clone();
        if current_config.health_check.enabled {
            tracing::info!("Starting initial health checker...");

            // Create HealthChecker directly instead of using utility function
            let health_checker = HealthChecker::new(
                proxy_service_holder
                    .read()
                    .map_err(|e| {
                        anyhow::anyhow!("Failed to acquire proxy service read lock: {}", e)
                    })?
                    .clone(),
                http_client.clone(),
            );

            *handle_guard = Some(tokio::spawn(async move {
                tracing::info!(
                    "Initial health checker task started. Interval: {}s, Path: {}, Unhealthy Threshold: {}, Healthy Threshold: {}",
                    current_config.health_check.interval_secs,
                    current_config.health_check.path,
                    current_config.health_check.unhealthy_threshold,
                    current_config.health_check.healthy_threshold
                );
                if let Err(e) = health_checker.run().await {
                    tracing::error!("Initial health checker run error: {}", e);
                }
            }));
        } else {
            tracing::info!("Initial configuration has health checking disabled.");
        }
    }

    // File Watcher Task
    let config_path_for_watcher = config_path.clone();
    let config_holder_clone = config_holder.clone();
    let proxy_service_holder_clone = proxy_service_holder.clone();
    let http_client_for_watcher = http_client.clone();
    let health_handle_for_watcher = health_checker_handle_arc_mutex.clone();
    let debounce_duration = Duration::from_secs(2);

    tokio::spawn(async move {
        let (notify_tx, mut notify_rx) = mpsc::channel::<()>(10);

        // Determine the directory to watch (parent of the config file)
        let config_file_as_path = Path::new(&config_path_for_watcher);
        let directory_to_watch = config_file_as_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty()) // Ensure parent is not an empty path
            .unwrap_or_else(|| Path::new(".")) // Default to current directory if no parent or parent is empty
            .to_path_buf(); // Owned PathBuf

        // Clone the config file path specifically for the closure, as the closure is `move`
        let config_file_path_for_closure = config_path_for_watcher.clone();

        let mut watcher = match notify::recommended_watcher(
            move |res: Result<notify::Event, notify::Error>| {
                // config_file_path_for_closure is moved into this closure.
                match res {
                    Ok(event) => {
                        let config_file_name_to_check = Path::new(&config_file_path_for_closure)
                            .file_name()
                            .unwrap_or_default();
                        // Check if the event kind is relevant and if it pertains to the specific config file
                        if (event.kind.is_modify()
                            || event.kind.is_create()
                            || event.kind.is_remove())
                            && event.paths.iter().any(|p| {
                                p.file_name().unwrap_or_default() == config_file_name_to_check
                            })
                        {
                            tracing::debug!(
                                "Config file event detected: {:?}, sending signal for reload.",
                                event.kind
                            );
                            if notify_tx.try_send(()).is_err() {
                                // Use tracing::warn! for warnings
                                tracing::warn!(
                                    "Config reload signal channel (internal to watcher) full or disconnected."
                                );
                            }
                        }
                    }
                    Err(e) => tracing::error!("File watch error: {:?}", e),
                }
            },
        ) {
            Ok(w) => w,
            Err(e) => {
                tracing::error!(
                    "Failed to create file watcher: {}. Hot reloading will be disabled.",
                    e
                );
                return;
            }
        };

        // Use the pre-calculated directory_to_watch.
        // The original config_path_for_watcher is still valid here for deriving the filename for logging,
        // as the closure captured config_file_path_for_closure (the clone).
        if let Err(e) = watcher.watch(&directory_to_watch, RecursiveMode::NonRecursive) {
            tracing::error!(
                "Failed to watch config directory {:?}: {}. Hot reloading will be disabled.",
                directory_to_watch,
                e
            );
            return;
        }
        tracing::info!(
            "Watching for config file changes in directory: {:?} for file: {}",
            directory_to_watch,
            Path::new(&config_path_for_watcher)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        );

        let mut last_reload_attempt_time = tokio::time::Instant::now();
        // Allow first event to trigger reload immediately after startup if a quick change happens
        // by setting the last attempt time to be older than the debounce duration.
        last_reload_attempt_time = last_reload_attempt_time
            .checked_sub(debounce_duration)
            .unwrap_or(last_reload_attempt_time);

        while notify_rx.recv().await.is_some() {
            // Debounce
            if last_reload_attempt_time.elapsed() < debounce_duration {
                tracing::info!("Debouncing config reload event. Still within cooldown period.");
                // Consume any other signals that arrived during the cooldown to prevent immediate re-triggering.
                while notify_rx.try_recv().is_ok() {}
                continue;
            }
            last_reload_attempt_time = tokio::time::Instant::now();

            tracing::info!(
                "Attempting to reload configuration from {}",
                config_path_for_watcher
            );
            match load_config(&config_path_for_watcher).await {
                Ok(new_config_data) => {
                    let new_config_arc: Arc<ServerConfig> = Arc::new(new_config_data);
                    tracing::info!("Successfully loaded new configuration.");

                    {
                        match config_holder_clone.write() {
                            Ok(mut config_w) => {
                                *config_w = new_config_arc.clone();
                                tracing::info!("Global ServerConfig Arc updated.");
                            }
                            Err(e) => {
                                tracing::error!(
                                    "Failed to acquire config write lock during reload: {}",
                                    e
                                );
                                continue;
                            }
                        }
                    }

                    let new_proxy_service = Arc::new(ProxyService::new(new_config_arc.clone()));
                    {
                        match proxy_service_holder_clone.write() {
                            Ok(mut proxy_s_w) => {
                                *proxy_s_w = new_proxy_service.clone();
                                tracing::info!("Global ProxyService Arc updated.");
                            }
                            Err(e) => {
                                tracing::error!(
                                    "Failed to acquire proxy service write lock during reload: {}",
                                    e
                                );
                                continue;
                            }
                        }
                    }

                    // Restart HealthChecker
                    let mut handle_guard = health_handle_for_watcher.lock().await;
                    if let Some(old_handle) = handle_guard.take() {
                        tracing::info!("Aborting previous health checker task...");
                        old_handle.abort();
                        // Note: We don't explicitly await the old_handle here for simplicity,
                        // abort() signals termination. If precise shutdown confirmation is needed,
                        // old_handle.await could be used with error checking for cancellation.
                    }

                    if new_config_arc.health_check.enabled {
                        tracing::info!(
                            "Starting new health checker task with updated configuration..."
                        );

                        // Create HealthChecker directly instead of using utility function
                        let health_checker = HealthChecker::new(
                            new_proxy_service.clone(),
                            http_client_for_watcher.clone(),
                        );
                        let config_for_logging = new_config_arc.clone();

                        *handle_guard = Some(tokio::spawn(async move {
                            tracing::info!(
                                "File Reload health checker task started. Interval: {}s, Path: {}, Unhealthy Threshold: {}, Healthy Threshold: {}",
                                config_for_logging.health_check.interval_secs,
                                config_for_logging.health_check.path,
                                config_for_logging.health_check.unhealthy_threshold,
                                config_for_logging.health_check.healthy_threshold
                            );
                            if let Err(e) = health_checker.run().await {
                                tracing::error!("File Reload health checker run error: {}", e);
                            }
                        }));
                    } else {
                        tracing::info!(
                            "Health checking is disabled in the new configuration. Not starting health checker task."
                        );
                    }
                    tracing::info!(
                        "Configuration reloaded and health checker (if enabled) managed."
                    );
                }
                Err(e) => {
                    tracing::error!(
                        "Failed to reload configuration: {}. Keeping old configuration.",
                        e
                    );
                }
            }
            // Consume any other queued signals that might have arrived during processing to prevent immediate re-trigger.
            while notify_rx.try_recv().is_ok() {}
        }
        tracing::info!("File watcher task is shutting down.");
    });

    // Create graceful shutdown manager
    let graceful_shutdown = Arc::new(GracefulShutdown::new());

    // Start signal handler for graceful shutdown
    let signal_handler_shutdown = graceful_shutdown.clone();
    tokio::spawn(async move {
        if let Err(e) = signal_handler_shutdown.run_signal_handler().await {
            tracing::error!("Signal handler error: {}", e);
        }
    });

    // Create the unified server (supports HTTP/1.1, HTTP/2, and HTTP/3)
    let server = UnifiedServer::new(
        proxy_service_holder.clone(),
        config_holder.clone(),
        http_client.clone(),
        file_system.clone(),
        health_checker_handle_arc_mutex.clone(), // Pass the health checker handle
        graceful_shutdown.clone(),
    )
    .await?;

    // Log initial routes from the config_holder
    {
        let ch = config_holder.read().map_err(|e| {
            anyhow::anyhow!("Failed to acquire config read lock for logging: {}", e)
        })?;
        for (prefix, route) in &ch.routes {
            tracing::info!("Configured route: {} -> {:?}", prefix, route);
        }

        let protocols = &ch.protocols;
        tracing::info!(
            "Starting server on {} (TLS enabled: {}, HTTP/2: {}, HTTP/3: {}, WebSocket: {})",
            ch.listen_addr,
            ch.tls.is_some(),
            protocols.http2_enabled,
            protocols.http3_enabled,
            protocols.websocket_enabled
        );

        println!(
            "Server listening on {} (TLS: {}, HTTP/2: {}, HTTP/3: {}, WebSocket: {})",
            ch.listen_addr,
            ch.tls.is_some(),
            protocols.http2_enabled,
            protocols.http3_enabled,
            protocols.websocket_enabled
        );

        if protocols.http3_enabled {
            if let Some(h3_addr) = server.http3_local_addr() {
                tracing::info!("HTTP/3 server listening on UDP {}", h3_addr);
                println!("HTTP/3 server listening on UDP {}", h3_addr);
            }
        }
    }

    // Run the server and wait for shutdown
    let server_result = tokio::select! {
        result = server.run() => result,
        shutdown_reason = graceful_shutdown.wait_for_shutdown_signal() => {
            tracing::info!("Shutdown signal received: {:?}", shutdown_reason);

            // Cleanup health checker
            let mut handle_guard = health_checker_handle_arc_mutex.lock().await;
            if let Some(health_handle) = handle_guard.take() {
                tracing::info!("Shutting down health checker...");
                health_handle.abort();
            }

            tracing::info!("Graceful shutdown completed");
            Ok(())
        }
    };

    server_result?;

    // Shutdown tracing on exit
    tracing_setup::shutdown_tracing();

    Ok(())
}

/// Validate configuration file and exit
async fn validate_config_command(config_path: &str) -> Result<()> {
    use prox::config::loader::load_config_unchecked;
    use prox::config::validation::ConfigValidator;

    println!("🔍 Validating configuration file: {config_path}");

    // First check if file exists and is readable
    if !Path::new(config_path).exists() {
        eprintln!("❌ Error: Configuration file '{config_path}' not found");
        std::process::exit(1);
    }

    // Try to parse the YAML
    let config = match load_config_unchecked(config_path).await {
        Ok(config) => {
            println!("✅ YAML parsing: OK");
            config
        }
        Err(e) => {
            eprintln!("❌ YAML parsing failed:");
            eprintln!("   {e}");
            std::process::exit(1);
        }
    };

    // Validate the configuration
    match ConfigValidator::validate(&config) {
        Ok(()) => {
            println!("✅ Configuration validation: OK");
            println!();
            println!("📋 Configuration Summary:");
            println!("   • Listen Address: {}", config.listen_addr);
            println!("   • Routes: {}", config.routes.len());
            println!("   • TLS Enabled: {}", config.tls.is_some());
            println!("   • Health Checks: {}", config.health_check.enabled);
            println!();
            println!("🎉 Configuration is valid and ready to use!");
            Ok(())
        }
        Err(e) => {
            eprintln!("❌ Configuration validation failed:");
            eprintln!("{e}");
            println!();
            println!("💡 Common fixes:");
            println!("   • Ensure all URLs start with http:// or https://");
            println!("   • Check that file paths exist");
            println!("   • Verify listen address format (e.g., '127.0.0.1:3000')");
            println!("   • Ensure rate limit periods use valid units (s, m, h)");
            std::process::exit(1);
        }
    }
}

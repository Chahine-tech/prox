use std::sync::Arc;
use std::sync::RwLock; // Added for shared mutable state
use std::time::Duration; // Added for debounce

use anyhow::{Context, Result};
use clap::Parser;
use notify::{RecursiveMode, Watcher}; // Removed Watcher import, Added Watcher import
use std::path::Path;
use tokio::sync::{Mutex as TokioMutex, mpsc}; // Added for async mutex and channels // Added for path manipulation

// Import directly from crate root where they are re-exported
use prox::{
    // HealthChecker, // Removed unused import
    HyperHttpClient,
    HyperServer,
    ProxyService,
    TowerFileSystem,
    config::loader::load_config,
    // config::models::ServerConfig, // Removed unused import
    ports::http_server::HttpServer,
    utils::health_checker_utils::spawn_health_checker_task, // Import shared helper
};

#[derive(Parser, Debug)]
#[clap(author, version, about)]
struct Args {
    #[clap(short, long, default_value = "config.yaml")]
    config: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    tracing::info!("Loading initial configuration from {}", args.config);
    let initial_server_config_data = load_config(&args.config)
        .await
        .with_context(|| format!("Failed to load initial config from {}", args.config))?;

    let initial_config_arc = Arc::new(initial_server_config_data);
    let config_holder = Arc::new(RwLock::new(initial_config_arc.clone()));

    let http_client: Arc<HyperHttpClient> = Arc::new(HyperHttpClient::new());
    let file_system: Arc<TowerFileSystem> = Arc::new(TowerFileSystem::new());

    let initial_proxy_service = Arc::new(ProxyService::new(config_holder.read().unwrap().clone()));
    let proxy_service_holder = Arc::new(RwLock::new(initial_proxy_service.clone()));

    // Health Checker Management
    let health_checker_handle_arc_mutex =
        Arc::new(TokioMutex::new(None::<tokio::task::JoinHandle<()>>));

    {
        // Scope for initial health checker start
        let mut handle_guard = health_checker_handle_arc_mutex.lock().await;
        let current_config = config_holder.read().unwrap().clone();
        if current_config.health_check.enabled {
            tracing::info!("Starting initial health checker...");
            *handle_guard = Some(spawn_health_checker_task(
                // Use shared helper
                proxy_service_holder.read().unwrap().clone(),
                http_client.clone(),
                current_config.clone(),
                "Initial".to_string(), // Pass as String
            ));
        } else {
            tracing::info!("Initial configuration has health checking disabled.");
        }
    }

    // File Watcher Task
    let config_path_for_watcher = args.config.clone();
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
                    let new_config_arc = Arc::new(new_config_data);
                    tracing::info!("Successfully loaded new configuration.");

                    {
                        let mut config_w = config_holder_clone.write().unwrap();
                        *config_w = new_config_arc.clone();
                        tracing::info!("Global ServerConfig Arc updated.");
                    }

                    let new_proxy_service = Arc::new(ProxyService::new(new_config_arc.clone()));
                    {
                        let mut proxy_s_w = proxy_service_holder_clone.write().unwrap();
                        *proxy_s_w = new_proxy_service.clone();
                        tracing::info!("Global ProxyService Arc updated.");
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
                        *handle_guard = Some(spawn_health_checker_task(
                            // Use shared helper
                            new_proxy_service.clone(), // Use the new proxy service
                            http_client_for_watcher.clone(),
                            new_config_arc.clone(), // Pass the new config snapshot
                            "File Reload".to_string(), // Pass as String
                        ));
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

    // Create the HTTP server
    let server = HyperServer::with_dependencies(
        proxy_service_holder.clone(),
        config_holder.clone(),
        http_client.clone(),
        file_system.clone(),
        health_checker_handle_arc_mutex.clone(), // Pass the health checker handle
    );

    // Log initial routes from the config_holder
    {
        let ch = config_holder.read().unwrap();
        for (prefix, route) in &ch.routes {
            tracing::info!("Configured route: {} -> {:?}", prefix, route);
        }
        tracing::info!(
            "Starting server on {} (TLS enabled: {})",
            ch.listen_addr,
            ch.tls.is_some()
        );
        println!(
            "Server listening on {} (TLS enabled: {})",
            ch.listen_addr,
            ch.tls.is_some()
        );
    }

    server.run().await?;

    Ok(())
}

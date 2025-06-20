use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;
use futures_util::stream::StreamExt;
use signal_hook::consts::{SIGINT, SIGTERM, SIGUSR1};
use signal_hook_tokio::Signals;
use tokio::sync::broadcast;
use tokio::time::timeout;

/// Represents different shutdown reasons
#[derive(Debug, Clone)]
pub enum ShutdownReason {
    /// Graceful shutdown requested (SIGTERM, SIGINT)
    Graceful,
    /// Restart requested (SIGUSR1)
    Restart,
    /// Force shutdown (timeout exceeded)
    Force,
}

/// Manages graceful shutdown and restart functionality
pub struct GracefulShutdown {
    /// Broadcast sender for shutdown signals
    shutdown_tx: broadcast::Sender<ShutdownReason>,
    /// Flag indicating if shutdown has been initiated
    shutdown_initiated: Arc<AtomicBool>,
    /// Maximum time to wait for graceful shutdown
    shutdown_timeout: Duration,
}

impl GracefulShutdown {
    /// Create a new GracefulShutdown manager with default 30-second timeout
    pub fn new() -> Self {
        Self::with_timeout(Duration::from_secs(30))
    }

    /// Create a new GracefulShutdown manager with custom timeout
    pub fn with_timeout(shutdown_timeout: Duration) -> Self {
        let (shutdown_tx, _) = broadcast::channel(16);
        Self {
            shutdown_tx,
            shutdown_initiated: Arc::new(AtomicBool::new(false)),
            shutdown_timeout,
        }
    }

    /// Get a receiver for shutdown signals
    pub fn subscribe(&self) -> broadcast::Receiver<ShutdownReason> {
        self.shutdown_tx.subscribe()
    }

    /// Check if shutdown has been initiated
    pub fn is_shutdown_initiated(&self) -> bool {
        self.shutdown_initiated.load(Ordering::Relaxed)
    }

    /// Manually trigger shutdown (useful for API-triggered restarts)
    pub fn trigger_shutdown(&self, reason: ShutdownReason) -> Result<()> {
        if self
            .shutdown_initiated
            .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            tracing::info!("Shutdown manually triggered: {:?}", reason);
            let _ = self.shutdown_tx.send(reason);
        }
        Ok(())
    }

    /// Start listening for OS signals and manage shutdown process
    pub async fn run_signal_handler(&self) -> Result<()> {
        let mut signals = Signals::new([SIGTERM, SIGINT, SIGUSR1])?;
        let shutdown_tx = self.shutdown_tx.clone();
        let shutdown_initiated = self.shutdown_initiated.clone();

        tracing::info!(
            "Signal handler started. Listening for SIGTERM, SIGINT (graceful shutdown) and SIGUSR1 (restart)"
        );

        while let Some(signal) = signals.next().await {
            let reason = match signal {
                SIGTERM | SIGINT => {
                    tracing::info!(
                        "Received shutdown signal ({}), initiating graceful shutdown...",
                        if signal == SIGTERM {
                            "SIGTERM"
                        } else {
                            "SIGINT"
                        }
                    );
                    ShutdownReason::Graceful
                }
                SIGUSR1 => {
                    tracing::info!(
                        "Received restart signal (SIGUSR1), initiating graceful restart..."
                    );
                    ShutdownReason::Restart
                }
                _ => continue,
            };

            // Only handle the first signal, ignore subsequent ones
            if shutdown_initiated
                .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                tracing::info!("Processing shutdown signal: {:?}", reason);
                let _ = shutdown_tx.send(reason.clone());

                // For restart signals, we might want to continue listening
                // but for now, we'll break and let the application handle the restart logic
                if matches!(reason, ShutdownReason::Graceful) {
                    break;
                }
            } else {
                tracing::warn!("Shutdown already in progress, ignoring additional signal");
            }
        }

        tracing::info!("Signal handler shutting down");
        Ok(())
    }

    /// Wait for shutdown with timeout, returns the reason for shutdown
    pub async fn wait_for_shutdown(&self) -> ShutdownReason {
        let mut receiver = self.subscribe();

        match timeout(self.shutdown_timeout, receiver.recv()).await {
            Ok(Ok(reason)) => {
                tracing::info!("Shutdown signal received: {:?}", reason);
                reason
            }
            Ok(Err(_)) => {
                tracing::warn!("Shutdown channel closed unexpectedly");
                ShutdownReason::Force
            }
            Err(_) => {
                tracing::error!(
                    "Shutdown timeout exceeded ({:?}), forcing shutdown",
                    self.shutdown_timeout
                );
                ShutdownReason::Force
            }
        }
    }

    /// Wait indefinitely for shutdown signal (used in main application loop)
    pub async fn wait_for_shutdown_signal(&self) -> ShutdownReason {
        let mut receiver = self.subscribe();

        match receiver.recv().await {
            Ok(reason) => {
                tracing::info!("Shutdown signal received: {:?}", reason);
                reason
            }
            Err(_) => {
                tracing::warn!("Shutdown channel closed unexpectedly");
                ShutdownReason::Force
            }
        }
    }

    /// Create a shutdown token that can be used to cancel operations
    pub fn shutdown_token(&self) -> ShutdownToken {
        ShutdownToken {
            receiver: self.subscribe(),
            shutdown_initiated: self.shutdown_initiated.clone(),
        }
    }
}

impl Default for GracefulShutdown {
    fn default() -> Self {
        Self::new()
    }
}

/// A token that can be used to check for shutdown signals
pub struct ShutdownToken {
    receiver: broadcast::Receiver<ShutdownReason>,
    shutdown_initiated: Arc<AtomicBool>,
}

impl Clone for ShutdownToken {
    fn clone(&self) -> Self {
        Self {
            receiver: self.receiver.resubscribe(),
            shutdown_initiated: self.shutdown_initiated.clone(),
        }
    }
}

impl ShutdownToken {
    /// Check if shutdown has been initiated (non-blocking)
    pub fn is_shutdown_requested(&self) -> bool {
        self.shutdown_initiated.load(Ordering::Relaxed)
    }

    /// Wait for shutdown signal (blocking)
    pub async fn cancelled(&mut self) -> ShutdownReason {
        match self.receiver.recv().await {
            Ok(reason) => reason,
            Err(_) => ShutdownReason::Force,
        }
    }

    /// Try to receive shutdown signal without blocking
    pub fn try_recv(&mut self) -> Option<ShutdownReason> {
        match self.receiver.try_recv() {
            Ok(reason) => Some(reason),
            Err(broadcast::error::TryRecvError::Empty) => None,
            Err(_) => Some(ShutdownReason::Force),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_manual_shutdown_trigger() {
        let shutdown = GracefulShutdown::new();
        let mut receiver = shutdown.subscribe();

        // Trigger shutdown manually
        shutdown.trigger_shutdown(ShutdownReason::Graceful).unwrap();

        // Should receive the signal
        let reason = receiver.recv().await.unwrap();
        assert!(matches!(reason, ShutdownReason::Graceful));
        assert!(shutdown.is_shutdown_initiated());
    }

    #[tokio::test]
    async fn test_shutdown_token() {
        let shutdown = GracefulShutdown::new();
        let mut token = shutdown.shutdown_token();

        assert!(!token.is_shutdown_requested());
        assert!(token.try_recv().is_none());

        shutdown.trigger_shutdown(ShutdownReason::Restart).unwrap();

        assert!(token.is_shutdown_requested());
        let reason = token.try_recv().unwrap();
        assert!(matches!(reason, ShutdownReason::Restart));
    }

    #[tokio::test]
    async fn test_timeout_shutdown() {
        let shutdown = GracefulShutdown::with_timeout(Duration::from_millis(100));

        let start = std::time::Instant::now();
        let reason = shutdown.wait_for_shutdown().await;
        let elapsed = start.elapsed();

        assert!(matches!(reason, ShutdownReason::Force));
        assert!(elapsed >= Duration::from_millis(100));
        assert!(elapsed < Duration::from_millis(200)); // Should not take too much longer
    }
}

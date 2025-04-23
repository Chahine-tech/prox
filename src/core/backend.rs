use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};
use crate::config::HealthStatus;

#[derive(Debug)]
pub struct BackendHealth {
    status: AtomicU8, // 0 = Unhealthy, 1 = Healthy
    pub consecutive_successes: AtomicU32,
    pub consecutive_failures: AtomicU32,
}

impl BackendHealth {
    pub fn new(_target: String) -> Self {
        Self {
            status: AtomicU8::new(1), // Start as healthy
            consecutive_successes: AtomicU32::new(0),
            consecutive_failures: AtomicU32::new(0),
        }
    }

    pub fn status(&self) -> HealthStatus {
        if self.status.load(Ordering::Relaxed) == 1 {
            HealthStatus::Healthy
        } else {
            HealthStatus::Unhealthy
        }
    }

    pub fn mark_healthy(&self) {
        self.status.store(1, Ordering::Relaxed);
        self.consecutive_failures.store(0, Ordering::Relaxed);
    }

    pub fn mark_unhealthy(&self) {
        self.status.store(0, Ordering::Relaxed);
        self.consecutive_successes.store(0, Ordering::Relaxed);
    }
}
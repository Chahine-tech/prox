use std::sync::atomic::{AtomicUsize, Ordering};
use rand::Rng;

/// Trait defining the interface for load balancing strategies
pub trait LoadBalancingStrategy: Send + Sync + 'static {
    /// Select a target from a list of targets
    fn select_target(&self, targets: &[String]) -> Option<String>;
    
    /// Create a new instance of this strategy as a boxed trait object
    fn boxed(self) -> Box<dyn LoadBalancingStrategy>
    where
        Self: Sized,
    {
        Box::new(self)
    }
}

/// Round-robin load balancing strategy
pub struct RoundRobinStrategy {
    counter: AtomicUsize,
}

impl RoundRobinStrategy {
    /// Create a new round-robin strategy
    pub fn new() -> Self {
        Self {
            counter: AtomicUsize::new(0),
        }
    }
}

impl LoadBalancingStrategy for RoundRobinStrategy {
    fn select_target(&self, targets: &[String]) -> Option<String> {
        if targets.is_empty() {
            return None;
        }
        
        // Atomically increment and get the counter value
        let count = self.counter.fetch_add(1, Ordering::SeqCst);
        
        // Calculate index with remainder to cycle through targets
        Some(targets[count % targets.len()].clone())
    }
}

/// Random selection load balancing strategy
pub struct RandomStrategy;

impl RandomStrategy {
    /// Create a new random selection strategy
    pub fn new() -> Self {
        Self
    }
}

impl LoadBalancingStrategy for RandomStrategy {
    fn select_target(&self, targets: &[String]) -> Option<String> {
        if targets.is_empty() {
            return None;
        }
        
        // Use rng().random_range directly
        let index = rand::rng().random_range(0..targets.len());
        Some(targets[index].clone())
    }
}

/// Factory for creating load balancing strategies from configuration
pub struct LoadBalancerFactory;

impl LoadBalancerFactory {
    /// Create a new load balancing strategy based on configuration
    pub fn create_strategy(strategy: &crate::config::LoadBalanceStrategy) -> Box<dyn LoadBalancingStrategy> {
        match strategy {
            crate::config::LoadBalanceStrategy::RoundRobin => RoundRobinStrategy::new().boxed(),
            crate::config::LoadBalanceStrategy::Random => RandomStrategy::new().boxed(),
        }
    }
}
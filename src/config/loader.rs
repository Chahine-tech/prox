use std::path::Path;
use thiserror::Error;
use tokio::fs;

use crate::config::models::ServerConfig;
use crate::config::validation::{ConfigValidator, ValidationError};

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("Failed to read config file: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Failed to parse YAML config: {0}")]
    ParseError(#[from] serde_yaml::Error),
    
    #[error("Configuration validation failed: {0}")]
    ValidationError(#[from] ValidationError),
}

pub type ConfigResult<T> = std::result::Result<T, ConfigError>;

/// Load and validate configuration from file
pub async fn load_config<P: AsRef<Path>>(path: P) -> ConfigResult<ServerConfig> {
    let config_content = fs::read_to_string(path).await?;
    let config: ServerConfig = serde_yaml::from_str(&config_content)?;
    
    // Validate the configuration
    ConfigValidator::validate(&config)?;
    
    Ok(config)
}

/// Load configuration from file without validation (for testing)
pub async fn load_config_unchecked<P: AsRef<Path>>(path: P) -> ConfigResult<ServerConfig> {
    let config_content = fs::read_to_string(path).await?;
    let config: ServerConfig = serde_yaml::from_str(&config_content)?;
    Ok(config)
}

/// Validate configuration without loading from file
pub fn validate_config(config: &ServerConfig) -> ConfigResult<()> {
    ConfigValidator::validate(config)?;
    Ok(())
}

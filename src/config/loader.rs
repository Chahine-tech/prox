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

pub async fn load_config<P: AsRef<Path>>(path: P) -> ConfigResult<ServerConfig> {
    let config_content = fs::read_to_string(path).await?;
    let config: ServerConfig = serde_yaml::from_str(&config_content)?;
    ConfigValidator::validate(&config)?;
    Ok(config)
}

pub async fn load_config_unchecked<P: AsRef<Path>>(path: P) -> ConfigResult<ServerConfig> {
    let config_content = fs::read_to_string(path).await?;
    let config: ServerConfig = serde_yaml::from_str(&config_content)?;
    Ok(config)
}

pub fn validate_config(config: &ServerConfig) -> ConfigResult<()> {
    ConfigValidator::validate(config)?;
    Ok(())
}

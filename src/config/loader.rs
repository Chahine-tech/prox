use anyhow::Result;
use std::path::Path;
use tokio::fs;

use crate::config::models::ServerConfig;

pub async fn load_config<P: AsRef<Path>>(path: P) -> Result<ServerConfig> {
    let config_content = fs::read_to_string(path).await?;
    let config: ServerConfig = serde_yaml::from_str(&config_content)?;
    Ok(config)
}
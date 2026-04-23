use crate::error::Result;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Deserialize, Default, Clone)]
#[allow(dead_code)]
pub struct Config {
    #[serde(default)]
    pub daemon: DaemonConfig,

    #[serde(default)]
    pub triggers: HashMap<String, TriggerConfig>,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct DaemonConfig {
    #[serde(default = "default_idle_timeout")]
    pub idle_timeout_sec: u64,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            idle_timeout_sec: default_idle_timeout(),
        }
    }
}

fn default_idle_timeout() -> u64 {
    300 // 5 minutes
}

#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "type")]
#[allow(dead_code)]
pub enum TriggerConfig {
    #[serde(rename = "periodic")]
    Periodic { interval_sec: u64, command: String },
    #[serde(rename = "file-change")]
    FileChange { watch: String, command: String },
    #[serde(rename = "explicit")]
    Explicit { command: String },
}

impl Config {
    pub fn load() -> Result<Self> {
        let home = std::env::var("HOME")?;
        let config_path = PathBuf::from(home)
            .join(".config")
            .join("shbot")
            .join("shbot.toml");

        if !config_path.exists() {
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(&config_path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }
}

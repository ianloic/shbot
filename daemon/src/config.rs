// Copyright (C) 2026 Ian McKellar
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_config_default_fallback() {
        // Set HOME to a temporary directory without a config file
        let temp_home = tempdir().unwrap();
        temp_env::with_var("HOME", Some(temp_home.path()), || {
            let config = Config::load().expect("Should fall back to default when no file exists");
            assert_eq!(config.daemon.idle_timeout_sec, 300);
            assert!(config.triggers.is_empty());
        });
    }

    #[test]
    fn test_config_parsing() {
        let temp_home = tempdir().unwrap();

        let config_dir = temp_home.path().join(".config").join("shbot");
        fs::create_dir_all(&config_dir).unwrap();
        let config_path = config_dir.join("shbot.toml");

        let toml_content = r#"
[daemon]
idle_timeout_sec = 600

[triggers.my_periodic]
type = "periodic"
interval_sec = 10
command = "echo 'hello'"

[triggers.my_file]
type = "file-change"
watch = "**/*.rs"
command = "cargo test"

[triggers.my_explicit]
type = "explicit"
command = "git status"
        "#;

        fs::write(config_path, toml_content).unwrap();

        temp_env::with_var("HOME", Some(temp_home.path()), || {
            let config = Config::load().expect("Should load the config successfully");
            assert_eq!(config.daemon.idle_timeout_sec, 600);
            assert_eq!(config.triggers.len(), 3);

            if let Some(TriggerConfig::Periodic {
                interval_sec,
                command,
            }) = config.triggers.get("my_periodic")
            {
                assert_eq!(*interval_sec, 10);
                assert_eq!(command, "echo 'hello'");
            } else {
                panic!("Expected periodic trigger");
            }

            if let Some(TriggerConfig::FileChange { watch, command }) =
                config.triggers.get("my_file")
            {
                assert_eq!(watch, "**/*.rs");
                assert_eq!(command, "cargo test");
            } else {
                panic!("Expected file-change trigger");
            }

            if let Some(TriggerConfig::Explicit { command }) = config.triggers.get("my_explicit") {
                assert_eq!(command, "git status");
            } else {
                panic!("Expected explicit trigger");
            }
        });
    }
}

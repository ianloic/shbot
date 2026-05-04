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

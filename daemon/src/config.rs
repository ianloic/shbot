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
use crate::scope::Scope;
use globset::{Glob, GlobBuilder, GlobMatcher, GlobSet, GlobSetBuilder};
use serde::Deserialize;
use std::borrow::Borrow;
use std::collections::HashMap;
use std::env;
use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;
use tracing::error;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(transparent)]
pub struct StateId(pub String);

impl StateId {
    pub fn git() -> Self {
        StateId("git".to_string())
    }
}

impl fmt::Display for StateId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<&str> for StateId {
    fn from(s: &str) -> Self {
        StateId(s.to_string())
    }
}

impl From<String> for StateId {
    fn from(s: String) -> Self {
        StateId(s)
    }
}

impl Borrow<str> for StateId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for StateId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct Config {
    #[serde(default)]
    pub daemon: DaemonConfig,

    #[serde(default)]
    pub state: HashMap<StateId, StateConfig>,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct DaemonConfig {
    #[serde(default = "default_idle_timeout")]
    pub idle_timeout_sec: u64,
    #[serde(default = "default_command_timeout")]
    pub command_timeout_sec: u64,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            idle_timeout_sec: default_idle_timeout(),
            command_timeout_sec: default_command_timeout(),
        }
    }
}

fn default_idle_timeout() -> u64 {
    300 // 5 minutes
}

fn default_command_timeout() -> u64 {
    60 // 1 minute
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct StateConfig {
    #[serde(default)]
    pub scope: Scope,
    #[serde(flatten)]
    pub source: StateSource,
    #[serde(flatten)]
    pub invalidate: StateInvalidate,
}

impl StateConfig {
    pub fn watch_pattern(&self) -> Option<&str> {
        if let Some(w) = &self.invalidate.watch {
            Some(w.as_str())
        } else if let StateSource::File { file } = &self.source {
            Some(file.as_str())
        } else {
            None
        }
    }

    pub fn watch_matcher(&self, name: &StateId) -> Option<&GlobMatcher> {
        let pattern = self.watch_pattern()?;
        self.invalidate
            .compiled_watch
            .get_or_init(|| match GlobBuilder::new(pattern).build() {
                Ok(g) => Some(g.compile_matcher()),
                Err(e) => {
                    error!(
                        "Invalid glob pattern '{}' for state '{}': {}",
                        pattern, name, e
                    );
                    None
                }
            })
            .as_ref()
    }

    pub fn ignore_set(&self) -> Option<&GlobSet> {
        self.invalidate
            .compiled_ignore
            .get_or_init(|| {
                let ignore_patterns = self.invalidate.ignore.as_ref()?;
                let mut builder = GlobSetBuilder::new();
                for pattern in ignore_patterns {
                    if let Ok(g) = Glob::new(pattern) {
                        builder.add(g);
                    }
                }
                builder.build().ok()
            })
            .as_ref()
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum StateSource {
    File { file: String },
    Command { command: String },
    Native { native: String },
}

#[derive(Debug, Deserialize, Clone, Default)]
#[allow(dead_code)]
pub struct StateInvalidate {
    #[serde(default)]
    pub watch: Option<String>,
    #[serde(default)]
    pub ignore: Option<Vec<String>>,
    #[serde(default)]
    pub timeout: Option<u64>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub pwd: Option<bool>,
    #[serde(skip)]
    pub compiled_watch: OnceLock<Option<GlobMatcher>>,
    #[serde(skip)]
    pub compiled_ignore: OnceLock<Option<GlobSet>>,
}

impl Config {
    pub fn load() -> Result<Self> {
        let home = env::var("HOME")?;
        let config_path = PathBuf::from(home)
            .join(".config")
            .join("shbot")
            .join("shbot.toml");

        let mut config: Config = if !config_path.exists() {
            Self::default()
        } else {
            let content = fs::read_to_string(&config_path)?;
            toml::from_str(&content)?
        };
        config.inject_implicit_states();
        Ok(config)
    }

    pub fn inject_implicit_states(&mut self) {
        if !self.state.contains_key("git") {
            self.state.insert(
                StateId::git(),
                StateConfig {
                    scope: Scope::Nearest(vec![".git".to_string()]),
                    source: StateSource::Native {
                        native: "git".to_string(),
                    },
                    invalidate: StateInvalidate {
                        watch: Some("**/*".to_string()),
                        ignore: Some(vec![
                            "**/.wine/**".to_string(),
                            "**/.cache/**".to_string(),
                            "**/.cargo/**".to_string(),
                            "**/.rustup/**".to_string(),
                            "**/.npm/**".to_string(),
                            "**/.nvm/**".to_string(),
                            "**/.git/objects/**".to_string(),
                        ]),
                        pwd: Some(true),
                        ..Default::default()
                    },
                },
            );
        }
        for (name, state_config) in &self.state {
            let _ = state_config.watch_matcher(name);
            let _ = state_config.ignore_set();
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        let mut config = Self {
            daemon: DaemonConfig::default(),
            state: HashMap::new(),
        };
        config.inject_implicit_states();
        config
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
            assert_eq!(config.daemon.command_timeout_sec, 60);
            assert_eq!(config.state.len(), 1);
            assert!(config.state.contains_key("git"));
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

[state.my_periodic]
command = "echo 'hello'"
timeout = 10

[state.my_file]
command = "cargo test"
watch = "**/*.rs"

[state.my_explicit]
command = "git status"

[state.my_file_source]
file = "/etc/os-release"
        "#;

        fs::write(config_path, toml_content).unwrap();

        temp_env::with_var("HOME", Some(temp_home.path()), || {
            let config = Config::load().expect("Should load the config successfully");
            assert_eq!(config.daemon.idle_timeout_sec, 600);
            assert_eq!(config.state.len(), 5);
            assert!(config.state.contains_key("git"));

            let periodic = config
                .state
                .get("my_periodic")
                .expect("Expected periodic state");
            assert_eq!(periodic.invalidate.timeout, Some(10));
            if let StateSource::Command { command } = &periodic.source {
                assert_eq!(command, "echo 'hello'");
            } else {
                panic!("Expected command source");
            }

            let file_change = config
                .state
                .get("my_file")
                .expect("Expected file-change state");
            assert_eq!(file_change.invalidate.watch.as_deref(), Some("**/*.rs"));
            if let StateSource::Command { command } = &file_change.source {
                assert_eq!(command, "cargo test");
            } else {
                panic!("Expected command source");
            }

            let explicit = config
                .state
                .get("my_explicit")
                .expect("Expected explicit state");
            if let StateSource::Command { command } = &explicit.source {
                assert_eq!(command, "git status");
            } else {
                panic!("Expected command source");
            }

            let file_source = config
                .state
                .get("my_file_source")
                .expect("Expected file source state");
            if let StateSource::File { file } = &file_source.source {
                assert_eq!(file, "/etc/os-release");
            } else {
                panic!("Expected file source");
            }
        });
    }
}

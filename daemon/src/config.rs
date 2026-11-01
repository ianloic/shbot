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
use serde::{Deserialize, Deserializer};
use std::borrow::Borrow;
use std::collections::HashMap;
use std::env;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::result::Result as StdResult;
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

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct StateConfig {
    pub scope: Scope,
    pub source: StateSource,
    pub invalidate: StateInvalidate,
}

#[derive(Deserialize)]
struct RawStateConfig {
    #[serde(default)]
    scope: Scope,
    source: Option<StateSource>,
    file: Option<String>,
    command: Option<String>,
    native: Option<String>,
    invalidate: Option<StateInvalidate>,
    watch: Option<String>,
    ignore: Option<Vec<String>>,
    timeout: Option<u64>,
    pwd: Option<bool>,
}

impl<'de> Deserialize<'de> for StateConfig {
    fn deserialize<D>(deserializer: D) -> StdResult<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawStateConfig::deserialize(deserializer)?;
        let source = if let Some(src) = raw.source {
            src
        } else if let Some(file) = raw.file {
            StateSource::File { file }
        } else if let Some(command) = raw.command {
            StateSource::Command { command }
        } else if let Some(native) = raw.native {
            StateSource::Native { native }
        } else {
            return Err(serde::de::Error::custom(
                "missing state source (expected `command`, `file`, `native`, or `source.*`)",
            ));
        };

        let mut invalidate = raw.invalidate.unwrap_or_default();
        if invalidate.watch.is_none() {
            invalidate.watch = raw.watch;
        }
        if invalidate.ignore.is_none() {
            invalidate.ignore = raw.ignore;
        }
        if invalidate.timeout.is_none() {
            invalidate.timeout = raw.timeout;
        }
        if invalidate.pwd.is_none() {
            invalidate.pwd = raw.pwd;
        }

        Ok(StateConfig {
            scope: raw.scope,
            source,
            invalidate,
        })
    }
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

    pub fn should_watch(&self) -> bool {
        self.watch_pattern().is_some() || self.invalidate.pwd.unwrap_or(false)
    }

    pub fn evaluate_scope(&self, start_dir: &Path) -> Option<PathBuf> {
        if start_dir.as_os_str().is_empty() {
            return None;
        }
        match &self.scope {
            Scope::Global => {
                if let StateSource::File { file } = &self.source {
                    let file_path = Path::new(file);
                    if file_path.is_absolute() {
                        return file_path
                            .parent()
                            .map(Path::to_path_buf)
                            .or_else(|| Some(PathBuf::from("/")));
                    }
                }
                if self.should_watch() {
                    Some(start_dir.to_path_buf())
                } else {
                    Some(PathBuf::from("/"))
                }
            }
            other => other.evaluate(start_dir),
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

    #[test]
    fn test_state_id_traits_and_nested_config_and_evaluate_scope() {
        let id = StateId::from("custom".to_string());
        assert_eq!(id.0.as_str(), "custom");
        assert_eq!(id.as_ref(), "custom");
        let borrowed: &str = Borrow::borrow(&id);
        assert_eq!(borrowed, "custom");
        assert_eq!(format!("{}", id), "custom");

        let nested_toml = r#"
[state.git]
scope = "cwd"
source = { native = "git" }
invalidate = { watch = "**/*.rs", ignore = ["target/**", "["], timeout = 5, pwd = false }

[state.nested_cmd]
source.command = "echo hi"
invalidate.timeout = 30

[state.nested_file]
source.file = "VERSION"
ignore = ["ignored/**"]
pwd = true
"#;
        let mut config: Config = toml::from_str(nested_toml).unwrap();
        config.inject_implicit_states();

        let git_sc = config.state.get("git").unwrap();
        assert_eq!(git_sc.scope, Scope::Cwd);
        assert_eq!(git_sc.invalidate.timeout, Some(5));
        assert_eq!(git_sc.invalidate.pwd, Some(false));
        assert!(git_sc.should_watch());
        assert!(git_sc.ignore_set().is_some());

        let nested_cmd = config.state.get("nested_cmd").unwrap();
        assert!(!nested_cmd.should_watch());
        assert_eq!(
            nested_cmd.evaluate_scope(Path::new("")),
            None,
            "empty start_dir must return None"
        );
        assert_eq!(
            nested_cmd.evaluate_scope(Path::new("/home/user/proj")),
            Some(PathBuf::from("/")),
            "global non-watch state scopes to /"
        );

        let nested_file = config.state.get("nested_file").unwrap();
        assert!(nested_file.should_watch());
        assert_eq!(
            nested_file.evaluate_scope(Path::new("/home/user/proj")),
            Some(PathBuf::from("/home/user/proj")),
            "global relative file state scopes to start_dir"
        );

        let abs_file = StateConfig {
            scope: Scope::Global,
            source: StateSource::File {
                file: "/etc/hostname".to_string(),
            },
            invalidate: StateInvalidate::default(),
        };
        assert_eq!(
            abs_file.evaluate_scope(Path::new("/home/user/proj")),
            Some(PathBuf::from("/etc")),
            "global absolute file state scopes to file parent directory"
        );

        let missing_source_toml = r#"
[state.bad]
timeout = 10
"#;
        let err = toml::from_str::<Config>(missing_source_toml).unwrap_err();
        assert!(err.to_string().contains("missing state source"));
    }
}

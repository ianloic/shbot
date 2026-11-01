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

use serde::{Deserialize, Deserializer};
use std::path::{Path, PathBuf};
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    Global,
    Cwd,
    Nearest(Vec<String>),
    Topmost(Vec<String>),
}

impl Default for Scope {
    fn default() -> Self {
        Scope::Global
    }
}

impl FromStr for Scope {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s == "global" {
            return Ok(Scope::Global);
        }
        if s == "cwd" {
            return Ok(Scope::Cwd);
        }

        if let Some(rest) = s.strip_prefix("nearest(") {
            if let Some(inner) = rest.strip_suffix(')') {
                let markers: Vec<String> = inner
                    .split('|')
                    .map(|m| m.trim().to_string())
                    .filter(|m| !m.is_empty())
                    .collect();
                if markers.is_empty() {
                    return Err("nearest() requires at least one marker".to_string());
                }
                return Ok(Scope::Nearest(markers));
            }
        }

        if let Some(rest) = s.strip_prefix("topmost(") {
            if let Some(inner) = rest.strip_suffix(')') {
                let markers: Vec<String> = inner
                    .split('|')
                    .map(|m| m.trim().to_string())
                    .filter(|m| !m.is_empty())
                    .collect();
                if markers.is_empty() {
                    return Err("topmost() requires at least one marker".to_string());
                }
                return Ok(Scope::Topmost(markers));
            }
        }

        Err(format!("Invalid scope format: '{}'", s))
    }
}

impl<'de> Deserialize<'de> for Scope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Scope::from_str(&s).map_err(serde::de::Error::custom)
    }
}

impl Scope {
    pub fn evaluate(&self, start_dir: &Path) -> Option<PathBuf> {
        match self {
            Scope::Global => None,
            Scope::Cwd => Some(start_dir.to_path_buf()),
            Scope::Nearest(markers) => {
                let mut current = start_dir;
                let mut probe = PathBuf::with_capacity(start_dir.as_os_str().len() + 32);
                loop {
                    for marker in markers {
                        probe.clear();
                        probe.push(current);
                        probe.push(marker);
                        if probe.exists() {
                            return Some(current.to_path_buf());
                        }
                    }
                    if let Some(parent) = current.parent() {
                        current = parent;
                    } else {
                        break;
                    }
                }
                None
            }
            Scope::Topmost(markers) => {
                let mut current = start_dir;
                let mut topmost_match = None;
                let mut probe = PathBuf::with_capacity(start_dir.as_os_str().len() + 32);
                loop {
                    for marker in markers {
                        probe.clear();
                        probe.push(current);
                        probe.push(marker);
                        if probe.exists() {
                            topmost_match = Some(current.to_path_buf());
                            break;
                        }
                    }
                    if let Some(parent) = current.parent() {
                        current = parent;
                    } else {
                        break;
                    }
                }
                topmost_match
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_scope_parsing() {
        assert_eq!(Scope::from_str("global").unwrap(), Scope::Global);
        assert_eq!(Scope::from_str("cwd").unwrap(), Scope::Cwd);
        assert_eq!(
            Scope::from_str("nearest(.git)").unwrap(),
            Scope::Nearest(vec![".git".to_string()])
        );
        assert_eq!(
            Scope::from_str("nearest(.git | .hg)").unwrap(),
            Scope::Nearest(vec![".git".to_string(), ".hg".to_string()])
        );
        assert_eq!(
            Scope::from_str("topmost(Cargo.toml)").unwrap(),
            Scope::Topmost(vec!["Cargo.toml".to_string()])
        );

        assert!(Scope::from_str("unknown").is_err());
        assert!(Scope::from_str("nearest()").is_err());
        assert!(Scope::from_str("nearest(.git").is_err());
        assert!(Scope::from_str("topmost(   )").is_err());
        assert!(Scope::from_str("topmost(.git").is_err());
    }

    #[test]
    fn test_scope_evaluate_global_cwd() {
        let dir = tempdir().unwrap();
        let path = dir.path();

        let global = Scope::Global;
        assert_eq!(global.evaluate(path), None);

        let cwd = Scope::Cwd;
        assert_eq!(cwd.evaluate(path), Some(path.to_path_buf()));
    }

    #[test]
    fn test_scope_evaluate_nearest() {
        let root = tempdir().unwrap();
        let project = root.path().join("project");
        let sub = project.join("sub");
        let deep = sub.join("deep");

        fs::create_dir_all(&deep).unwrap();

        // Create a marker in project
        fs::write(project.join(".git"), "").unwrap();

        let scope = Scope::Nearest(vec![".git".to_string()]);

        // Start from deep, should find it in project
        assert_eq!(scope.evaluate(&deep), Some(project.clone()));

        // Start from sub, should find it in project
        assert_eq!(scope.evaluate(&sub), Some(project.clone()));

        // Start from root, shouldn't find it, returns None
        assert_eq!(scope.evaluate(root.path()), None);

        // Create another marker in deep
        fs::write(deep.join(".git"), "").unwrap();

        // Now nearest from deep should be deep
        assert_eq!(scope.evaluate(&deep), Some(deep.clone()));
    }

    #[test]
    fn test_scope_evaluate_topmost() {
        let root = tempdir().unwrap();
        let project = root.path().join("project");
        let sub = project.join("sub");
        let deep = sub.join("deep");

        fs::create_dir_all(&deep).unwrap();

        // Create a marker in project
        fs::write(project.join(".git"), "").unwrap();

        let scope = Scope::Topmost(vec![".git".to_string()]);

        // Topmost from deep should find project
        assert_eq!(scope.evaluate(&deep), Some(project.clone()));

        // Create a marker in deep too
        fs::write(deep.join(".git"), "").unwrap();

        // Topmost should STILL find project, even though deep has one
        assert_eq!(scope.evaluate(&deep), Some(project.clone()));
    }

    #[test]
    fn test_scope_deserialize() {
        use serde::de::IntoDeserializer;
        use serde::de::value::{Error as ValueError, StringDeserializer};

        let deserializer: StringDeserializer<ValueError> =
            String::from("nearest(.git | .hg)").into_deserializer();
        let scope = Scope::deserialize(deserializer).unwrap();

        assert_eq!(
            scope,
            Scope::Nearest(vec![".git".to_string(), ".hg".to_string()])
        );
    }
}

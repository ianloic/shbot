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

use std::fmt;

#[derive(Debug)]
pub enum ShbotError {
    Io(std::io::Error),
    Notify(notify::Error),
    TomlSerDe(toml::de::Error),
    Git2(git2::Error),
    EnvVar(std::env::VarError),
    TracingSub(tracing::subscriber::SetGlobalDefaultError),
    Custom(String),
}

pub type Result<T> = std::result::Result<T, ShbotError>;

impl fmt::Display for ShbotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ShbotError::Io(e) => write!(f, "IO Error: {}", e),
            ShbotError::Notify(e) => write!(f, "Watcher Error: {}", e),
            ShbotError::TomlSerDe(e) => write!(f, "TOML parse error: {}", e),
            ShbotError::Git2(e) => write!(f, "Git error: {}", e),
            ShbotError::EnvVar(e) => write!(f, "Environment variable error: {}", e),
            ShbotError::TracingSub(e) => write!(f, "Tracing error: {}", e),
            ShbotError::Custom(m) => write!(f, "{}", m),
        }
    }
}

impl std::error::Error for ShbotError {}

impl From<std::io::Error> for ShbotError {
    fn from(err: std::io::Error) -> Self {
        ShbotError::Io(err)
    }
}

impl From<notify::Error> for ShbotError {
    fn from(err: notify::Error) -> Self {
        ShbotError::Notify(err)
    }
}

impl From<toml::de::Error> for ShbotError {
    fn from(err: toml::de::Error) -> Self {
        ShbotError::TomlSerDe(err)
    }
}

impl From<git2::Error> for ShbotError {
    fn from(err: git2::Error) -> Self {
        ShbotError::Git2(err)
    }
}

impl From<std::env::VarError> for ShbotError {
    fn from(err: std::env::VarError) -> Self {
        ShbotError::EnvVar(err)
    }
}

impl From<tracing::subscriber::SetGlobalDefaultError> for ShbotError {
    fn from(err: tracing::subscriber::SetGlobalDefaultError) -> Self {
        ShbotError::TracingSub(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err = ShbotError::Io(io_err);
        assert_eq!(err.to_string(), "IO Error: file not found");

        let custom_err = ShbotError::Custom("custom error".to_string());
        assert_eq!(custom_err.to_string(), "custom error");

        let env_err = std::env::VarError::NotPresent;
        let err2 = ShbotError::EnvVar(env_err);
        assert_eq!(err2.to_string(), "Environment variable error: environment variable not found");
    }

    #[test]
    fn test_error_from() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "not found");
        let err: ShbotError = io_err.into();
        assert!(matches!(err, ShbotError::Io(_)));

        let env_err = std::env::VarError::NotPresent;
        let err2: ShbotError = env_err.into();
        assert!(matches!(err2, ShbotError::EnvVar(_)));
        
        let git_err = git2::Error::from_str("git error");
        let err3: ShbotError = git_err.into();
        assert!(matches!(err3, ShbotError::Git2(_)));
    }
}

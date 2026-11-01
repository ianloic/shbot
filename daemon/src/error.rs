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

use std::env::VarError;
use std::error::Error as StdError;
use std::fmt;
use std::io::Error as IoError;
use std::result::Result as StdResult;
use toml::de::Error as TomlDeError;
use tracing::subscriber::SetGlobalDefaultError;

#[derive(Debug)]
pub enum ShbotError {
    Io(IoError),
    Notify(notify::Error),
    TomlSerDe(TomlDeError),
    EnvVar(VarError),
    TracingSub(SetGlobalDefaultError),
    Custom(String),
}

pub type Result<T> = StdResult<T, ShbotError>;

impl fmt::Display for ShbotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ShbotError::Io(e) => write!(f, "IO Error: {}", e),
            ShbotError::Notify(e) => write!(f, "Watcher Error: {}", e),
            ShbotError::TomlSerDe(e) => write!(f, "TOML parse error: {}", e),
            ShbotError::EnvVar(e) => write!(f, "Environment variable error: {}", e),
            ShbotError::TracingSub(e) => write!(f, "Tracing error: {}", e),
            ShbotError::Custom(m) => write!(f, "{}", m),
        }
    }
}

impl StdError for ShbotError {}

impl From<IoError> for ShbotError {
    fn from(err: IoError) -> Self {
        ShbotError::Io(err)
    }
}

impl From<notify::Error> for ShbotError {
    fn from(err: notify::Error) -> Self {
        ShbotError::Notify(err)
    }
}

impl From<TomlDeError> for ShbotError {
    fn from(err: TomlDeError) -> Self {
        ShbotError::TomlSerDe(err)
    }
}

impl From<VarError> for ShbotError {
    fn from(err: VarError) -> Self {
        ShbotError::EnvVar(err)
    }
}

impl From<SetGlobalDefaultError> for ShbotError {
    fn from(err: SetGlobalDefaultError) -> Self {
        ShbotError::TracingSub(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;
    use toml::Value;

    #[test]
    fn test_error_display() {
        let io_err = IoError::new(ErrorKind::NotFound, "file not found");
        let err = ShbotError::Io(io_err);
        assert_eq!(err.to_string(), "IO Error: file not found");

        let custom_err = ShbotError::Custom("custom error".to_string());
        assert_eq!(custom_err.to_string(), "custom error");

        let env_err = VarError::NotPresent;
        let err2 = ShbotError::EnvVar(env_err);
        assert_eq!(
            err2.to_string(),
            "Environment variable error: environment variable not found"
        );

        let notify_err = notify::Error::generic("notify error");
        let err3 = ShbotError::Notify(notify_err);
        assert_eq!(err3.to_string(), "Watcher Error: notify error");

        let toml_err: TomlDeError = toml::from_str::<Value>("invalid toml").unwrap_err();
        let err4 = ShbotError::TomlSerDe(toml_err);
        assert!(err4.to_string().starts_with("TOML parse error:"));

        let _ = tracing::subscriber::set_global_default(tracing::subscriber::NoSubscriber::new());
        let tracing_err =
            tracing::subscriber::set_global_default(tracing::subscriber::NoSubscriber::new())
                .unwrap_err();
        let err5: ShbotError = tracing_err.into();
        assert!(err5.to_string().starts_with("Tracing error:"));
    }

    #[test]
    fn test_error_from() {
        let io_err = IoError::new(ErrorKind::NotFound, "not found");
        let err: ShbotError = io_err.into();
        assert!(matches!(err, ShbotError::Io(_)));

        let env_err = VarError::NotPresent;
        let err2: ShbotError = env_err.into();
        assert!(matches!(err2, ShbotError::EnvVar(_)));

        let notify_err = notify::Error::generic("notify error");
        let err4: ShbotError = notify_err.into();
        assert!(matches!(err4, ShbotError::Notify(_)));

        let toml_err: TomlDeError = toml::from_str::<Value>("invalid toml").unwrap_err();
        let err5: ShbotError = toml_err.into();
        assert!(matches!(err5, ShbotError::TomlSerDe(_)));
    }
}

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

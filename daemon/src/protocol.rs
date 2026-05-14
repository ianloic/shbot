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

use crate::state::Pid;
use std::path::PathBuf;

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Hello(Pid),
    InvalidHello(String),
    Cd(PathBuf),
    Reload,
    Invalidate(String),
    LogLevel(String),
    Status,
    Unknown(String),
}

impl Command {
    pub fn parse(line: &str) -> Option<Self> {
        let line_trimmed = line.trim_end();
        if line_trimmed.is_empty() {
            return None;
        }

        let mut parts = line_trimmed.splitn(2, ' ');
        let cmd = parts.next().unwrap_or("");
        let arg = parts.next().unwrap_or("");

        match cmd {
            "hello" => {
                if let Ok(pid) = arg.parse::<Pid>() {
                    Some(Command::Hello(pid))
                } else {
                    Some(Command::InvalidHello(arg.to_string()))
                }
            }
            "cd" => Some(Command::Cd(PathBuf::from(arg))),
            "reload" => Some(Command::Reload),
            "invalidate" => Some(Command::Invalidate(arg.to_string())),
            "loglevel" => Some(Command::LogLevel(arg.to_string())),
            "status" => Some(Command::Status),
            _ => Some(Command::Unknown(cmd.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_empty() {
        assert_eq!(Command::parse(""), None);
        assert_eq!(Command::parse("   "), None);
        assert_eq!(Command::parse("\n"), None);
        assert_eq!(Command::parse(" \n"), None);
    }

    #[test]
    fn test_parse_hello() {
        assert_eq!(Command::parse("hello 1234"), Some(Command::Hello(1234)));
        assert_eq!(Command::parse("hello 1234\n"), Some(Command::Hello(1234)));
        assert_eq!(
            Command::parse("hello invalid"),
            Some(Command::InvalidHello("invalid".to_string()))
        );
        assert_eq!(
            Command::parse("hello "),
            Some(Command::InvalidHello("".to_string()))
        );
    }

    #[test]
    fn test_parse_cd() {
        assert_eq!(
            Command::parse("cd /tmp"),
            Some(Command::Cd(PathBuf::from("/tmp")))
        );
        assert_eq!(
            Command::parse("cd /tmp/dir\n"),
            Some(Command::Cd(PathBuf::from("/tmp/dir")))
        );
        assert_eq!(Command::parse("cd "), Some(Command::Cd(PathBuf::from(""))));
    }

    #[test]
    fn test_parse_reload() {
        assert_eq!(Command::parse("reload"), Some(Command::Reload));
        assert_eq!(Command::parse("reload\n"), Some(Command::Reload));
        assert_eq!(Command::parse("reload extra args"), Some(Command::Reload)); // current implementation ignores arg for reload
    }

    #[test]
    fn test_parse_invalidate() {
        assert_eq!(
            Command::parse("invalidate all"),
            Some(Command::Invalidate("all".to_string()))
        );
        assert_eq!(
            Command::parse("invalidate all\n"),
            Some(Command::Invalidate("all".to_string()))
        );
        assert_eq!(
            Command::parse("invalidate "),
            Some(Command::Invalidate("".to_string()))
        );
    }

    #[test]
    fn test_parse_loglevel() {
        assert_eq!(
            Command::parse("loglevel debug"),
            Some(Command::LogLevel("debug".to_string()))
        );
        assert_eq!(
            Command::parse("loglevel debug\n"),
            Some(Command::LogLevel("debug".to_string()))
        );
        assert_eq!(
            Command::parse("loglevel "),
            Some(Command::LogLevel("".to_string()))
        );
    }

    #[test]
    fn test_parse_status() {
        assert_eq!(Command::parse("status"), Some(Command::Status));
        assert_eq!(Command::parse("status\n"), Some(Command::Status));
        assert_eq!(Command::parse("status extra"), Some(Command::Status));
    }

    #[test]
    fn test_parse_unknown() {
        assert_eq!(
            Command::parse("unknown_cmd"),
            Some(Command::Unknown("unknown_cmd".to_string()))
        );
        assert_eq!(
            Command::parse("unknown_cmd arg"),
            Some(Command::Unknown("unknown_cmd".to_string()))
        );
    }
}

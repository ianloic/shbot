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

use rexpect::session::{spawn, PtySession};
use tempfile::TempDir;
use std::process::{Child, Command};
use std::path::PathBuf;

pub struct TestEnv {
    pub pty: PtySession,
    pub home_dir: TempDir,
    pub host: String,
    pub daemon_cmd: Option<Child>,
}

impl TestEnv {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let home_dir = tempfile::tempdir()?;
        let host = "test_host".to_string();

        let mut pty = spawn("env TERM=dumb PROMPT='> ' zsh -f -i", Some(5000))?;
        pty.exp_string("> ")?;
        
        pty.send_line(&format!("export HOME={}", home_dir.path().display()))?;
        pty.exp_string("> ")?;
        pty.send_line(&format!("export HOST={}", host))?;
        pty.exp_string("> ")?;

        Ok(Self {
            pty,
            home_dir,
            host,
            daemon_cmd: None,
        })
    }

    pub fn get_shbot_bin_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
        let shbot_bin = std::env::var("SHBOT_BIN_PATH").map(PathBuf::from).unwrap_or_else(|_| {
            let mut path = std::env::current_exe().expect("Failed to get current executable path");
            path.pop();
            path.pop();
            path.push("shbot");
            path
        });
        
        if !shbot_bin.exists() {
            Err(format!("shbot binary not found at {:?}", shbot_bin).into())
        } else {
            Ok(shbot_bin)
        }
    }

    pub fn start_daemon(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let shbot_bin = Self::get_shbot_bin_path()?;

        let daemon_cmd = Command::new(shbot_bin)
            .arg("daemon")
            .env("HOME", self.home_dir.path())
            .env("HOST", &self.host)
            .env("RUST_LOG", "debug")
            .spawn()?;

        self.daemon_cmd = Some(daemon_cmd);

        let socket_path = self.home_dir.path().join(format!(".local/state/shbot/{}.socket", self.host));
        let start = std::time::Instant::now();
        while !socket_path.exists() {
            if start.elapsed().as_secs() > 10 {
                self.kill_daemon();
                return Err(format!("Daemon socket was not created at {:?}", socket_path).into());
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        Ok(())
    }

    pub fn start_connect(&self) -> Result<(), Box<dyn std::error::Error>> {
        let shbot_bin = Self::get_shbot_bin_path()?;

        let status = Command::new(shbot_bin)
            .arg("connect")
            .env("HOME", self.home_dir.path())
            .env("HOST", &self.host)
            .env("RUST_LOG", "debug")
            .status()?;

        if !status.success() {
            return Err("shbot connect failed".into());
        }

        let socket_path = self.home_dir.path().join(format!(".local/state/shbot/{}.socket", self.host));
        let start = std::time::Instant::now();
        while !socket_path.exists() {
            if start.elapsed().as_secs() > 10 {
                return Err(format!("Daemon socket was not created at {:?}", socket_path).into());
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        Ok(())
    }

    pub fn kill_daemon(&mut self) {
        if let Some(mut cmd) = self.daemon_cmd.take() {
            let pid = cmd.id();
            let _ = Command::new("kill")
                .arg("-TERM")
                .arg(pid.to_string())
                .status();
            let _ = cmd.wait(); // Wait for the process to exit and flush coverage
        }
    }

    pub fn abort_daemon(&mut self) {
        if let Some(mut cmd) = self.daemon_cmd.take() {
            let pid = cmd.id();
            let _ = Command::new("kill")
                .arg("-KILL")
                .arg(pid.to_string())
                .status();
            let _ = cmd.wait(); // Wait for the process to exit
        }
    }

    pub fn source_plugin(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let pwd = std::env::current_dir()?;
        let plugin_path = pwd.parent().unwrap().join("zsh").join("shbot.plugin.zsh");
        self.send_line(&format!("source {}", plugin_path.display()))?;
        
        // Trigger precmd to connect
        self.send_line("")?;

        Ok(())
    }

    pub fn setup_git_repo(&self, repo_name: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let repo_path = self.home_dir.path().join(repo_name);
        std::fs::create_dir(&repo_path)?;
        Command::new("git")
            .arg("init")
            .current_dir(&repo_path)
            .status()?;

        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(&repo_path)
            .status()?;
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(&repo_path)
            .status()?;

        Ok(repo_path)
    }

    pub fn send_line(&mut self, line: &str) -> Result<String, Box<dyn std::error::Error>> {
        self.pty.send_line(line)?;
        let out = self.pty.exp_string("> ")?;
        Ok(out)
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        self.kill_daemon();
    }
}

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

use rexpect::session::{PtySession, spawn};
use std::env;
use std::error::Error;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{self, Child, Command};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

pub struct TestEnv {
    pub pty: PtySession,
    pub home_dir: TempDir,
    pub host: String,
    pub daemon_cmd: Option<Child>,
}

impl TestEnv {
    pub fn new() -> Result<Self, Box<dyn Error>> {
        let home_dir = tempfile::tempdir()?;
        let host = "test_host".to_string();

        let mut pty = spawn("env TERM=dumb PROMPT='> ' zsh -f -i", Some(30000))?;
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

    pub fn get_shbot_bin_path() -> Result<PathBuf, Box<dyn Error>> {
        let shbot_bin = env::var("SHBOT_BIN_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let mut path = env::current_exe().expect("Failed to get current executable path");
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

    pub fn start_daemon(&mut self) -> Result<(), Box<dyn Error>> {
        let shbot_bin = Self::get_shbot_bin_path()?;

        let log_path = self
            .home_dir
            .path()
            .join(format!(".local/state/shbot/{}.log", self.host));
        fs::create_dir_all(log_path.parent().unwrap())?;
        let log_file = File::create(&log_path)?;
        let log_file_err = log_file.try_clone()?;

        let daemon_cmd = Command::new(shbot_bin)
            .arg("daemon")
            .env("HOME", self.home_dir.path())
            .env("HOST", &self.host)
            .env("RUST_LOG", "debug")
            .stdout(log_file)
            .stderr(log_file_err)
            .spawn()?;

        self.daemon_cmd = Some(daemon_cmd);

        let socket_path = self
            .home_dir
            .path()
            .join(format!(".local/state/shbot/{}.socket", self.host));
        self.wait_for(60, || Ok::<bool, Box<dyn Error>>(socket_path.exists()))?;
        if !socket_path.exists() {
            self.kill_daemon();
            return Err(format!("Daemon socket was not created at {:?}", socket_path).into());
        }

        Ok(())
    }

    pub fn start_connect(&mut self) -> Result<(), Box<dyn Error>> {
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

        let socket_path = self
            .home_dir
            .path()
            .join(format!(".local/state/shbot/{}.socket", self.host));
        self.wait_for(60, || Ok::<bool, Box<dyn Error>>(socket_path.exists()))?;
        if !socket_path.exists() {
            return Err(format!("Daemon socket was not created at {:?}", socket_path).into());
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
        } else {
            let socket_path = self
                .home_dir
                .path()
                .join(format!(".local/state/shbot/{}.socket", self.host));
            if let Ok(mut stream) = UnixStream::connect(&socket_path) {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                if stream.write_all(b"status\n").is_ok() {
                    let mut resp = String::new();
                    let _ = stream.read_to_string(&mut resp);
                    for line in resp.lines() {
                        if let Some(rest) = line.trim().strip_prefix("Daemon PID:")
                            && let Ok(pid) = rest.trim().parse::<u32>()
                        {
                            let _ = Command::new("kill")
                                .arg("-TERM")
                                .arg(pid.to_string())
                                .status();
                            for _ in 0..40 {
                                let status = Command::new("kill")
                                    .arg("-0")
                                    .arg(pid.to_string())
                                    .status();
                                if !status.is_ok_and(|s| s.success()) {
                                    break;
                                }
                                thread::sleep(Duration::from_millis(25));
                            }
                        }
                    }
                }
            }
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

    pub fn source_plugin(&mut self) -> Result<(), Box<dyn Error>> {
        let pwd = env::current_dir()?;
        let plugin_path = pwd.parent().unwrap().join("zsh").join("shbot.plugin.zsh");
        self.send_line(&format!("source {}", plugin_path.display()))?;

        // Trigger precmd to connect
        self.send_line("")?;

        Ok(())
    }

    pub fn setup_git_repo(&self, repo_name: &str) -> Result<PathBuf, Box<dyn Error>> {
        let repo_path = self.home_dir.path().join(repo_name);
        fs::create_dir(&repo_path)?;
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

    pub fn send_line(&mut self, line: &str) -> Result<String, Box<dyn Error>> {
        self.pty.send_line(line)?;
        let out = self.pty.exp_string("> ")?;
        Ok(out)
    }

    pub fn wait_for<F, E>(&self, timeout_secs: u64, mut condition: F) -> Result<(), E>
    where
        F: FnMut() -> Result<bool, E>,
        E: From<String>,
    {
        let start = Instant::now();
        loop {
            if condition()? {
                return Ok(());
            }
            if start.elapsed().as_secs() > timeout_secs {
                return Err(E::from(format!(
                    "Timed out after {}s waiting for condition",
                    timeout_secs
                )));
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn wait_for_ref<F, E>(&self, timeout_secs: u64, mut condition: F) -> Result<(), E>
    where
        F: FnMut(&Self) -> Result<bool, E>,
        E: From<String>,
    {
        let start = Instant::now();
        loop {
            if condition(self)? {
                return Ok(());
            }
            if start.elapsed().as_secs() > timeout_secs {
                return Err(E::from(format!(
                    "Timed out after {}s waiting for condition",
                    timeout_secs
                )));
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn wait_for_mut<F, E>(&mut self, timeout_secs: u64, mut condition: F) -> Result<(), E>
    where
        F: FnMut(&mut Self) -> Result<bool, E>,
        E: From<String>,
    {
        let start = Instant::now();
        loop {
            if condition(self)? {
                return Ok(());
            }
            if start.elapsed().as_secs() > timeout_secs {
                return Err(E::from(format!(
                    "Timed out after {}s waiting for condition",
                    timeout_secs
                )));
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        self.kill_daemon();
        // Clean up zsh process
        let _ = self.pty.send_line("exit");
        // Force kill any remaining children of this test
        let _ = Command::new("pkill")
            .arg("-P")
            .arg(process::id().to_string())
            .status();
    }
}

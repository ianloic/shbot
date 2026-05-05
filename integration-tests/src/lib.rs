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

pub mod test_env;

#[cfg(test)]
mod tests {
    use super::test_env::TestEnv;
    use std::os::unix::net::UnixListener;

    // Quick test to ensure that the Zsh plugin sources correctly and spawns the shbot binary
    #[test]
    fn test_zsh_plugin_bootstrap() -> Result<(), Box<dyn std::error::Error>> {
        let mut env = TestEnv::new()?;

        let dummy_sock = env.home_dir.path().join("dummy.sock");
        let listener = UnixListener::bind(&dummy_sock)?;

        let shbot_bin = env.home_dir.path().join("shbot");
        std::fs::write(&shbot_bin, "#!/bin/sh\nexit 0\n")?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shbot_bin, std::fs::Permissions::from_mode(0o755))?;

        // Add fake bin to path and set socket
        env.send_line(&format!("export PATH={}:$PATH", env.home_dir.path().display()))?;
        println!("Sending export SHBOT_SOCKET...");
        env.send_line(&format!("export SHBOT_SOCKET={}", dummy_sock.display()))?;

        println!("Sourcing plugin...");
        env.source_plugin()?;

        println!("Waiting for listener.accept()...");
        listener.set_nonblocking(true)?;

        // Debug variables
        let debug_output = env.send_line("echo FD=$SHBOT_FD")?;
        println!("Zsh printed: {}", debug_output);

        let start_time = std::time::Instant::now();
        let mut stream_opt = None;
        loop {
            match listener.accept() {
                Ok((s, _)) => {
                    stream_opt = Some(s);
                    break;
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if start_time.elapsed().as_secs() > 1 {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(e) => return Err(e.into()),
            }
        }

        if stream_opt.is_none() {
            println!("Socket accept timed out. Fetching Zsh debug info...");
            let debug_info = env.send_line("echo ZSH_DEBUG_INFO")?;
            println!("DEBUG: {}", debug_info);
            panic!("Test failed: Socket timeout.");
        }
        let mut stream = stream_opt.unwrap();
        println!("Accepted connection!");

        use std::io::{Read, Write};
        let mut buf = [0; 64];
        let n = stream.read(&mut buf)?;
        let rx = String::from_utf8_lossy(&buf[..n]);

        // We expect hello <pid>\n and cd <pwd>\n
        assert!(rx.contains("hello"));
        assert!(rx.contains("cd "));

        // Simulate sending a payload
        stream.write_all(b"update generic payloadinfo\n")?;

        // Give ZLE a moment to wake up, read the socket, and set the variable
        std::thread::sleep(std::time::Duration::from_millis(200));

        // Test if Zsh variable is set correctly
        env.pty.send_line("echo $SHBOT_STATE[generic]")?;
        env.pty.exp_string("payloadinfo")?;

        Ok(())
    }

    #[test]
    fn test_git_watcher_updates_client() -> Result<(), Box<dyn std::error::Error>> {
        let mut env = TestEnv::new()?;
        let repo_path = env.setup_git_repo("myrepo")?;

        env.start_daemon()?;

        env.send_line("export SHBOT_SET_VCS_STATUS=1")?;
        env.send_line(&format!("cd {}", repo_path.display()))?;

        env.source_plugin()?;

        std::thread::sleep(std::time::Duration::from_millis(2000));

        env.pty.send_line("echo RES\"\"ULT_PRE=${VCS_STATUS_NUM_UNTRACKED}")?;
        env.pty.exp_string("RESULT_PRE=")?;
        let pre_untracked_raw = env.pty.exp_string("> ")?;

        env.send_line("touch new_file.txt")?;

        // Wait for daemon file watcher to detect, re-eval git, and broadcast
        std::thread::sleep(std::time::Duration::from_millis(2000));

        env.pty.send_line("echo RES\"\"ULT_POST=${VCS_STATUS_NUM_UNTRACKED}")?;
        env.pty.exp_string("RESULT_POST=")?;
        let post_untracked_raw = env.pty.exp_string("> ")?;

        env.kill_daemon();

        let pre_untracked = pre_untracked_raw
            .replace('\r', "\n")
            .replace('%', "")
            .trim()
            .to_string();
        let post_untracked = post_untracked_raw
            .replace('\r', "\n")
            .replace('%', "")
            .trim()
            .to_string();

        println!("PRE RAW: {}", pre_untracked_raw);
        println!("POST RAW: {}", post_untracked_raw);

        println!("PRE_UNTRACKED: {}", pre_untracked);
        println!("POST_UNTRACKED: {}", post_untracked);

        assert_eq!(pre_untracked, "0", "Initially, untracked files should be 0");
        assert_eq!(
            post_untracked, "1",
            "After file creation, untracked files should be 1"
        );

        Ok(())
    }

    #[test]
    fn test_cli_commands() -> Result<(), Box<dyn std::error::Error>> {
        let mut env = TestEnv::new()?;
        let repo_path = env.setup_git_repo("myrepo")?;

        env.start_daemon()?;

        let shbot_bin = TestEnv::get_shbot_bin_path()?;

        // test loglevel
        let out = std::process::Command::new(&shbot_bin)
            .arg("loglevel")
            .arg("debug")
            .env("HOME", env.home_dir.path())
            .env("HOST", &env.host)
            .output()?;
        assert!(out.status.success());
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("status: log level updated successfully"));

        // test reload
        let out = std::process::Command::new(&shbot_bin)
            .arg("reload")
            .env("HOME", env.home_dir.path())
            .env("HOST", &env.host)
            .output()?;
        assert!(out.status.success());
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("configuration reloaded successfully") || stdout.contains("failed to load config"));

        env.send_line("export SHBOT_SET_VCS_STATUS=1")?;
        env.send_line(&format!("cd {}", repo_path.display()))?;
        env.source_plugin()?;

        std::thread::sleep(std::time::Duration::from_millis(500));

        // Test sending 'invalidate' over the socket
        env.send_line(&format!("_shbot_send \"invalidate {}\"", repo_path.display()))?;
        std::thread::sleep(std::time::Duration::from_millis(500));

        Ok(())
    }
}

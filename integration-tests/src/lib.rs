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

        let mut stream_opt = None;
        env.wait_for(1, || {
            match listener.accept() {
                Ok((s, _)) => {
                    stream_opt = Some(s);
                    Ok::<bool, Box<dyn std::error::Error>>(true)
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    Ok::<bool, Box<dyn std::error::Error>>(false)
                }
                Err(e) => Err(e.into()),
            }
        })?;

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
        env.wait_for_mut(2, |env| {
            let out = env.send_line("echo $SHBOT_STATE[generic]")?;
            Ok::<bool, Box<dyn std::error::Error>>(out.contains("payloadinfo"))
        })?;

        // Test if Zsh variable is set correctly
        let final_out = env.send_line("echo $SHBOT_STATE[generic]")?;
        assert!(final_out.contains("payloadinfo"));

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

        let mut pre_untracked_raw = String::new();
        env.wait_for_mut(5, |env| {
            env.pty.send_line("echo RES\"\"ULT_PRE=${VCS_STATUS_NUM_UNTRACKED}")?;
            env.pty.exp_string("RESULT_PRE=")?;
            pre_untracked_raw = env.pty.exp_string("> ")?;
            let val = pre_untracked_raw.replace('\r', "\n").split('\n').next().unwrap_or("").trim().to_string();
            Ok::<bool, Box<dyn std::error::Error>>(val == "0")
        })?;

        env.send_line("touch new_file.txt")?;

        // Wait for daemon file watcher to detect, re-eval git, and broadcast
        let mut post_untracked_raw = String::new();
        env.wait_for_mut(5, |env| {
            env.pty.send_line("echo RES\"\"ULT_POST=${VCS_STATUS_NUM_UNTRACKED}")?;
            env.pty.exp_string("RESULT_POST=")?;
            post_untracked_raw = env.pty.exp_string("> ")?;
            let val = post_untracked_raw.replace('\r', "\n").split('\n').next().unwrap_or("").trim().to_string();
            Ok::<bool, Box<dyn std::error::Error>>(val == "1")
        })?;

        env.kill_daemon();

        let pre_untracked = pre_untracked_raw
            .replace('\r', "\n")
            .replace('%', "")
            .replace('#', "")
            .trim()
            .to_string();
        let post_untracked = post_untracked_raw
            .replace('\r', "\n")
            .replace('%', "")
            .replace('#', "")
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

        let log_path = env.home_dir.path().join(format!(".local/state/shbot/{}.log", env.host));
        env.wait_for(5, || {
            Ok::<bool, Box<dyn std::error::Error>>(
                std::fs::read_to_string(&log_path).map(|c| c.contains("Handling cd to")).unwrap_or(false)
            )
        })?;

        // Test sending 'invalidate' over the socket
        env.send_line(&format!("_shbot_send \"invalidate {}\"", repo_path.display()))?;
        
        env.wait_for(5, || {
            Ok::<bool, Box<dyn std::error::Error>>(
                std::fs::read_to_string(&log_path).map(|c| c.contains("Invalidate request for:")).unwrap_or(false)
            )
        })?;

        Ok(())
    }

    #[test]
    fn test_daemon_crash_recovery() -> Result<(), Box<dyn std::error::Error>> {
        let mut env = TestEnv::new()?;
        
        // 1. Start daemon natively
        env.start_daemon()?;
        
        let socket_path = env.home_dir.path().join(format!(".local/state/shbot/{}.socket", env.host));
        assert!(socket_path.exists());
        
        // 2. Read PID and kill it abruptly (SIGKILL)
        let pid = env.daemon_cmd.as_ref().unwrap().id();
        std::process::Command::new("kill")
            .arg("-KILL")
            .arg(pid.to_string())
            .status()?;
            
        env.wait_for_mut(5, |env| {
            Ok::<bool, Box<dyn std::error::Error>>(env.daemon_cmd.as_mut().unwrap().try_wait().unwrap_or(None).is_some())
        })?;
        
        assert!(socket_path.exists(), "Socket file should remain after crash");
        env.daemon_cmd = None; // Reset the handle so kill_daemon doesn't trip on it

        // 3. Start daemon again via 'connect' (it should clean up stale files and start successfully)
        env.start_connect()?;
        
        // Ensure it's up
        let shbot_bin = TestEnv::get_shbot_bin_path()?;
        let out = std::process::Command::new(&shbot_bin)
            .arg("status")
            .env("HOME", env.home_dir.path())
            .env("HOST", &env.host)
            .output()?;
        
        assert!(out.status.success(), "Daemon should respond to status after crash recovery");
        
        // Final cleanup using pkill
        let _ = std::process::Command::new("pkill").arg("-f").arg("shbot daemon").status();
        
        Ok(())
    }
    #[test]
    fn test_resource_fd_leak() -> Result<(), Box<dyn std::error::Error>> {
        let mut env = TestEnv::new()?;
        env.start_daemon()?;

        let pid = env.daemon_cmd.as_ref().unwrap().id();

        // Let the daemon settle by waiting for log output
        let log_path = env.home_dir.path().join(format!(".local/state/shbot/{}.log", env.host));
        env.wait_for(5, || {
            Ok::<bool, Box<dyn std::error::Error>>(
                std::fs::read_to_string(&log_path).map(|c| c.contains("listening")).unwrap_or(false)
            )
        })?;

        let initial_fds = std::fs::read_dir(format!("/proc/{}/fd", pid))?.count();

        let socket_path = env.home_dir.path().join(format!(".local/state/shbot/{}.socket", env.host));

        // Connect and disconnect rapidly to ensure FDs are released
        for i in 0..1000 {
            let mut stream = std::os::unix::net::UnixStream::connect(&socket_path)?;
            use std::io::Write;
            stream.write_all(format!("hello {}\n", i).as_bytes())?;
            // Drop stream
        }

        let mut final_fds = 0;
        env.wait_for(5, || {
            final_fds = std::fs::read_dir(format!("/proc/{}/fd", pid))?.count();
            Ok::<bool, Box<dyn std::error::Error>>(final_fds < initial_fds + 50)
        })?;

        assert!(
            final_fds < initial_fds + 50,
            "FD leak detected! initial: {}, final: {}",
            initial_fds,
            final_fds
        );

        Ok(())
    }

    #[test]
    fn test_resource_inotify_leak() -> Result<(), Box<dyn std::error::Error>> {
        let mut env = TestEnv::new()?;
        env.start_connect()?;

        let socket_path = env.home_dir.path().join(format!(".local/state/shbot/{}.socket", env.host));
        let shbot_bin = TestEnv::get_shbot_bin_path()?;

        for i in 0..200 {
            let temp_dir = tempfile::tempdir()?;
            let mut stream = std::os::unix::net::UnixStream::connect(&socket_path)?;
            use std::io::Write;
            stream.write_all(format!("hello {}\n", i).as_bytes())?;
            stream.write_all(format!("cd {}\n", temp_dir.path().display()).as_bytes())?;
            // Abruptly drop the client stream without unregistering via command
        }

        let mut count: usize = usize::MAX;
        env.wait_for_ref(5, |env| {
            let out = std::process::Command::new(&shbot_bin)
                .arg("status")
                .env("HOME", env.home_dir.path())
                .env("HOST", &env.host)
                .output()?;
            let status = String::from_utf8_lossy(&out.stdout);
            let lines: Vec<&str> = status.lines().collect();
            if let Some(watches_line) = lines.iter().find(|&&l| l.contains("Active File Monitors:")) {
                let parts: Vec<&str> = watches_line.split(':').collect();
                count = parts[1].trim().parse()?;
                if count == 0 {
                    return Ok(true);
                }
            }
            Ok::<bool, Box<dyn std::error::Error>>(false)
        })?;
        
        assert_eq!(count, 0, "There should be no leaked inotify watches");

        Ok(())
    }

    #[test]
    fn test_resource_zombie_process() -> Result<(), Box<dyn std::error::Error>> {
        let mut env = TestEnv::new()?;
        
        // Write config with a trigger that hangs
        let config_path = env.home_dir.path().join(".config/shbot/shbot.toml");
        std::fs::create_dir_all(config_path.parent().unwrap())?;
        std::fs::write(&config_path, r#"
[triggers.hang_test]
type = "file-change"
watch = "**/*.txt"
command = "sleep 60"
"#)?;

        env.start_daemon()?;

        let socket_path = env.home_dir.path().join(format!(".local/state/shbot/{}.socket", env.host));
        let pid = env.daemon_cmd.as_ref().unwrap().id();

        // Connect to trigger a watch
        let temp_dir = tempfile::tempdir()?;
        let mut stream = std::os::unix::net::UnixStream::connect(&socket_path)?;
        use std::io::Write;
        stream.write_all(b"hello 123\n")?;
        stream.write_all(format!("cd {}\n", temp_dir.path().display()).as_bytes())?;

        // Create a file to trigger the watch
        std::fs::write(temp_dir.path().join("trigger.txt"), "trigger")?;

        // Wait for the sleep 60 to start
        env.wait_for(5, || {
            let out = std::process::Command::new("ps").arg("-ef").output()?;
            let ps_output = String::from_utf8_lossy(&out.stdout);
            Ok::<bool, Box<dyn std::error::Error>>(ps_output.lines().any(|l| l.contains("sleep 60") && !l.contains("defunct")))
        })?;

        // Drop the client abruptly
        drop(stream);

        // Give it time to clean up
        env.wait_for(5, || {
            let out = std::process::Command::new("ps").arg("-ef").output()?;
            let ps_output = String::from_utf8_lossy(&out.stdout);
            Ok::<bool, Box<dyn std::error::Error>>(!ps_output.lines().any(|l| l.contains("sleep 60") && !l.contains("defunct")))
        })?;

        // Kill the daemon to make sure sleep 60 is either killed or reaped by tokio
        std::process::Command::new("kill")
            .arg("-TERM")
            .arg(pid.to_string())
            .status()?;
        env.wait_for_mut(5, |env| {
            Ok::<bool, Box<dyn std::error::Error>>(env.daemon_cmd.as_mut().unwrap().try_wait().unwrap_or(None).is_some())
        })?;

        // Check for any zombie processes named "sleep" spawned by us.
        // It's hard to precisely find zombies of a specific process if it's dead, 
        // but if tokio cleans up before dying, or if dying cleans it up, we can check `ps`.
        let out = std::process::Command::new("ps")
            .arg("-ef")
            .output()?;
        let ps_output = String::from_utf8_lossy(&out.stdout);
        // Ensure there's no defunct sleep process
        for line in ps_output.lines() {
            if line.contains("sleep 60") && line.contains("<defunct>") {
                panic!("Found zombie process: {}", line);
            }
        }

        Ok(())
    }

    #[test]
    fn test_resource_memory_leak() -> Result<(), Box<dyn std::error::Error>> {
        let mut env = TestEnv::new()?;
        env.start_daemon()?;

        let socket_path = env.home_dir.path().join(format!(".local/state/shbot/{}.socket", env.host));
        let pid = env.daemon_cmd.as_ref().unwrap().id();

        let log_path = env.home_dir.path().join(format!(".local/state/shbot/{}.log", env.host));
        env.wait_for(5, || {
            Ok::<bool, Box<dyn std::error::Error>>(
                std::fs::read_to_string(&log_path).map(|c| c.contains("listening")).unwrap_or(false)
            )
        })?;

        let get_rss = || -> Result<u64, Box<dyn std::error::Error>> {
            let statm = std::fs::read_to_string(format!("/proc/{}/statm", pid))?;
            let parts: Vec<&str> = statm.split_whitespace().collect();
            let rss_pages: u64 = parts[1].parse()?;
            Ok(rss_pages)
        };

        let initial_fds = std::fs::read_dir(format!("/proc/{}/fd", pid))?.count();
        // Warm up phase to allow caches and allocator pools to expand
        for i in 0..500 {
            let temp_dir = tempfile::tempdir()?;
            let mut stream = std::os::unix::net::UnixStream::connect(&socket_path)?;
            use std::io::Write;
            stream.write_all(format!("hello {}\n", i).as_bytes())?;
            stream.write_all(format!("cd {}\n", temp_dir.path().display()).as_bytes())?;
        }

        env.wait_for(10, || {
            let current_fds = std::fs::read_dir(format!("/proc/{}/fd", pid))?.count();
            Ok::<bool, Box<dyn std::error::Error>>(current_fds < initial_fds + 50)
        })?;
        let rss_after_warmup = get_rss()?;

        // Heavy load phase
        for i in 500..1500 {
            let temp_dir = tempfile::tempdir()?;
            let mut stream = std::os::unix::net::UnixStream::connect(&socket_path)?;
            use std::io::Write;
            stream.write_all(format!("hello {}\n", i).as_bytes())?;
            stream.write_all(format!("cd {}\n", temp_dir.path().display()).as_bytes())?;
        }

        env.wait_for(10, || {
            let current_fds = std::fs::read_dir(format!("/proc/{}/fd", pid))?.count();
            Ok::<bool, Box<dyn std::error::Error>>(current_fds < initial_fds + 50)
        })?;
        let rss_after_load = get_rss()?;

        // Once warmed up, the RSS should plateau. 
        // We allow a small growth (e.g. 5000 pages = 20MB) for minor variations and allocator pooling.
        assert!(
            rss_after_load < rss_after_warmup + 5000, 
            "Memory leak detected! RSS after warmup: {}, RSS after load: {}", 
            rss_after_warmup, rss_after_load
        );

        Ok(())
    }

    #[test]
    fn test_daemon_sighup_reload() -> Result<(), Box<dyn std::error::Error>> {
        let mut env = TestEnv::new()?;
        
        let config_path = env.home_dir.path().join(".config/shbot/shbot.toml");
        std::fs::create_dir_all(config_path.parent().unwrap())?;
        std::fs::write(&config_path, "loglevel = \"info\"\n")?;
        
        // 1. Start daemon
        env.start_daemon()?;
        
        let shbot_bin = TestEnv::get_shbot_bin_path()?;
        
        // Verify initial state is running
        let out_status = std::process::Command::new(&shbot_bin)
            .arg("status")
            .env("HOME", env.home_dir.path())
            .env("HOST", &env.host)
            .output()?;
        assert!(out_status.status.success(), "Daemon should be running");
        
        // 2. Change config to debug
        std::fs::write(&config_path, "loglevel = \"debug\"\n")?;
        
        let pid = env.daemon_cmd.as_ref().unwrap().id();
        
        // 3. Send SIGHUP
        std::process::Command::new("kill")
            .arg("-HUP")
            .arg(pid.to_string())
            .status()?;
            
        // Wait for SIGHUP to be processed by checking the log
        let log_path = env.home_dir.path().join(format!(".local/state/shbot/{}.log", env.host));
        env.wait_for(5, || {
            Ok::<bool, Box<dyn std::error::Error>>(
                std::fs::read_to_string(&log_path).map(|c| c.contains("Received SIGHUP, reloading configuration")).unwrap_or(false)
            )
        })?;
        
        // 4. Verify SIGHUP was received in the log
        let log_path = env.home_dir.path().join(format!(".local/state/shbot/{}.log", env.host));
        let log_content = std::fs::read_to_string(&log_path).unwrap_or_default();
        
        assert!(log_content.contains("Received SIGHUP, reloading configuration"), "SIGHUP should be logged. Log content:\n{}", log_content);
        
        let _ = std::process::Command::new("kill").arg("-TERM").arg(pid.to_string()).status();
        Ok(())
    }
}

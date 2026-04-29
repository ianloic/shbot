#[cfg(test)]
mod tests {
    use rexpect::session::spawn;
    use std::os::unix::net::UnixListener;

    // Quick test to ensure that the Zsh plugin sources correctly and spawns the shbot binary
    #[test]
    fn test_zsh_plugin_bootstrap() -> Result<(), Box<dyn std::error::Error>> {
        // We set up a fake shbot bin in path and test whether connection triggers.
        // For right now, we can just test if the plugin loads.

        // Use TERM=dumb to minimize escape sequences, but keep ZLE active for zle -F to work.
        let mut p = spawn("env TERM=dumb PROMPT='> ' zsh -f -i", Some(2000))?;

        // Wait for prompt
        p.exp_string("> ")?;

        let pwd = std::env::current_dir()?;
        let plugin_path = pwd.parent().unwrap().join("zsh").join("shbot.plugin.zsh");

        // Setup dummy socket and fake binary
        let temp_dir = tempfile::tempdir()?;
        let dummy_sock = temp_dir.path().join("dummy.sock");
        let listener = UnixListener::bind(&dummy_sock)?;

        let shbot_bin = temp_dir.path().join("shbot");
        std::fs::write(&shbot_bin, "#!/bin/sh\nexit 0\n")?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shbot_bin, std::fs::Permissions::from_mode(0o755))?;

        // Add fake bin to path and set socket
        p.send_line(&format!("export PATH={}:$PATH", temp_dir.path().display()))?;
        p.exp_string("> ")?;

        println!("Sending export SHBOT_SOCKET...");
        p.send_line(&format!("export SHBOT_SOCKET={}", dummy_sock.display()))?;
        p.exp_string("> ")?;

        println!("Sourcing plugin...");
        p.send_line(&format!("source {}", plugin_path.display()))?;
        p.exp_string("> ")?;

        println!("Sending empty line to trigger precmd...");
        p.send_line("")?;
        p.exp_string("> ")?;

        println!("Waiting for listener.accept()...");
        listener.set_nonblocking(true)?;

        // Debug variables
        p.send_line("echo FD=$SHBOT_FD")?;
        let debug_output = p.exp_string("> ")?;
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
            p.send_line("echo ZSH_DEBUG_INFO")?;
            let debug_info = p.exp_string("> ")?;
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
        p.send_line("echo $SHBOT_STATE[generic]")?;

        // rexpect needs us to match the exact output. We expect `payloadinfo`
        p.exp_string("payloadinfo")?;

        Ok(())
    }

    #[test]
    fn test_git_watcher_updates_client() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let home_path = temp_dir.path();

        let repo_path = home_path.join("myrepo");
        std::fs::create_dir(&repo_path)?;
        std::process::Command::new("git")
            .arg("init")
            .current_dir(&repo_path)
            .status()?;

        std::process::Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(&repo_path)
            .status()?;
        std::process::Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(&repo_path)
            .status()?;

        let mut shbot_bin = std::env::current_exe()?;
        shbot_bin.pop();
        shbot_bin.pop();
        shbot_bin.push("shbot");

        if !shbot_bin.exists() {
            panic!("shbot binary not found at {:?}", shbot_bin);
        }

        let mut daemon_cmd = std::process::Command::new(shbot_bin)
            .arg("daemon")
            .env("HOME", home_path)
            .env("HOST", "test_host")
            .env("RUST_LOG", "debug")
            .spawn()?;

        let socket_path = home_path.join(".local/state/shbot/test_host.socket");
        let start = std::time::Instant::now();
        while !socket_path.exists() {
            if start.elapsed().as_secs() > 10 {
                daemon_cmd.kill()?;
                panic!("Daemon socket was not created at {:?}", socket_path);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        let pwd = std::env::current_dir()?;
        let plugin_path = pwd.parent().unwrap().join("zsh").join("shbot.plugin.zsh");

        let mut p = spawn("env TERM=dumb PROMPT='> ' zsh -f -i", Some(5000))?;
        p.send_line(&format!("export HOME={}", home_path.display()))?;
        p.exp_string("> ")?;
        p.send_line("export HOST=test_host")?;
        p.exp_string("> ")?;
        p.send_line("export SHBOT_SET_VCS_STATUS=1")?;
        p.exp_string("> ")?;
        p.send_line(&format!("cd {}", repo_path.display()))?;
        p.exp_string("> ")?;

        p.send_line(&format!("source {}", plugin_path.display()))?;
        p.exp_string("> ")?;

        // Trigger precmd to connect
        p.send_line("")?;
        p.exp_string("> ")?;

        std::thread::sleep(std::time::Duration::from_millis(2000));

        p.send_line("echo RES\"\"ULT_PRE=${VCS_STATUS_NUM_UNTRACKED}")?;
        p.exp_string("RESULT_PRE=")?;
        let pre_untracked_raw = p.exp_string("> ")?;

        p.send_line("touch new_file.txt")?;
        p.exp_string("> ")?;

        // Wait for daemon file watcher to detect, re-eval git, and broadcast
        std::thread::sleep(std::time::Duration::from_millis(2000));

        p.send_line("echo RES\"\"ULT_POST=${VCS_STATUS_NUM_UNTRACKED}")?;
        p.exp_string("RESULT_POST=")?;
        let post_untracked_raw = p.exp_string("> ")?;

        daemon_cmd.kill()?;

        let pre_untracked = pre_untracked_raw
            .replace('\r', "\n")
            // .replace('\n', "")
            .replace('%', "")
            .trim()
            .to_string();
        let post_untracked = post_untracked_raw
            .replace('\r', "\n")
            // .replace('\n', "")
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
}

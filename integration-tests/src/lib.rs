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
    use std::error::Error;
    use std::fs;
    use std::io::{Read, Write};
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
        env.send_line(&format!(
            "export PATH={}:$PATH",
            env.home_dir.path().display()
        ))?;
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
        env.wait_for(1, || match listener.accept() {
            Ok((s, _)) => {
                stream_opt = Some(s);
                Ok::<bool, Box<dyn std::error::Error>>(true)
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                Ok::<bool, Box<dyn std::error::Error>>(false)
            }
            Err(e) => Err(e.into()),
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
    fn test_protocol_newline_injection_is_escaped() -> Result<(), Box<dyn std::error::Error>> {
        let mut env = TestEnv::new()?;

        let dummy_sock = env.home_dir.path().join("dummy.sock");
        let listener = UnixListener::bind(&dummy_sock)?;

        env.send_line(&format!("export SHBOT_SOCKET={}", dummy_sock.display()))?;
        env.source_plugin()?;

        listener.set_nonblocking(true)?;
        let mut stream = None;
        env.wait_for(5, || match listener.accept() {
            Ok((s, _)) => {
                stream = Some(s);
                Ok::<bool, Box<dyn std::error::Error>>(true)
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                Ok::<bool, Box<dyn std::error::Error>>(false)
            }
            Err(e) => Err(e.into()),
        })?;
        let mut stream = stream.expect("plugin never connected");

        // A payload containing an escaped newline that would, if naively
        // relayed, be parsed by the shell as a forged second protocol line.
        stream.write_all(b"update inj x%0Aupdate evil forged\n")?;

        // The decoded payload must contain the newline literally. The retry
        // loop also drains stale PTY output left by zle reset-prompt redraws.
        env.wait_for_mut(5, |env| {
            let out = env.send_line("print -r -- \"INJ=${SHBOT_STATE[inj]}\"")?;
            Ok::<bool, Box<dyn std::error::Error>>(out.contains("x\n") || out.contains("x\r\n"))
        })?;
        env.wait_for_mut(5, |env| {
            let out = env.send_line("print -r -- \"INJ=${SHBOT_STATE[inj]}\"")?;
            Ok::<bool, Box<dyn std::error::Error>>(out.contains("update evil forged"))
        })?;

        // ...but no forged state key may have been created.
        env.wait_for_mut(5, |env| {
            let out = env.send_line("print -r -- \"EV=${SHBOT_STATE[evil]-unset}\"")?;
            Ok::<bool, Box<dyn std::error::Error>>(out.contains("EV=unset"))
        })?;
        let out = env.send_line("print -r -- \"EV=${SHBOT_STATE[evil]-unset}\"")?;
        assert!(
            out.contains("EV=unset"),
            "forged state key must not be created, got: {}",
            out
        );

        Ok(())
    }

    #[test]
    fn test_protocol_vcs_status_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let mut env = TestEnv::new()?;

        let dummy_sock = env.home_dir.path().join("dummy.sock");
        let listener = UnixListener::bind(&dummy_sock)?;

        env.send_line("export SHBOT_SET_VCS_STATUS=1")?;
        env.send_line(&format!("export SHBOT_SOCKET={}", dummy_sock.display()))?;
        env.source_plugin()?;

        listener.set_nonblocking(true)?;
        let mut stream = None;
        env.wait_for(5, || match listener.accept() {
            Ok((s, _)) => {
                stream = Some(s);
                Ok::<bool, Box<dyn std::error::Error>>(true)
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                Ok::<bool, Box<dyn std::error::Error>>(false)
            }
            Err(e) => Err(e.into()),
        })?;
        let mut stream = stream.expect("plugin never connected");

        // 27 \x1f-separated gitstatus fields with escaped separators. Field 27
        // carries an escaped newline to prove decoding happens before parsing.
        let fields: Vec<&str> = vec![
            "/tmp/repo",
            "abcdef123",
            "main",
            "",
            "",
            "",
            "",
            "0",
            "1",
            "0",
            "0",
            "0",
            "0",
            "0",
            "0",
            "",
            "",
            "",
            "",
            "",
            "",
            "0",
            "0",
            "2",
            "3",
            "",
            "subject%0Abody",
        ];
        assert_eq!(fields.len(), 27);
        let payload = fields.join("%1F");
        // Send a multi-line burst in a single write to verify _shbot_read_handler drains all available lines
        stream.write_all(
            format!(
                "update burst_plain hello_world\nupdate burst_pct 100%25_done\nupdate git {}\n",
                payload
            )
            .as_bytes(),
        )?;

        env.wait_for_mut(5, |env| {
            let out = env.send_line(
                "print -r -- \"BR=${VCS_STATUS_LOCAL_BRANCH}|SW=${VCS_STATUS_NUM_SKIP_WORKTREE}|AU=${VCS_STATUS_NUM_ASSUME_UNCHANGED}|B1=${SHBOT_STATE[burst_plain]}|B2=${SHBOT_STATE[burst_pct]}\"",
            )?;
            Ok::<bool, Box<dyn Error>>(
                out.contains("BR=main|SW=2|AU=3|B1=hello_world|B2=100%_done"),
            )
        })?;

        // Field alignment must survive the many empty fields.
        env.wait_for_mut(5, |env| {
            let out = env.send_line("print -r -- \"NS=${VCS_STATUS_NUM_STAGED}\"")?;
            Ok::<bool, Box<dyn std::error::Error>>(out.contains("NS=1"))
        })?;

        // The escaped newline in the last field decodes to a real newline.
        env.wait_for_mut(5, |env| {
            let out = env.send_line("print -r -- \"$VCS_STATUS_COMMIT_SUMMARY\"")?;
            Ok::<bool, Box<dyn std::error::Error>>(out.contains("subject"))
        })?;
        let out = env.send_line("print -r -- \"$VCS_STATUS_COMMIT_SUMMARY\"")?;
        assert!(out.contains("subject"), "got: {}", out);
        assert!(out.contains("body"), "got: {}", out);
        assert!(!out.contains("%0A"), "payload not decoded: {}", out);

        // Sending an empty git update ("update git \n") must clear SHBOT_STATE[git],
        // unset all VCS_STATUS_* variables, and set VCS_STATUS_RESULT=norepo-sync.
        stream.write_all(b"update git \n")?;
        env.wait_for_mut(5, |env| {
            let out = env.send_line(
                "print -r -- \"ST=${SHBOT_STATE[git]}|WD=${VCS_STATUS_WORKDIR-unset}|BR=${VCS_STATUS_LOCAL_BRANCH-unset}|RES=${VCS_STATUS_RESULT}\"",
            )?;
            Ok::<bool, Box<dyn Error>>(out.contains("ST=|WD=unset|BR=unset|RES=norepo-sync"))
        })?;
        let out = env.send_line(
            "print -r -- \"ST=${SHBOT_STATE[git]}|WD=${VCS_STATUS_WORKDIR-unset}|BR=${VCS_STATUS_LOCAL_BRANCH-unset}|RES=${VCS_STATUS_RESULT}\"",
        )?;
        assert!(
            out.contains("ST=|WD=unset|BR=unset|RES=norepo-sync"),
            "Empty git update must clear git state and VCS_STATUS_*, got: {}",
            out
        );

        Ok(())
    }

    #[test]
    fn test_send_rejects_newline_injection() -> Result<(), Box<dyn std::error::Error>> {
        let mut env = TestEnv::new()?;

        let dummy_sock = env.home_dir.path().join("dummy.sock");
        let listener = UnixListener::bind(&dummy_sock)?;

        env.send_line(&format!("export SHBOT_SOCKET={}", dummy_sock.display()))?;
        env.source_plugin()?;

        listener.set_nonblocking(true)?;
        let mut stream = None;
        env.wait_for(5, || match listener.accept() {
            Ok((s, _)) => {
                stream = Some(s);
                Ok::<bool, Box<dyn std::error::Error>>(true)
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                Ok::<bool, Box<dyn std::error::Error>>(false)
            }
            Err(e) => Err(e.into()),
        })?;
        let mut stream = stream.expect("plugin never connected");
        stream.set_nonblocking(true)?;

        // Drain the initial hello/cd exchange.
        env.wait_for(5, || {
            let mut buf = [0; 256];
            match stream.read(&mut buf) {
                Ok(n) if n > 0 => Ok::<bool, Box<dyn std::error::Error>>(true),
                Ok(_) => Ok(false),
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(false),
                Err(e) => Err(e.into()),
            }
        })?;

        // A $PWD containing a newline must never reach the wire as two commands.
        env.send_line("_shbot_send $'cd /evil\\nreload'")?;
        let out = env.send_line("print -r -- RC=$?")?;
        assert!(out.contains("RC=1"), "send must be refused, got: {}", out);

        // Nothing extra may have been written to the socket.
        let mut buf = [0; 256];
        let n = stream.read(&mut buf).unwrap_or(0);
        assert!(
            n == 0,
            "unexpected bytes written to socket after refused send"
        );

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
            env.pty
                .send_line("echo RES\"\"ULT_PRE=${VCS_STATUS_NUM_UNTRACKED}")?;
            env.pty.exp_string("RESULT_PRE=")?;
            pre_untracked_raw = env.pty.exp_string("> ")?;
            let val = pre_untracked_raw
                .replace('\r', "\n")
                .split('\n')
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            Ok::<bool, Box<dyn std::error::Error>>(val == "0")
        })?;

        env.send_line("touch new_file.txt")?;

        // Wait for daemon file watcher to detect, re-eval git, and broadcast
        let mut post_untracked_raw = String::new();
        env.wait_for_mut(5, |env| {
            env.pty
                .send_line("echo RES\"\"ULT_POST=${VCS_STATUS_NUM_UNTRACKED}")?;
            env.pty.exp_string("RESULT_POST=")?;
            post_untracked_raw = env.pty.exp_string("> ")?;
            let val = post_untracked_raw
                .replace('\r', "\n")
                .split('\n')
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            Ok::<bool, Box<dyn std::error::Error>>(val == "1")
        })?;

        // Verify end-to-end skip-worktree and assume-unchanged index flag reporting
        env.send_line("touch skip.txt assume.txt && git add skip.txt assume.txt && git commit -m 'init'")?;
        env.send_line("git update-index --skip-worktree skip.txt && git update-index --assume-unchanged assume.txt")?;
        env.wait_for_mut(5, |env| {
            let out = env.send_line(
                "print -r -- \"FLAGS=${VCS_STATUS_NUM_SKIP_WORKTREE}:${VCS_STATUS_NUM_ASSUME_UNCHANGED}\"",
            )?;
            Ok::<bool, Box<dyn Error>>(out.contains("FLAGS=1:1"))
        })?;
        let flags_out = env.send_line(
            "print -r -- \"FLAGS=${VCS_STATUS_NUM_SKIP_WORKTREE}:${VCS_STATUS_NUM_ASSUME_UNCHANGED}\"",
        )?;
        assert!(
            flags_out.contains("FLAGS=1:1"),
            "Expected skip-worktree=1 and assume-unchanged=1, got: {}",
            flags_out
        );

        env.kill_daemon();

        let pre_untracked = pre_untracked_raw
            .replace('\r', "\n")
            .replace(['%', '#'], "")
            .trim()
            .to_string();
        let post_untracked = post_untracked_raw
            .replace('\r', "\n")
            .replace(['%', '#'], "")
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
    fn test_git_status_clears_on_cd_out_of_repo() -> Result<(), Box<dyn Error>> {
        let mut env = TestEnv::new()?;
        let repo_path = env.setup_git_repo("myrepo")?;
        let outside_path = env.home_dir.path().join("outside_repo");
        fs::create_dir_all(&outside_path)?;

        env.start_daemon()?;

        env.send_line("export SHBOT_SET_VCS_STATUS=1")?;
        env.send_line(&format!("cd {}", repo_path.display()))?;
        env.source_plugin()?;

        env.wait_for_mut(5, |env| {
            let out = env.send_line(
                "print -r -- \"IN_REPO=${VCS_STATUS_LOCAL_BRANCH}|${VCS_STATUS_RESULT}\"",
            )?;
            Ok::<bool, Box<dyn Error>>(out.contains("IN_REPO=main|ok-sync"))
        })?;
        let in_repo_out =
            env.send_line("print -r -- \"IN_REPO=${VCS_STATUS_LOCAL_BRANCH}|${VCS_STATUS_RESULT}\"")?;
        assert!(
            in_repo_out.contains("IN_REPO=main|ok-sync"),
            "Expected git status in repo, got: {}",
            in_repo_out
        );

        // Now cd out of the git repository into a non-git directory
        env.send_line(&format!("cd {}", outside_path.display()))?;

        env.wait_for_mut(5, |env| {
            let out = env.send_line(
                "print -r -- \"OUT_REPO=${SHBOT_STATE[git]}|${VCS_STATUS_WORKDIR-unset}|${VCS_STATUS_LOCAL_BRANCH-unset}|${VCS_STATUS_RESULT}\"",
            )?;
            Ok::<bool, Box<dyn Error>>(out.contains("OUT_REPO=|unset|unset|norepo-sync"))
        })?;
        let out_repo = env.send_line(
            "print -r -- \"OUT_REPO=${SHBOT_STATE[git]}|${VCS_STATUS_WORKDIR-unset}|${VCS_STATUS_LOCAL_BRANCH-unset}|${VCS_STATUS_RESULT}\"",
        )?;

        env.kill_daemon();

        assert!(
            out_repo.contains("OUT_REPO=|unset|unset|norepo-sync"),
            "Expected git status to be cleared after cd out of repo, got: {}",
            out_repo
        );

        Ok(())
    }

    #[test]
    fn test_git_watcher_survives_deleted_subdir_and_cd_roundtrip() -> Result<(), Box<dyn Error>> {
        let mut env = TestEnv::new()?;
        let repo_path = env.setup_git_repo("myrepo")?;
        let outside_path = env.home_dir.path().join("outside_repo");
        fs::create_dir_all(&outside_path)?;

        env.start_daemon()?;

        env.send_line("export SHBOT_SET_VCS_STATUS=1")?;
        env.send_line(&format!("cd {}", repo_path.display()))?;
        env.source_plugin()?;

        env.wait_for_mut(5, |env| {
            let out = env.send_line("print -r -- \"U=${VCS_STATUS_NUM_UNTRACKED}\"")?;
            Ok::<bool, Box<dyn Error>>(out.contains("U=0"))
        })?;

        // 1. Create a subdirectory and a file inside it; wait for untracked=1 to prove
        //    notify has registered a recursive inotify watch on the subdirectory.
        env.send_line("mkdir subdir && touch subdir/nested.txt")?;
        env.wait_for_mut(5, |env| {
            let out = env.send_line("print -r -- \"U=${VCS_STATUS_NUM_UNTRACKED}\"")?;
            Ok::<bool, Box<dyn Error>>(out.contains("U=1"))
        })?;

        // 2. Delete the watched subdirectory and wait for untracked=0.
        //    In notify's inotify backend, deleting a watched subdirectory leaves a stale
        //    watch descriptor in notify's internal path map.
        env.send_line("rm -rf subdir")?;
        env.wait_for_mut(5, |env| {
            let out = env.send_line("print -r -- \"U=${VCS_STATUS_NUM_UNTRACKED}\"")?;
            Ok::<bool, Box<dyn Error>>(out.contains("U=0"))
        })?;

        // 3. cd out of the repo so the daemon unwatches repo_path, then cd back in.
        env.send_line(&format!("cd {}", outside_path.display()))?;
        env.wait_for_mut(5, |env| {
            let out = env.send_line("print -r -- \"RES=${VCS_STATUS_RESULT}\"")?;
            Ok::<bool, Box<dyn Error>>(out.contains("RES=norepo-sync"))
        })?;

        env.send_line(&format!("cd {}", repo_path.display()))?;
        env.wait_for_mut(5, |env| {
            let out = env.send_line("print -r -- \"RES=${VCS_STATUS_RESULT}\"")?;
            Ok::<bool, Box<dyn Error>>(out.contains("RES=ok-sync"))
        })?;

        // 4. Now `touch asdf` and `rm asdf` in the repo root MUST immediately update
        //    VCS_STATUS_NUM_UNTRACKED without running `git status`.
        env.send_line("touch asdf")?;
        env.wait_for_mut(5, |env| {
            let out = env.send_line("print -r -- \"AFTER_TOUCH=${VCS_STATUS_NUM_UNTRACKED}\"")?;
            Ok::<bool, Box<dyn Error>>(out.contains("AFTER_TOUCH=1"))
        })?;

        env.send_line("rm asdf")?;
        env.wait_for_mut(5, |env| {
            let out = env.send_line("print -r -- \"AFTER_RM=${VCS_STATUS_NUM_UNTRACKED}\"")?;
            Ok::<bool, Box<dyn Error>>(out.contains("AFTER_RM=0"))
        })?;

        env.kill_daemon();
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
        assert!(
            stdout.contains("configuration reloaded successfully")
                || stdout.contains("failed to load config")
        );

        env.send_line("export SHBOT_SET_VCS_STATUS=1")?;
        env.send_line(&format!("cd {}", repo_path.display()))?;
        env.source_plugin()?;

        let log_path = env
            .home_dir
            .path()
            .join(format!(".local/state/shbot/{}.log", env.host));
        env.wait_for(5, || {
            Ok::<bool, Box<dyn std::error::Error>>(
                std::fs::read_to_string(&log_path)
                    .map(|c| c.contains("Handling cd to"))
                    .unwrap_or(false),
            )
        })?;

        // Test sending 'invalidate' over the socket
        env.send_line(&format!(
            "_shbot_send \"invalidate {}\"",
            repo_path.display()
        ))?;

        env.wait_for(5, || {
            Ok::<bool, Box<dyn std::error::Error>>(
                std::fs::read_to_string(&log_path)
                    .map(|c| c.contains("Invalidate request for:"))
                    .unwrap_or(false),
            )
        })?;

        Ok(())
    }

    #[test]
    fn test_daemon_crash_recovery() -> Result<(), Box<dyn std::error::Error>> {
        let mut env = TestEnv::new()?;

        // 1. Start daemon natively
        env.start_daemon()?;

        let socket_path = env
            .home_dir
            .path()
            .join(format!(".local/state/shbot/{}.socket", env.host));
        assert!(socket_path.exists());

        // 2. Read PID and kill it abruptly (SIGKILL)
        let pid = env.daemon_cmd.as_ref().unwrap().id();
        std::process::Command::new("kill")
            .arg("-KILL")
            .arg(pid.to_string())
            .status()?;

        env.wait_for_mut(5, |env| {
            Ok::<bool, Box<dyn std::error::Error>>(
                env.daemon_cmd
                    .as_mut()
                    .unwrap()
                    .try_wait()
                    .unwrap_or(None)
                    .is_some(),
            )
        })?;

        assert!(
            socket_path.exists(),
            "Socket file should remain after crash"
        );
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

        assert!(
            out.status.success(),
            "Daemon should respond to status after crash recovery"
        );

        // Final cleanup: kill exactly the daemon started by 'connect', scoped by
        // the PID reported in the status dump (never a blanket pkill).
        let status = String::from_utf8_lossy(&out.stdout);
        if let Some(pid_line) = status.lines().find(|l| l.contains("Daemon PID:")) {
            if let Some(pid_str) = pid_line.split(':').nth(1) {
                if let Ok(daemon_pid) = pid_str.trim().parse::<u32>() {
                    let _ = std::process::Command::new("kill")
                        .arg("-TERM")
                        .arg(daemon_pid.to_string())
                        .status();
                }
            }
        }

        Ok(())
    }
    #[test]
    fn test_resource_fd_leak() -> Result<(), Box<dyn std::error::Error>> {
        let mut env = TestEnv::new()?;
        env.start_daemon()?;

        let pid = env.daemon_cmd.as_ref().unwrap().id();

        // Let the daemon settle by waiting for log output
        let log_path = env
            .home_dir
            .path()
            .join(format!(".local/state/shbot/{}.log", env.host));
        env.wait_for(5, || {
            Ok::<bool, Box<dyn std::error::Error>>(
                std::fs::read_to_string(&log_path)
                    .map(|c| c.contains("Binding to socket"))
                    .unwrap_or(false),
            )
        })?;

        let initial_fds = std::fs::read_dir(format!("/proc/{}/fd", pid))?.count();

        let socket_path = env
            .home_dir
            .path()
            .join(format!(".local/state/shbot/{}.socket", env.host));

        // Connect and disconnect rapidly to ensure FDs are released
        for _ in 0..1000 {
            let mut stream = std::os::unix::net::UnixStream::connect(&socket_path)?;
            use std::io::Write;
            stream.write_all(format!("hello {}\n", std::process::id()).as_bytes())?;
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

        let socket_path = env
            .home_dir
            .path()
            .join(format!(".local/state/shbot/{}.socket", env.host));
        let shbot_bin = TestEnv::get_shbot_bin_path()?;

        for _ in 0..200 {
            let temp_dir = tempfile::tempdir()?;
            let mut stream = std::os::unix::net::UnixStream::connect(&socket_path)?;
            use std::io::Write;
            stream.write_all(format!("hello {}\n", std::process::id()).as_bytes())?;
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
            if let Some(watches_line) = lines.iter().find(|&&l| l.contains("Active File Monitors:"))
            {
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
        std::fs::write(
            &config_path,
            r#"
[state.hang_test]
command = "sleep 60"
watch = "**/*.txt"
"#,
        )?;

        env.start_daemon()?;

        let socket_path = env
            .home_dir
            .path()
            .join(format!(".local/state/shbot/{}.socket", env.host));
        let pid = env.daemon_cmd.as_ref().unwrap().id();

        // Connect to trigger a watch
        let temp_dir = tempfile::tempdir()?;
        let mut stream = std::os::unix::net::UnixStream::connect(&socket_path)?;
        use std::io::Write;
        stream.write_all(format!("hello {}\n", std::process::id()).as_bytes())?;
        stream.write_all(format!("cd {}\n", temp_dir.path().display()).as_bytes())?;

        // Create a file to trigger the watch
        std::fs::write(temp_dir.path().join("trigger.txt"), "trigger")?;

        // Wait for the sleep 60 to start
        env.wait_for(5, || {
            let out = std::process::Command::new("ps").arg("-ef").output()?;
            let ps_output = String::from_utf8_lossy(&out.stdout);
            Ok::<bool, Box<dyn std::error::Error>>(
                ps_output
                    .lines()
                    .any(|l| l.contains("sleep 60") && !l.contains("defunct")),
            )
        })?;

        // Drop the client abruptly
        drop(stream);

        // Give it time to clean up
        env.wait_for(5, || {
            let out = std::process::Command::new("ps").arg("-ef").output()?;
            let ps_output = String::from_utf8_lossy(&out.stdout);
            Ok::<bool, Box<dyn std::error::Error>>(
                !ps_output
                    .lines()
                    .any(|l| l.contains("sleep 60") && !l.contains("defunct")),
            )
        })?;

        // Kill the daemon to make sure sleep 60 is either killed or reaped by tokio
        std::process::Command::new("kill")
            .arg("-TERM")
            .arg(pid.to_string())
            .status()?;
        env.wait_for_mut(5, |env| {
            Ok::<bool, Box<dyn std::error::Error>>(
                env.daemon_cmd
                    .as_mut()
                    .unwrap()
                    .try_wait()
                    .unwrap_or(None)
                    .is_some(),
            )
        })?;

        // Check for any zombie processes named "sleep" spawned by us.
        // It's hard to precisely find zombies of a specific process if it's dead,
        // but if tokio cleans up before dying, or if dying cleans it up, we can check `ps`.
        let out = std::process::Command::new("ps").arg("-ef").output()?;
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

        let socket_path = env
            .home_dir
            .path()
            .join(format!(".local/state/shbot/{}.socket", env.host));
        let pid = env.daemon_cmd.as_ref().unwrap().id();

        let log_path = env
            .home_dir
            .path()
            .join(format!(".local/state/shbot/{}.log", env.host));
        env.wait_for(5, || {
            Ok::<bool, Box<dyn std::error::Error>>(
                std::fs::read_to_string(&log_path)
                    .map(|c| c.contains("Binding to socket"))
                    .unwrap_or(false),
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
        for _ in 0..500 {
            let temp_dir = tempfile::tempdir()?;
            let mut stream = std::os::unix::net::UnixStream::connect(&socket_path)?;
            use std::io::Write;
            stream.write_all(format!("hello {}\n", std::process::id()).as_bytes())?;
            stream.write_all(format!("cd {}\n", temp_dir.path().display()).as_bytes())?;
        }

        env.wait_for(10, || {
            let current_fds = std::fs::read_dir(format!("/proc/{}/fd", pid))?.count();
            Ok::<bool, Box<dyn std::error::Error>>(current_fds < initial_fds + 50)
        })?;
        let rss_after_warmup = get_rss()?;

        // Heavy load phase
        for _ in 500..1500 {
            let temp_dir = tempfile::tempdir()?;
            let mut stream = std::os::unix::net::UnixStream::connect(&socket_path)?;
            use std::io::Write;
            stream.write_all(format!("hello {}\n", std::process::id()).as_bytes())?;
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
            rss_after_warmup,
            rss_after_load
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
        let log_path = env
            .home_dir
            .path()
            .join(format!(".local/state/shbot/{}.log", env.host));
        env.wait_for(5, || {
            Ok::<bool, Box<dyn std::error::Error>>(
                std::fs::read_to_string(&log_path)
                    .map(|c| c.contains("Received SIGHUP, reloading configuration"))
                    .unwrap_or(false),
            )
        })?;

        // 4. Verify SIGHUP was received in the log
        let log_path = env
            .home_dir
            .path()
            .join(format!(".local/state/shbot/{}.log", env.host));
        let log_content = std::fs::read_to_string(&log_path).unwrap_or_default();

        assert!(
            log_content.contains("Received SIGHUP, reloading configuration"),
            "SIGHUP should be logged. Log content:\n{}",
            log_content
        );

        let _ = std::process::Command::new("kill")
            .arg("-TERM")
            .arg(pid.to_string())
            .status();
        Ok(())
    }

    #[test]
    fn test_scope_configuration() -> Result<(), Box<dyn std::error::Error>> {
        let mut env = TestEnv::new()?;

        let config_path = env.home_dir.path().join(".config/shbot/shbot.toml");
        std::fs::create_dir_all(config_path.parent().unwrap())?;
        std::fs::write(
            &config_path,
            r#"
[state.scoped_test]
command = "echo 'scoped'"
scope = "nearest(.git)"
"#,
        )?;

        // 1. Start daemon
        env.start_daemon()?;

        let shbot_bin = TestEnv::get_shbot_bin_path()?;

        // Verify initial state is running and parses the scope without crashing
        let out_status = std::process::Command::new(&shbot_bin)
            .arg("status")
            .env("HOME", env.home_dir.path())
            .env("HOST", &env.host)
            .output()?;
        assert!(out_status.status.success(), "Daemon should be running");

        let pid = env.daemon_cmd.as_ref().unwrap().id();
        let _ = std::process::Command::new("kill")
            .arg("-TERM")
            .arg(pid.to_string())
            .status();
        Ok(())
    }

    #[test]
    fn test_binary_resolution_and_auto_download() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::{PermissionsExt, symlink};
        use std::process::Command;

        let mut env = TestEnv::new()?;
        let dummy_sock = env.home_dir.path().join("dummy.sock");
        let _listener = UnixListener::bind(&dummy_sock)?;
        env.send_line(&format!("export SHBOT_SOCKET={}", dummy_sock.display()))?;
        env.send_line("export SHBOT_UNAME_S=Linux")?;
        env.send_line("export SHBOT_UNAME_M=x86_64")?;
        env.source_plugin()?;
        env.send_line("export SHBOT_VERSION=0.1.0")?;

        // Prepare a fake release directory served via file://
        let release_dir = env.home_dir.path().join("releases");
        let build_dir = env.home_dir.path().join("build");
        fs::create_dir_all(&release_dir)?;
        fs::create_dir_all(&build_dir)?;

        let fake_shbot = build_dir.join("shbot");
        fs::write(&fake_shbot, "#!/bin/sh\necho \"downloaded:$*\"\n")?;
        fs::set_permissions(&fake_shbot, fs::Permissions::from_mode(0o755))?;

        let archive_name = "shbot-v0.1.0-linux-x86_64.tar.gz";
        let archive_path = release_dir.join(archive_name);
        let status = Command::new("tar")
            .arg("-C")
            .arg(&build_dir)
            .arg("-czf")
            .arg(&archive_path)
            .arg("shbot")
            .status()?;
        assert!(status.success());

        // 1. Unsupported platforms (not in SHBOT_SUPPORTED_PLATFORMS) must fail immediately
        //    without creating .download.lock or attempting a download.
        env.send_line("export SHBOT_UNAME_S=Darwin")?;
        env.send_line("export SHBOT_UNAME_M=arm64")?;
        env.send_line("_shbot_resolve_bin")?;
        let unsup_rc = env.send_line("print -r -- RC=$?")?;
        assert!(
            unsup_rc.contains("RC=1"),
            "Unsupported platform must not resolve a prebuilt, got: {}",
            unsup_rc
        );
        assert!(
            !env.home_dir
                .path()
                .join(".cache/shbot/.download.lock")
                .exists(),
            "Unsupported platform must not acquire download lock or attempt download"
        );
        env.send_line("export SHBOT_UNAME_S=Linux")?;
        env.send_line("export SHBOT_UNAME_M=x86_64")?;

        // 2. Tampered sha256sums.txt must fail download and leave cache empty
        let sums_path = release_dir.join("sha256sums.txt");
        fs::write(
            &sums_path,
            format!(
                "0000000000000000000000000000000000000000000000000000000000000000  {}\n",
                archive_name
            ),
        )?;
        env.send_line(&format!(
            "export SHBOT_RELEASE_BASE_URL=file://{}",
            release_dir.display()
        ))?;
        env.send_line("_shbot_resolve_bin")?;
        let bad_rc = env.send_line("print -r -- RC=$?")?;
        assert!(
            bad_rc.contains("RC=1"),
            "Tampered checksum must fail, got: {}",
            bad_rc
        );

        // 2. Valid sha256sums.txt is still blocked while retry cooldown is active,
        //    then succeeds once cooldown is cleared.
        let sha_out = Command::new("sha256sum").arg(&archive_path).output()?;
        assert!(sha_out.status.success());
        let sha_hex = String::from_utf8_lossy(&sha_out.stdout)
            .split_whitespace()
            .next()
            .unwrap()
            .to_string();
        fs::write(&sums_path, format!("{}  {}\n", sha_hex, archive_name))?;

        env.send_line("_shbot_resolve_bin")?;
        let cooldown_rc = env.send_line("print -r -- RC=$?")?;
        assert!(
            cooldown_rc.contains("RC=1"),
            "Download must be skipped during retry cooldown, got: {}",
            cooldown_rc
        );

        env.send_line("SHBOT_LAST_DOWNLOAD_FAILURE=0")?;
        let out = env.send_line("shbot status")?;
        assert!(
            out.contains("downloaded:status"),
            "Expected downloaded binary to run after clearing cooldown, got: {}",
            out
        );

        let cached_bin = env
            .home_dir
            .path()
            .join(".cache/shbot/v0.1.0/shbot-linux-x86_64");
        assert!(
            cached_bin.exists(),
            "Cached binary must exist after download"
        );

        // 3. Single-flight flock: if another process populates the binary while holding
        //    .download.lock, _shbot_download_bin reuses it without hitting the network.
        env.send_line("export SHBOT_VERSION=0.1.1")?;
        env.send_line("export SHBOT_RELEASE_BASE_URL=file:///nonexistent")?;
        let v011_dir = env.home_dir.path().join(".cache/shbot/v0.1.1");
        fs::create_dir_all(&v011_dir)?;
        let v011_bin = v011_dir.join("shbot-linux-x86_64");
        fs::write(&v011_bin, "#!/bin/sh\necho \"flock-populated:$*\"\n")?;
        fs::set_permissions(&v011_bin, fs::Permissions::from_mode(0o755))?;
        let flock_out = env.send_line(
            "_shbot_download_bin linux-x86_64 ~/.cache/shbot/v0.1.1/shbot-linux-x86_64 && shbot status",
        )?;
        assert!(
            flock_out.contains("flock-populated:status"),
            "Expected flock double-check to succeed without downloading, got: {}",
            flock_out
        );

        // 4. SemVer-aware offline fallback:
        //    When SHBOT_VERSION=0.1.5 and download fails (offline):
        //    - v0.0.9 and v0.2.0 are rejected (incompatible 0.Y minor epoch)
        //    - v0.1.0 and v0.1.1 are compatible, and the highest patch (v0.1.1) is chosen.
        fs::remove_dir_all(env.home_dir.path().join(".cache/shbot"))?;
        for (ver, tag) in [("0.0.9", "v009"), ("0.2.0", "v020")] {
            let dir = env.home_dir.path().join(format!(".cache/shbot/v{}", ver));
            fs::create_dir_all(&dir)?;
            let bin = dir.join("shbot-linux-x86_64");
            fs::write(&bin, format!("#!/bin/sh\necho \"{}:$*\"\n", tag))?;
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o755))?;
        }
        env.send_line("export SHBOT_VERSION=0.1.5")?;
        env.send_line("SHBOT_LAST_DOWNLOAD_FAILURE=0")?;
        env.send_line("_shbot_resolve_bin")?;
        let incomp_rc = env.send_line("print -r -- RC=$?")?;
        assert!(
            incomp_rc.contains("RC=1"),
            "Incompatible 0.0.x and 0.2.x cached versions must be rejected for 0.1.5, got: {}",
            incomp_rc
        );

        for (ver, tag) in [("0.1.0", "v010"), ("0.1.3", "v013")] {
            let dir = env.home_dir.path().join(format!(".cache/shbot/v{}", ver));
            fs::create_dir_all(&dir)?;
            let bin = dir.join("shbot-linux-x86_64");
            fs::write(&bin, format!("#!/bin/sh\necho \"{}:$*\"\n", tag))?;
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o755))?;
        }
        let fallback_out = env.send_line("shbot status")?;
        assert!(
            fallback_out.contains("v013:status"),
            "Expected highest compatible 0.1.x cached version (v0.1.3) to be chosen, got: {}",
            fallback_out
        );

        //    For 1.Y.Z (major >= 1), major version is the protocol epoch:
        //    SHBOT_VERSION=1.2.0 accepts v1.0.4 and v1.1.8 (choosing v1.1.8) and rejects v2.0.0.
        for (ver, tag) in [("1.0.4", "v104"), ("1.1.8", "v118"), ("2.0.0", "v200")] {
            let dir = env.home_dir.path().join(format!(".cache/shbot/v{}", ver));
            fs::create_dir_all(&dir)?;
            let bin = dir.join("shbot-linux-x86_64");
            fs::write(&bin, format!("#!/bin/sh\necho \"{}:$*\"\n", tag))?;
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o755))?;
        }
        env.send_line("export SHBOT_VERSION=1.2.0")?;
        let fallback_v1 = env.send_line("shbot status")?;
        assert!(
            fallback_v1.contains("v118:status"),
            "Expected highest compatible 1.x cached version (v1.1.8) to be chosen, got: {}",
            fallback_v1
        );

        // 5. Local dev-bin symlink takes precedence over cached release binary
        let state_dir = env.home_dir.path().join(".local/state/shbot");
        fs::create_dir_all(&state_dir)?;
        let dev_target = env.home_dir.path().join("shbot-dev");
        fs::write(&dev_target, "#!/bin/sh\necho \"dev-bin:$*\"\n")?;
        fs::set_permissions(&dev_target, fs::Permissions::from_mode(0o755))?;
        symlink(&dev_target, state_dir.join("dev-bin"))?;

        let out = env.send_line("shbot reload")?;
        assert!(
            out.contains("dev-bin:reload"),
            "Expected dev-bin to override cached binary, got: {}",
            out
        );

        // 4. Explicit SHBOT_BIN takes highest precedence over dev-bin and cached binary
        let explicit_bin = env.home_dir.path().join("shbot-explicit");
        fs::write(&explicit_bin, "#!/bin/sh\necho \"explicit:$*\"\n")?;
        fs::set_permissions(&explicit_bin, fs::Permissions::from_mode(0o755))?;
        env.send_line(&format!("export SHBOT_BIN={}", explicit_bin.display()))?;

        let out = env.send_line("shbot loglevel debug")?;
        assert!(
            out.contains("explicit:loglevel debug"),
            "Expected SHBOT_BIN to override dev-bin, got: {}",
            out
        );

        Ok(())
    }

    #[test]
    fn test_zsh_payload_percent_encoding_edge_cases() -> Result<(), Box<dyn Error>> {
        use std::os::unix::fs::PermissionsExt;

        let mut env = TestEnv::new()?;
        let dummy_sock = env.home_dir.path().join("encoding.sock");
        let listener = UnixListener::bind(&dummy_sock)?;

        let shbot_bin = env.home_dir.path().join("shbot");
        fs::write(&shbot_bin, "#!/bin/sh\nexit 0\n")?;
        fs::set_permissions(&shbot_bin, fs::Permissions::from_mode(0o755))?;

        env.send_line(&format!(
            "export PATH={}:$PATH",
            env.home_dir.path().display()
        ))?;
        env.send_line(&format!("export SHBOT_SOCKET={}", dummy_sock.display()))?;
        env.source_plugin()?;

        let (mut stream, _) = listener.accept()?;
        let mut buf = [0u8; 256];
        let _ = stream.read(&mut buf)?;

        // Send wire payload containing:
        // - literal "%01" (encoded as "%2501")
        // - literal "%\x01" (encoded as "%25%01")
        // - literal "\x01" (encoded as "%01")
        // - literal "%25" (encoded as "%2525")
        // - newline (%0A) and unit separator (%1F)
        stream.write_all(b"update tricky lit:%2501|pct_soh:%25%01|soh:%01|pct25:%2525|nl:%0A|us:%1F\n")?;
        stream.flush()?;

        env.wait_for_mut(5, |env| {
            let check = env.send_line(
                "print -rn -- \"${SHBOT_STATE[tricky]}\" | od -An -tx1 | tr -d ' \\n'; echo",
            )?;
            // Expected bytes for "lit:%01|pct_soh:%\x01|soh:\x01|pct25:%25|nl:\n|us:\x1f":
            // l i t : % 0 1 | p c t _ s o h : % 01 | s o h : 01 | p c t 2 5 : % 2 5 | n l : 0a | u s : 1f
            Ok::<bool, Box<dyn Error>>(
                check.contains("6c69743a2530317c7063745f736f683a25017c736f683a017c70637432353a2532357c6e6c3a0a7c75733a1f"),
            )
        })?;

        Ok(())
    }

    #[test]
    fn test_custom_states_scopes_and_invalidation_end_to_end() -> Result<(), Box<dyn Error>> {
        let mut env = TestEnv::new()?;

        let proj_dir = env.home_dir.path().join("proj");
        let sub_dir = proj_dir.join("sub");
        fs::create_dir_all(&sub_dir)?;
        fs::write(proj_dir.join("VERSION"), "1.0.0\n")?;
        fs::write(sub_dir.join("watched.txt"), "w1\n")?;
        let counter_path = env.home_dir.path().join("counter.txt");
        fs::write(&counter_path, "c1\n")?;

        let config_path = env.home_dir.path().join(".config/shbot/shbot.toml");
        fs::create_dir_all(config_path.parent().unwrap())?;
        fs::write(
            &config_path,
            format!(
                r#"
[state.ver]
scope = "nearest(VERSION)"
source.file = "VERSION"

[state.watched_cmd]
scope = "cwd"
command = "cat watched.txt"
watch = "watched.txt"

[state.explicit_global]
scope = "global"
source.command = "cat {}"
"#,
                counter_path.display()
            ),
        )?;

        env.start_daemon()?;
        let shbot_bin = TestEnv::get_shbot_bin_path()?;
        env.send_line(&format!("export SHBOT_BIN={}", shbot_bin.display()))?;
        env.source_plugin()?;

        env.send_line(&format!("cd {}", sub_dir.display()))?;

        env.wait_for_mut(5, |env| {
            let out = env.send_line(
                "echo \"V=${SHBOT_STATE[ver]}|W=${SHBOT_STATE[watched_cmd]}|E=${SHBOT_STATE[explicit_global]}\"",
            )?;
            Ok::<bool, Box<dyn Error>>(out.contains("V=1.0.0|W=w1|E=c1"))
        })?;

        // Modify VERSION and watched.txt -> file monitor should automatically push updates
        fs::write(proj_dir.join("VERSION"), "2.0.0\n")?;
        fs::write(sub_dir.join("watched.txt"), "w2\n")?;

        env.wait_for_mut(5, |env| {
            let out = env.send_line(
                "echo \"V=${SHBOT_STATE[ver]}|W=${SHBOT_STATE[watched_cmd]}\"",
            )?;
            Ok::<bool, Box<dyn Error>>(out.contains("V=2.0.0|W=w2"))
        })?;

        // Update counter.txt and send `invalidate explicit_global` -> should re-evaluate and push c2
        fs::write(&counter_path, "c2\n")?;
        env.send_line("_shbot_send 'invalidate explicit_global'")?;

        env.wait_for_mut(5, |env| {
            let out = env.send_line("echo \"E=${SHBOT_STATE[explicit_global]}\"")?;
            Ok::<bool, Box<dyn Error>>(out.contains("E=c2"))
        })?;

        Ok(())
    }

    #[test]
    fn test_dynamic_config_hot_reload_and_sighup_end_to_end() -> Result<(), Box<dyn Error>> {
        use std::process::Command;

        let mut env = TestEnv::new()?;
        let config_path = env.home_dir.path().join(".config/shbot/shbot.toml");
        fs::create_dir_all(config_path.parent().unwrap())?;
        fs::write(
            &config_path,
            r#"
[state.initial_state]
command = "echo init_ok"
"#,
        )?;

        env.start_daemon()?;
        let shbot_bin = TestEnv::get_shbot_bin_path()?;
        env.send_line(&format!("export SHBOT_BIN={}", shbot_bin.display()))?;
        env.source_plugin()?;

        env.wait_for_mut(5, |env| {
            let out = env.send_line("echo \"INIT=${SHBOT_STATE[initial_state]}\"")?;
            Ok::<bool, Box<dyn Error>>(out.contains("INIT=init_ok"))
        })?;

        // 1. Add a new state and run `shbot reload` without changing directory
        fs::write(
            &config_path,
            r#"
[state.initial_state]
command = "echo init_ok"

[state.hot_added]
command = "echo hot_ok"
"#,
        )?;
        env.wait_for_mut(5, |env| {
            let reload_out = env.send_line("shbot reload")?;
            Ok::<bool, Box<dyn Error>>(
                reload_out.contains("configuration reloaded successfully"),
            )
        })?;

        env.wait_for_mut(5, |env| {
            let out = env.send_line("echo \"HOT=${SHBOT_STATE[hot_added]}\"")?;
            Ok::<bool, Box<dyn Error>>(out.contains("HOT=hot_ok"))
        })?;

        // 2. Add another state and reload via SIGHUP
        fs::write(
            &config_path,
            r#"
[state.initial_state]
command = "echo init_ok"

[state.sighup_added]
command = "echo sighup_ok"
"#,
        )?;
        let pid = env.daemon_cmd.as_ref().unwrap().id();
        Command::new("kill")
            .arg("-HUP")
            .arg(pid.to_string())
            .status()?;

        env.wait_for_mut(5, |env| {
            let out = env.send_line("echo \"HUP=${SHBOT_STATE[sighup_added]}\"")?;
            Ok::<bool, Box<dyn Error>>(out.contains("HUP=sighup_ok"))
        })?;

        // 3. Invalid TOML on SIGHUP and `shbot reload` must log error and keep daemon alive
        fs::write(&config_path, "invalid = [toml\n")?;
        Command::new("kill")
            .arg("-HUP")
            .arg(pid.to_string())
            .status()?;
        env.wait_for_mut(5, |env| {
            let bad_reload = env.send_line("shbot reload")?;
            Ok::<bool, Box<dyn Error>>(bad_reload.contains("error: failed to load config"))
        })?;

        Ok(())
    }

    #[test]
    fn test_broken_pipe_recovery_and_version_auto_upgrade() -> Result<(), Box<dyn Error>> {
        use std::process::Command;
        use std::thread;

        let mut env = TestEnv::new()?;
        env.start_daemon()?;
        let old_pid = env.daemon_cmd.as_ref().unwrap().id();

        let shbot_bin = TestEnv::get_shbot_bin_path()?;
        env.send_line(&format!("export SHBOT_BIN={}", shbot_bin.display()))?;
        env.source_plugin()?;

        // 1. Kill the daemon so the Zsh client's socket FD becomes a broken pipe,
        //    then start a fresh daemon and verify the Zsh plugin detects the broken pipe
        //    on _shbot_send, resets SHBOT_FD=0, and reconnects on the next prompt!
        env.kill_daemon();

        let repo_path = env.setup_git_repo("reconnect_repo")?;
        fs::write(repo_path.join("a.txt"), "hi")?;

        env.start_daemon()?;
        let new_pid = env.daemon_cmd.as_ref().unwrap().id();
        assert_ne!(old_pid, new_pid);

        // First command triggers _shbot_send on the dead FD (cleaning it up) and/or precmd reconnect
        env.send_line(&format!("cd {}", repo_path.display()))?;
        env.send_line("")?;

        env.wait_for_mut(5, |env| {
            let out = env.send_line("echo \"GIT=${SHBOT_STATE[git]}\"")?;
            Ok::<bool, Box<dyn Error>>(out.contains("reconnect_repo"))
        })?;

        // 2. Kill the current daemon, bind a mock older daemon (v0.1.0) on the socket,
        //    and run `shbot --log-file <file> connect` to verify auto-upgrade and custom log file!
        env.kill_daemon();
        let socket_path = env
            .home_dir
            .path()
            .join(format!(".local/state/shbot/{}.socket", env.host));
        let _ = fs::remove_file(&socket_path);
        let mock_old_listener = UnixListener::bind(&socket_path)?;
        thread::spawn(move || {
            if let Ok((mut stream, _)) = mock_old_listener.accept() {
                let mut buf = [0u8; 64];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(b"shbot daemon status:\n  Daemon Version: 0.1.0\n");
            }
        });

        let custom_log = env.home_dir.path().join("upgrade.log");
        let status = Command::new(&shbot_bin)
            .args(["--log-file", custom_log.to_str().unwrap(), "connect"])
            .env("HOME", env.home_dir.path())
            .env("HOST", &env.host)
            .env("RUST_LOG", "info")
            .status()?;
        assert!(
            status.success(),
            "connect upgrading older daemon should succeed"
        );
        assert!(custom_log.exists(), "--log-file should create the log file");

        env.wait_for(5, || {
            let out = Command::new(&shbot_bin)
                .arg("status")
                .env("HOME", env.home_dir.path())
                .env("HOST", &env.host)
                .output()?;
            let stdout = String::from_utf8_lossy(&out.stdout);
            Ok::<bool, Box<dyn Error>>(
                stdout.contains("Daemon Version:") && !stdout.contains("0.1.0"),
            )
        })?;

        Ok(())
    }
}

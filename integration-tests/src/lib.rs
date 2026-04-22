#[cfg(test)]
mod tests {
    use rexpect::session::spawn;
    use std::os::unix::net::UnixListener;
    
    // Quick test to ensure that the Zsh plugin sources correctly and spawns the shbot binary
    #[test]
    fn test_zsh_plugin_bootstrap() -> anyhow::Result<()> {
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
                Ok((s, _)) => { stream_opt = Some(s); break; },
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if start_time.elapsed().as_secs() > 1 {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(e) => return Err(e.into()),
            }
        };
        
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
}

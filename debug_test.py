import pexpect
import tempfile
import socket
import os

with tempfile.TemporaryDirectory() as d:
    sock_path = os.path.join(d, "dummy.sock")
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.bind(sock_path)
    s.listen(1)

    p = pexpect.spawn("env PROMPT='> ' zsh -f -i +Z", encoding='utf-8')
    p.expect("> ")
    print("Got initial prompt")

    p.sendline(f"export SHBOT_SOCKET={sock_path}")
    p.expect("> ")
    print("Exported socket")

    plugin_path = os.path.abspath("zsh/shbot.plugin.zsh")
    p.sendline(f"source {plugin_path}")
    p.expect("> ")
    print("Sourced plugin")

    p.sendline("")
    p.expect("> ")
    print("Triggered precmd")

    s.settimeout(2.0)
    try:
        conn, _ = s.accept()
        print("ACCEPTED CONNECTION")
        print("Data:", conn.recv(1024))
    except Exception as e:
        print("Failed to accept:", e)
        # Try to read whatever zsh printed
        print("Zsh output:")
        try:
            p.sendline("echo 'printing internal states'")
            p.expect("> ")
            print(p.before)
            p.sendline("echo $SHBOT_FD")
            p.expect("> ")
            print(p.before)
        except Exception:
            pass

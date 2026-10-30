# Shbot Architecture

This document describes the high-level architecture of `shbot`, a daemon and Zsh
integration that provides background tasks, asynchronous Git status updates, and
file-watching state updates directly to your shell without blocking.

`shbot` consists of three primary domains: the Rust Daemon, the Zsh Plugin, and
the Integration Test Harness.

## 1. Rust Daemon (`daemon/`)

The `shbot` daemon is written in Rust and uses the `tokio` asynchronous runtime.
It acts as a central hub for all active shell sessions, monitoring file systems,
and broadcasting state updates back to specific shells.

### Key Components:

- **`main.rs` & `cli.rs`:** Handles argument parsing using `clap`. Supports
  commands like `daemon` (to start the background process via a self-`spawn()`
  of the current executable if not socket-activated), `connect` (for shells to
  initially bootstrap), `status` (for troubleshooting), `reload` (for dynamic
  config reload) and `loglevel` (for dynamic log filtering).
- **`server.rs`:** Manages the UNIX socket listener
  (`~/.local/state/shbot/<HOST>.socket`). Every accepted connection is
  authenticated with kernel peer credentials (`SO_PEERCRED`): connections from a
  different uid are rejected, and a client's `hello <pid>` registration is only
  accepted when it matches the kernel-provided pid, preventing PID-spoofing and
  client hijacking. The socket file is created with mode `0600` and the state
  directory with `0700`, independent of the process umask. The server enforces
  hard resource limits: a 64 KiB cap per protocol line, at most 128 concurrent
  connections, and an idle timeout (`daemon.idle_timeout_sec`) that disconnects
  connections with no client commands or server pushes within the configured
  window. A second daemon instance refuses to bind over a live socket instead of
  unlinking it.
- **`state.rs` (`DaemonState`):** A thread-safe, central registry of all
  connected `ClientInfo`s (using `tokio::sync::RwLock` around a `HashMap`). It
  maps PID to Working Directory (CWD) and holds a messaging channel
  (`mpsc::Sender<String>`) bridging the async environment to the socket writer.
  An in-flight set deduplicates concurrent evaluations of the same scope so that
  slow or hanging tasks are not re-spawned on every trigger.
- **`watcher.rs` (`SyncWatcher`):** Leverages the `notify` crate to hook into
  OS-level filesystem events securely. When a directory tree changes, it
  immediately identifies intersecting CWDs of connected clients and triggers
  both Git and File Watch Task re-evaluations. Event bursts flow through a
  bounded channel (excess events are dropped and re-detected on the next change)
  and recursive watches are capped at 256 paths; the daemon refuses to
  recursively watch `/`, `/proc`, `/sys`, `/dev`, and `/run`.
- **`runner.rs`:** Handles user-defined `shbot.toml` configurations. Executes
  background tasks ranging from simple `timeout` recurring actions to targeted
  `watch` state invalidations on specific paths. Supports directory scoping (via
  `scope.rs`) allowing configurations to dynamically bind to project roots (like
  the nearest `.git` directory). Commands are executed with `sh -c` under strict
  resource controls: stdin is attached to `/dev/null`, the child is killed when
  the future is dropped (`kill_on_drop`), execution is bounded by
  `daemon.command_timeout_sec`, and captured output is capped at 64 KiB. It
  securely pushes executed outputs to matching clients over the socket via
  `broadcast_to_cwd`.
- **`git.rs`:** Inspects the current state of a Git repository quickly using the
  pure-Rust `gix` (`gitoxide`) library, determining dirty states or pending
  commits dynamically when a client navigates into an active tree or when the
  `watcher` detects modifications.

## 2. Bootstrapping and Daemon Start-Up Process

The integration heavily relies on instantaneous connection bootstrapping without
requiring polling loops or external supervisors. The start-up sequence operates
using `systemd` socket activation or a built-in self-spawn fallback mechanism:

1. **Systemd Socket Activation:** By default, the `install.sh` configures
   systemd user services (`shbot.socket` and `shbot.service`). When `shbot` (or
   Zsh) tries to communicate with the socket, systemd automatically spawns the
   daemon and passes the active `UnixListener` via the `ListenFd` crate. systemd
   pre-creates the socket with `SocketMode=0600`.
2. **Shell Check:** When the Zsh plugin starts, it uses the native `zsocket`
   command to immediately test the socket file
   (`~/.local/state/shbot/<HOST>.socket`). If the socket responds, connection
   completes instantly.
3. **Binary Resolution & Lazy Download (`_shbot_resolve_bin`):** If no daemon is
   listening (and systemd is unavailable), the Zsh plugin locates the `shbot`
   executable in priority order:
   - `$SHBOT_BIN` (explicit environment override)
   - `${XDG_STATE_HOME:-$HOME/.local/state}/shbot/dev-bin` (local development
     symlink managed by `mise run dev-link` / `mise run dev-unlink`)
   - `${XDG_CACHE_HOME:-$HOME/.cache}/shbot/v<SHBOT_VERSION>/shbot-<os>-<arch>`
     (cached release binary)
   - If the cached release binary is missing, the plugin acquires a
     single-flight file lock (`zsystem flock` on
     `~/.cache/shbot/.download.lock`), re-checks whether another concurrent
     shell already populated the binary, and otherwise downloads the matching
     prebuilt tarball and `sha256sums.txt` from GitHub Releases with bounded
     connect/transfer timeouts (`--connect-timeout 2` / `--max-time 10`),
     verifies the SHA-256 digest, and atomically extracts the binary into the
     cache directory (`0700`).
   - If the download fails (e.g. while offline), the plugin records a retry
     cooldown (`SHBOT_DOWNLOAD_RETRY_COOLDOWN`, default 300 seconds) so
     subsequent prompts do not repeatedly stall on network timeouts, and falls
     back to the highest SemVer-compatible version already cached in
     `~/.cache/shbot/v*/`.
4. **Foreground Launcher (`shbot connect`):** Once resolved, the shell
   synchronously runs `<shbot_bin> connect`.
5. **Stale Socket Cleanup:** The launcher probes the existing socket file; a
   stale leftover from a crashed daemon is unlinked, while a live socket ends
   the bootstrap immediately. The daemon itself repeats this liveness check and
   refuses to bind over a socket owned by another live daemon.
6. **Self-Spawn:** The launcher spawns `<current_exe> daemon` (fork + exec) with
   stdio redirected to the per-host log file. The daemon process creates the
   state directory (`0700`) and binds the socket (`0600`) itself.
7. **Startup Verification:** The launcher polls briefly (bounded to ~500 ms)
   until the socket responds, then returns control to Zsh, which completes the
   `zsocket` connection with zero additional delay.

## 3. Zsh Integration (`zsh/`)

The shell plugin (`shbot.plugin.zsh`) avoids the slowness of traditional
synchronous prompts (like checking Git status before drawing `$PS1`) by
connecting to the `shbot` daemon in the background.

### How it works:

- **`zsh/net/socket`:** The plugin establishes a UNIX socket connection to the
  daemon on startup.
- **Asynchronous Reads (`zle -F`):** Instead of blocking the shell to wait for
  `shbot`, it registers a file descriptor to the Zsh Line Editor (`ZLE`). When
  the daemon finishes evaluating something (e.g., Git state or a file trigger)
  and pushes data across the socket, Zsh silently interrupts, reads the payload,
  updates internal variables (like `$SHBOT_STATE`), and safely issues a
  `zle .reset-prompt` to visually reflect the new state.
- **Hooks (`add-zsh-hook`):** Uses Zsh lifecycle hooks to notify the daemon of
  state changes:
  - `chpwd`: When the user uses `cd`, the plugin sends `cd /new/path` to the
    daemon, instantly triggering specific git re-evaluation or watch tasks.
  - `zshexit`: Properly cleans up file descriptors and the socket stream when
    the shell exits.

### Theme & API Integrations

The integration supports seamless migration from the widely used
`romkatv/gitstatus` daemon. By setting `SHBOT_SET_VCS_STATUS=1`, `shbot` acts as
an API proxy, unpacking its asynchronous state payloads to populate standard
`VCS_STATUS_*` variables directly in the background. Combined with a few mocked
shell functions (like `gitstatus_query`), this natively exposes `shbot`'s
high-performance backend to highly demanding themes like `powerlevel10k` without
needing any theme modification.

## 4. Integration Tests (`integration-tests/`)

Because the Zsh plugin relies heavily on ZLE (`zle -F`), simple script execution
(like testing a standalone bash script) wouldn't correctly simulate the
interactive prompt environment.

### Harness Strategy:

- **`rexpect` PTY Control:** The testing harness spawns a true, interactive Zsh
  instance using a Pseudo-Terminal (PTY) so that ZLE works correctly.
- **Dummy Backend:** Instead of relying on the real Rust daemon, the test
  harness sets up its own temporary UNIX socket and compiles a fake `shbot`
  binary in a TempDir. The fake binary correctly bootstraps the plugin
  integration.
- **Automated Socket Assertions:** The test harness directly sends mock
  `update git dirty` protocols across the socket. It waits to allow Zsh to
  asynchronously parse the payload, then safely sends keyboard commands (e.g.,
  `echo $SHBOT_STATE`) to verify that the internal shell state matched the
  server payload correctly.

## 5. Client/Server Protocol

The communication between the Zsh clients and the central Rust daemon utilizes
newline-delimited text over UNIX domain sockets. Using a simple text protocol
makes it easy to integrate with native shell tools and enables straightforward
manual debugging.

### Client to Daemon Commands:

- `hello <pid>`: Sent by the shell upon startup to register its unique Process
  ID with the daemon. The daemon validates the claimed pid against the
  connection's kernel peer credentials (`SO_PEERCRED`); a mismatching pid is
  rejected with an `error:` reply and the connection is closed.
- `cd <path>`: Alerts the daemon that the user has changed directories. This
  immediately triggers Git state evaluation and any matching watch tasks for the
  new directory.
- `status`: Sent by the `shbot status` CLI command to request a dump of the
  daemon's internal state (including the daemon PID, every client's PID and CWD,
  and state metrics). The daemon will respond and then immediately close the
  connection.
- `reload`: Sent by the `shbot reload` CLI command. Forces the daemon to reload
  its configuration from disk and apply it immediately.
- `loglevel <level>`: Sent by `shbot loglevel <level>`. Modifies the daemon's
  running `EnvFilter` without restarting.
- `invalidate <target>`: Forces the daemon to discard and re-evaluate cached
  state.

### Daemon to Client Events:

- `update <state_id> <payload>`: Asynchronous broadcast sent from the daemon to
  the client whenever background tasks complete or file system changes are
  detected. Modifies the shell's prompt securely without blocking input.
  Examples: `update git dirty`, `update periodic true`.
- The payload is percent-encoded on the wire (`\n` as `%0A`, `\r` as `%0D`, the
  unit separator as `%1F`, `%` as `%25`) so that arbitrary task output, file
  contents, or git data (remote URLs, commit summaries) can never inject forged
  protocol lines into the shell. The Zsh plugin decodes the escapes before
  storing the value in `$SHBOT_STATE`, so users always observe the raw payload.
- `shbot daemon status:\n...`: A multi-line response provided to the
  `shbot status` CLI command containing detailed client and metric information.

### Protocol Versioning & SemVer Compatibility

The Zsh plugin and Rust daemon follow Cargo-style Semantic Versioning
(`MAJOR.MINOR.PATCH`) for the socket protocol and CLI contract:

- **Pre-1.0 (`0.Y.Z`)**: The **minor** version `Y` defines the protocol epoch.
  Patch releases (`0.Y.0` → `0.Y.1`) are protocol-compatible; breaking protocol
  changes bump `Y` (`0.1.x` → `0.2.0`).
- **1.0 and later (`X.Y.Z`, `X >= 1`)**: The **major** version `X` defines the
  protocol epoch. Minor and patch releases within `X.*.*` preserve protocol
  compatibility; breaking protocol changes bump `X`.
- When offline and the target `v<SHBOT_VERSION>` binary is not yet cached,
  `_shbot_find_fallback_bin` only falls back to cached binaries matching the
  current protocol epoch (selecting the highest compatible version) and refuses
  incompatible epochs.

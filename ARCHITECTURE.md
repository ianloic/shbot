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
  commands like `daemon` (to start the background process using double-fork
  isolation via `daemonize` if not socket-activated), `connect` (for shells to
  initially bootstrap), `status` (for troubleshooting), `reload` (for dynamic
  config reload) and `loglevel` (for dynamic log filtering).
- **`server.rs`:** Manages the UNIX socket listener
  (`~/.local/state/shbot/<HOST>.socket`). It accepts incoming shell connections,
  registers them, and handles incoming commands (like `cd`) to update the
  daemon's awareness of where the shell is currently operating.
- **`state.rs` (`AppState`):** A thread-safe, central registry of all connected
  `ClientInfo`s (using `tokio::sync::RwLock` around a `HashMap`). It maps PID to
  Working Directory (CWD) and holds a messaging channel (`mpsc::Sender<String>`)
  bridging the async environment to the socket writer.
- **`watcher.rs` (`SyncWatcher`):** Leverages the `notify` crate to hook into
  OS-level filesystem events securely. When a directory tree changes, it
  immediately identifies intersecting CWDs of connected clients and triggers
  both Git and File Watch Task re-evaluations.
- **`runner.rs`:** Handles user-defined `shbot.toml` configurations. Executes
  background tasks ranging from simple `timeout` recurring actions to targeted
  `watch` state invalidations on specific paths. It securely pushes executed
  outputs to matching clients over the socket via `broadcast_to_cwd`.
- **`git.rs`:** Inspects the current state of a Git repository quickly,
  determining dirty states or pending commits dynamically when a client
  navigates into an active tree or when the `watcher` detects modifications.

## 2. Bootstrapping and Daemon Start-Up Process

The integration heavily relies on instantaneous connection bootstrapping without
requiring polling loops or external supervisors. The start-up sequence operates
using `systemd` socket activation or a built-in `fork()` fallback mechanism:

1. **Systemd Socket Activation:** By default, the `install.sh` configures
   systemd user services (`shbot.socket` and `shbot.service`). When `shbot` (or
   Zsh) tries to communicate with the socket, systemd automatically spawns the
   daemon and passes the active `UnixListener` via the `ListenFd` crate.
2. **Shell Check:** When the Zsh plugin starts, it uses the native `zsocket`
   command to immediately test the socket file
   (`~/.local/state/shbot/<HOST>.socket`). If the socket responds, connection
   completes instantly.
3. **Foreground Launcher (`shbot connect`):** If no daemon is listening (and
   systemd is unavailable), the shell synchronously runs `shbot connect`.
4. **Early Bind and Validation:** The `connect` launcher instantly drops any
   stale socket left over from a previous crash and securely binds to the new
   `UnixListener` _in the foreground process_.
5. **Daemonize Double-Fork:** The launcher then uses the `daemonize` crate to
   safely double-fork into the background and isolate the process. Because this
   is purely an OS-level `fork()` (without `exec()`), the background process
   perfectly inherits the active file descriptor of the bound `UnixListener`.
6. **Instant Guarantee:** The parent launcher safely terminates knowing the
   socket is completely operational. When control is returned to Zsh, the shell
   plugin verifies the new socket instantly with a zero-delay `zsocket` success.

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
  ID with the daemon.
- `cd <path>`: Alerts the daemon that the user has changed directories. This
  immediately triggers Git state evaluation and any matching watch tasks for the
  new directory.
- `status`: Sent by the `shbot status` CLI command to request a dump of the
  daemon's internal state. The daemon will respond and then immediately close
  the connection.
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
- `shbot daemon status:\n...`: A multi-line response provided to the
  `shbot status` CLI command containing detailed client and metric information.

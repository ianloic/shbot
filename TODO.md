# Project TODOs

This document tracks incomplete architectural features and future improvements
required to make `shbot` production-ready.

## 1. Packaging and Systemd Integration

- [x] Create `shbot.service` and `shbot.socket` configuration files for systemd
      user service deployment leveraging `listenfd`.
- [x] Create an `install.sh` script or `Makefile` to automatically build
      `shbot`, install the binaries to `~/.cargo/bin`, and configure `~/.zshrc`.

## 2. Advanced "File-Change" Trigger Patterns

- [x] Update the `evalute_file_triggers` engine to support standard glob
      patterns (e.g. `**/*.rs` or `src/*.js`) instead of relying solely on exact
      absolute/relative paths.

## 3. Protocol Expansion

- [x] Fully implement the `invalidate <query>` UNIX socket command to allow
      power-users to wipe out stale daemon caches for immediate sync debugging.
- [x] Add a `shbot` subcommand to configure the daemon's log level dynamically
      while it's running. This requires adding a new UNIX socket protocol
      command.

## 4. Path and File Conventions

- [x] Move the socket and other runtime files from `~/.local/state/shbot/socket`
      directly to `~/.local/state/shbot`.
- [x] Name the `.pid` and `.log` files after the hostname to prevent collisions
      between hosts, matching the `.socket` convention.

## 5. Bugs and Stability Issues

- [x] **Stale state recovery**: `shbot` fails to start if there are stale
      socket/pid files. It incorrectly assumes it can connect or lock them. This
      needs to be handled cleanly (e.g. unlinking dead sockets).
- [x] **Update loop spam**: `shbot` spams the same state update repititively. It
      shouldn't send identical state updates over and over again, and it
      shouldn't re-scan the git repo unless something actually changes.

## 6. Test Coverage Improvements

The project currently lacks granular test coverage. The following specific
sections of code and functionality need unit/integration tests to ensure
stability:

- [x] **`daemon/src/git.rs`**: Unit tests for parsing the
      `git status --porcelain` output, especially handling unusual states (e.g.
      detached HEAD, complex staged/unstaged combinations, merge conflicts, and
      uninitialized repos).
- [x] **`daemon/src/server.rs`**: Protocol parsing tests to verify `cd`,
      `hello`, `invalidate <query>`, and log-level configuration commands are
      correctly interpreted from socket streams.
- [x] **`daemon/src/runner.rs`**: Unit tests for `evaluate_file_triggers` to
      ensure glob patterns (e.g. `**/*.rs`) match properly and avoid false
      positives/negatives when checking modified files.
- [x] **`daemon/src/state.rs`**: Caching tests to verify diffing functionality
      accurately prevents redundant identical state updates from being spammed
      to clients.
- [x] **`daemon/src/config.rs`**: Unit tests validating configuration file
      loading and fallback behaviors.
- [x] **Zsh Plugin / System Daemon recovery**: Expand `integration-tests` to
      simulate crash recovery (e.g. stale `.pid` and `.socket` cleanup
      capability) and robust reconnection logic without panicking.

## 7. API and Theme Integrations

- [x] Create a drop-in replacement API for `gitstatusd` (e.g.
      `romkatv/gitstatus`) to allow immediate migration for complex prompt
      themes like `powerlevel10k`.

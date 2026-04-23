# Project TODOs

This document tracks incomplete architectural features and future improvements
required to make `shbot` production-ready.

## 1. Packaging and Systemd Integration

- [ ] Create `shbot.service` and `shbot.socket` configuration files for systemd
      user service deployment leveraging `listenfd`.
- [ ] Create an `install.sh` script or `Makefile` to automatically build
      `shbot`, install the binaries to `~/.local/bin`, and configure `~/.zshrc`.

## 2. Advanced "File-Change" Trigger Patterns

- [ ] Update the `evalute_file_triggers` engine to support standard glob
      patterns (e.g. `**/*.rs` or `src/*.js`) instead of relying solely on exact
      absolute/relative paths.

## 3. Protocol Expansion

- [ ] Fully implement the `invalidate <query>` UNIX socket command to allow
      power-users to wipe out stale daemon caches for immediate sync debugging.
- [ ] Add a `shbot` subcommand to configure the daemon's log level dynamically
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

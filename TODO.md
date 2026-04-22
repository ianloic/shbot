# Project TODOs

This document tracks incomplete architectural features and future improvements
required to make `shbot` production-ready.

## 1. Dynamic File Watcher Registration

- [x] Refactor `SyncWatcher` to dynamically call
      `watcher.watch(path, RecursiveMode::Recursive)` when a client sends a
      `cd <path>` socket command.
- [x] Implement un-watching logic to gracefully drop paths when the final shell
      client navigates away from them to preserve system resources.

## 2. Packaging and Systemd Integration

- [ ] Create `shbot.service` and `shbot.socket` configuration files for systemd
      user service deployment leveraging `listenfd`.
- [ ] Create an `install.sh` script or `Makefile` to automatically build
      `shbot`, install the binaries to `~/.local/bin`, and configure `~/.zshrc`.

## 3. Configuration Hot-Reloading

- [x] Setup signal handlers (e.g. `SIGHUP`) or a custom `--reload` argument tied
      to the Unix socket that triggers the daemon to reread
      `~/.config/shbot/shbot.toml`.
- [x] Safely hotswap the `Arc<Config>` across all asynchronous loops on reload
      without dropping client connections.

## 4. Advanced "File-Change" Trigger Patterns

- [ ] Update the `evalute_file_triggers` engine to support standard glob
      patterns (e.g. `**/*.rs` or `src/*.js`) instead of relying solely on exact
      absolute/relative paths.

## 5. Protocol Expansion

- [ ] Fully implement the `invalidate <query>` UNIX socket command to allow
      power-users to wipe out stale daemon caches for immediate sync debugging.

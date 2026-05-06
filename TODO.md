# Project TODOs

This document tracks incomplete architectural features and future improvements
required to make `shbot` production-ready.

## Technical Debt & Refactoring

- [x] **Replace `sleep` in Integration Tests**: Refactor the integration test
      suite to remove `tokio::time::sleep` calls. Implement a deterministic,
      event-driven signaling mechanism using `tokio` channels to ensure test
      reliability and eliminate race conditions.
- [x] **Migrate Tooling to `mise`**: Replace manual script downloads with a
      `mise.toml` definition, implementing `fmt` and `coverage` as native `mise`
      tasks for better environment reproducibility.

## Test Coverage Gaps

While overall coverage is >90%, several specific areas remain uncovered and
should be addressed:

- [x] **`server.rs` Error Handling**:
  - [x] Test unexpected client disconnections mid-command and
        `stream.write_all()` errors.
  - [x] Test decoding failures for corrupted socket payloads.
  - [x] Test failure paths for dynamic config reloading (e.g., unparseable TOML
        or missing `LOG_RELOAD_HANDLE`).

- [x] **`main.rs` Startup & Signals**:
  - [x] Test the `SIGHUP` loop that attempts to hot-reload the configuration.
  - [x] Test the `Daemonize` forking logic in `handle_connect` (covered by
        `test_daemon_crash_recovery`).
  - [x] Test CLI errors when `shbot status`, `shbot reload`, or `shbot loglevel`
        encounter a socket that exists but is not being listened to.

- [x] **`watcher.rs` Edge Cases**:
  - [x] Test attempts to unwatch a path that was never watched.
  - [x] Test unexpected `notify` channel disconnections (`rx.recv()` errors).

- [x] **`git.rs` Edge Cases**:
  - [x] Test handling missing message encodings.
  - [x] Test edge cases with `pushRemote` versus standard remote resolution.
  - [x] Test specific `libgit2` error return branches that are hard to mock.

## Resource Leak Testing

Long-running daemons are susceptible to logical resource leaks. We should build
integration tests to ensure stability over time:

- [x] **File Descriptor (FD) Leaks**:
  - [x] Create a test that connects and disconnects 1,000+ mock clients rapidly.
  - [x] Assert that the number of open FDs in `/proc/<daemon_pid>/fd/` returns
        to baseline after connections are closed.

- [x] **`inotify` Watch Leaks**:
  - [x] Create a test that simulates clients `cd`ing into hundreds of temporary
        directories and then disconnecting abruptly.
  - [x] Assert that the daemon successfully unregisters the watches and the
        active watch count (`shbot status`) returns to 0 to prevent hitting OS
        `inotify` limits.

- [x] **Zombie Process Leaks**:
  - [x] Create a test that triggers a slow or hanging shell command (e.g.,
        `sleep 60`).
  - [x] Drop the client connection or force an interruption.
  - [x] Assert via `sysinfo` or `/proc/<pid>/task/` that no child processes
        remain in a zombie (`Z`) state.

- [x] **Memory Leaks**:
  - [x] Run the integration test suite periodically with LeakSanitizer
        (`RUSTFLAGS="-Z sanitizer=leak" cargo test`).
  - [x] Alternatively, build a stress test that measures Resident Set Size (RSS)
        before and after heavy load to ensure unbounded collections aren't
        growing infinitely.

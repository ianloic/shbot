# Project TODOs

This document tracks incomplete architectural features and future improvements
required to make `shbot` production-ready.

## Technical Debt & Refactoring

- ~~**Replace `sleep` in Integration Tests**~~: Refactor the integration test
  suite to remove `tokio::time::sleep` calls. Implement a deterministic,
  event-driven signaling mechanism using `tokio` channels to ensure test
  reliability and eliminate race conditions.

## Test Coverage Gaps

While overall coverage is >90%, several specific areas remain uncovered and
should be addressed:

- ~~**`server.rs` Error Handling**~~:
  - ~~Test unexpected client disconnections mid-command and `stream.write_all()`
    errors.~~
  - ~~Test decoding failures for corrupted socket payloads.~~
  - ~~Test failure paths for dynamic config reloading (e.g., unparseable TOML or
    missing `LOG_RELOAD_HANDLE`).~~

- ~~**`main.rs` Startup & Signals**~~:
  - Test the `SIGHUP` loop that attempts to hot-reload the configuration.
  - ~~Test the `Daemonize` forking logic in `handle_connect` (covered by
    `test_daemon_crash_recovery`).~~
  - ~~Test CLI errors when `shbot status`, `shbot reload`, or `shbot loglevel`
    encounter a socket that exists but is not being listened to.~~

- **`watcher.rs` Edge Cases**:
  - Test attempts to unwatch a path that was never watched.
  - Test unexpected `notify` channel disconnections (`rx.recv()` errors).

- **`git.rs` Edge Cases**:
  - Test handling missing message encodings.
  - Test edge cases with `pushRemote` versus standard remote resolution.
  - Test specific `libgit2` error return branches that are hard to mock.

## Resource Leak Testing

Long-running daemons are susceptible to logical resource leaks. We should build
integration tests to ensure stability over time:

- **File Descriptor (FD) Leaks**:
  - Create a test that connects and disconnects 1,000+ mock clients rapidly.
  - Assert that the number of open FDs in `/proc/<daemon_pid>/fd/` returns to
    baseline after connections are closed.

- **`inotify` Watch Leaks**:
  - Create a test that simulates clients `cd`ing into hundreds of temporary
    directories and then disconnecting abruptly.
  - Assert that the daemon successfully unregisters the watches and the active
    watch count (`shbot status`) returns to 0 to prevent hitting OS `inotify`
    limits.

- **Zombie Process Leaks**:
  - Create a test that triggers a slow or hanging shell command (e.g.,
    `sleep 60`).
  - Drop the client connection or force an interruption.
  - Assert via `sysinfo` or `/proc/<pid>/task/` that no child processes remain
    in a zombie (`Z`) state.

- **Memory Leaks**:
  - Run the integration test suite periodically with LeakSanitizer
    (`RUSTFLAGS="-Z sanitizer=leak" cargo test`).
  - Alternatively, build a stress test that measures Resident Set Size (RSS)
    before and after heavy load to ensure unbounded collections aren't growing
    infinitely.

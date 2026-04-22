# Instructions for Coding Agents

Welcome! If you are an AI coding assistant, agent, or LLM contributing to this
repository, please follow the guidelines specified below.

## 1. System Architecture

Before proposing modifications or investigating bugs, you should familiarize
yourself with the system's design and runtime behavior.

**Do not guess the system structure.** Please immediately read the
`ARCHITECTURE.md` file located in the root of this repository. It explains the
high-level boundaries between the Rust daemon, the Zsh shell plugin, and the
integration tests mechanism.

## 2. Keeping Documentation Up to Date

As you implement new features, refactor existing code, or alter the boundaries
of the system, **you are required to keep `ARCHITECTURE.md` up to date.**

If your changes introduce a new component, alter how the `shbot` daemon
communicates over the socket, or change the lifecycle hooks in the Zsh plugin,
please ensure those changes are accurately reflected in `ARCHITECTURE.md` as
part of your commit.

## 3. Development Guidelines

- **Rust Code**: Located in `daemon/`. Ensure all warnings are resolved and
  format using standard `cargo fmt`. Check changes with `cargo check` and run
  `cargo test`.
- **Zsh Plugin**: Located in `zsh/`. Test socket operations heavily; traditional
  shell test scripts are not enough.
- **Testing**: Integration tests are housed in `integration-tests/` and use a
  `rexpect` PTY harness to properly emulate ZLE features required by the shell
  plugin. Always run these tests locally (`cd integration-tests && cargo test`)
  when modifying socket interaction or shell commands.
- **Formatting**: ALWAYS ensure that all files (Rust, Zsh, and Markdown) are
  correctly formatted before landing any changes or proposing PRs. To quickly
  format the entire repository simply run `./scripts/fmt.sh` from the repository
  root. It will automatically download required formatters (`shfmt` and `dprint`) 
  if they are not globally installed.

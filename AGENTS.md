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

Similarly, **you are strictly required to keep `README.md` up to date.** As the
software evolves—whether through new user-facing features, configuration file
options, installation steps, or Zsh API capabilities—the `README.md` must always
reflect the current functionality.

Additionally, the project backlog is maintained in `TODO.md` in the root of the
repository. **As you complete items from the backlog or discover new
improvements that need to be made, you must keep `TODO.md` up to date.** Mark
completed tasks and append any new architectural or technical debt items you
find.

## 3. Development Guidelines

- **Rust Code**: Located in `daemon/`. Ensure all warnings are resolved and
  format using standard `cargo fmt`. Check changes with `cargo check` and run
  `cargo test`.
- **Zsh Plugin**: Located in `zsh/`. Test socket operations heavily; traditional
  shell test scripts are not enough.
- **Testing**: Integration tests are housed in `integration-tests/` and use a
  `rexpect` PTY harness to properly emulate ZLE features required by the shell
  plugin. Always run these tests locally (`cd integration-tests && cargo test`)
  when modifying socket interaction or shell commands. **Crucially, you must
  always write tests for all new features, bug fixes, and improvements.**
- **Formatting**: ALWAYS ensure that all files (Rust, Zsh, and Markdown) are
  correctly formatted before landing any changes or proposing PRs. To quickly
  format the entire repository simply run `./scripts/fmt.sh` from the repository
  root. It will automatically download required formatters (`shfmt` and
  `dprint`) if they are not globally installed.
- **Code Style**: When importing types or functions from within the crate,
  always pull them into scope via `use` statements. Do not use inline `crate::`
  paths (e.g., `crate::error::Result`) anywhere in the code except within `use`
  declarations.

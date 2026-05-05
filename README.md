# shbot

`shbot` is a daemon and Zsh plugin that delivers asynchronous background tasks,
filesystem watches, and high-performance Git status updates directly to your
shell without blocking your interactive prompt.

By moving expensive operations out of the critical rendering path of your shell,
`shbot` ensures your prompt draws instantly while still providing rich,
up-to-date state.

## Getting Started

### 1. Installation

The easiest way to install `shbot` and its systemd user services is to use the
included installation script:

```bash
git clone https://github.com/yourusername/shbot.git
cd shbot
./install.sh
```

This script will seamlessly compile the daemon, copy the binary to
`~/.local/bin/`, set up `systemd` socket activation, and configure your
`~/.zshrc`.

### 2. Shell Setup

### 2. Shell Setup

If you used `./install.sh`, this is already done for you! Otherwise, add the
bundled Zsh plugin to your `~/.zshrc`:

```zsh
# Source the shbot plugin
source /path/to/shbot/zsh/shbot.plugin.zsh
```

Upon launching a new shell, `shbot` will automatically start the background
daemon (if it isn't socket-activated via systemd already) and instantly connect
your shell.

### 3. Minimal Configuration (Optional)

`shbot` natively comes with zero-configuration Git status reporting. If you want
to configure background triggers or file-watchers, create the configuration file
at `~/.config/shbot/shbot.toml`:

```toml
[daemon]
# Set the daemon to automatically shut down after 5 minutes of no shell activity.
idle_timeout_sec = 300

# A task that runs in the background every 60 seconds
[triggers.my_background_task]
type = "periodic"
interval_sec = 60
command = "echo 'Hello World'"

# A task that runs whenever files in `src` change (supports glob patterns)
[triggers.my_watcher]
type = "file-change"
watch = "src/**/*.rs"
command = "cargo check"
```

---

## Configuration File

The `~/.config/shbot/shbot.toml` configuration file supports the following
blocks:

### `[daemon]`

- `idle_timeout_sec` (Default: `300`): The number of seconds the background
  daemon should continue running after all active shells have disconnected.

### `[triggers.<name>]`

Define custom automated behaviors. The `<name>` you provide here will be the
identifier used to access the result in Zsh (e.g., `my_task` populates
`$SHBOT_STATE[my_task]`).

Types of triggers:

- **Periodic**:
  - `type`: `"periodic"`
  - `interval_sec`: Frequency in seconds.
  - `command`: Shell command to execute. Output will be piped to Zsh.
- **File Change**:
  - `type`: `"file-change"`
  - `watch`: A glob pattern of the paths to monitor for changes (relative to the
    shell's Current Working Directory), e.g., `"src/**/*.rs"`.
  - `command`: Shell command to execute on change.
- **Explicit**:
  - `type`: `"explicit"`
  - `command`: Shell command to execute manually.

> **Hot Reloading:** `shbot` dynamically hot-reloads its configuration when the
> TOML file changes, instantly terminating or applying new trigger intervals
> without needing to restart active shells. You can also manually trigger a
> reload by running `shbot reload`.

---

## CLI Commands

While `shbot` runs primarily in the background, it provides a CLI for
interaction:

- `shbot connect`: Bootstraps the daemon (spawns it in the background if not
  running).
- `shbot daemon`: Runs the long-lived daemon server directly in the foreground.
- `shbot status`: Requests the current internal state from the running daemon,
  including detailed client information (like CWDs) and state collection metrics
  (e.g. file monitor count and git refresh times).
- `shbot reload`: Forces the daemon to reload its configuration file
  dynamically.
- `shbot loglevel <level>`: Dynamically sets the daemon's runtime log filter
  (e.g., `shbot loglevel debug`) without restarting.

---

## Zsh API

`shbot` populates an associative array in Zsh named `$SHBOT_STATE`. Each key
corresponds to a background task or native module, and triggers a prompt refresh
(`zle reset-prompt`) automatically upon update.

### Accessing Custom Triggers

If you defined `[triggers.my_background_task]` in your configuration whose
command output was `"Success"`, you can access it in Zsh like so:

```zsh
echo $SHBOT_STATE[my_background_task]
# Outputs: Success
```

### Accessing Git Status (`$SHBOT_STATE[git]`)

`shbot` features an extraordinarily deep, native, high-performance integration
with `git2`. Whenever you enter a Git repository or whenever a file changes, the
daemon computes a rich 27-field dataset mapping seamlessly to the standard
expected by `gitstatusd`.

To safely avoid whitespace collision in parsed values like commit messages, the
serialized Git string uses the ASCII Unit Separator (`\x1f`).

You can extract the string into a Zsh array in your prompt configuration scripts
via parameter expansion:

```zsh
local -a git_state=("${(@s.$'\x1f'.)SHBOT_STATE[git]}")

# Example usages:
local branch_name=$git_state[3]
local commits_ahead=$git_state[13]
local num_staged=$git_state[9]
local action=$git_state[7] # e.g., 'rebase', 'merge'
local commit_summary=$git_state[27]
```

### Drop-in `gitstatusd` Replacement

If you are currently using `romkatv/gitstatus` (for example, with
`powerlevel10k`) and want an easy migration, you can use `shbot` as a drop-in
replacement by enabling native VCS extraction and mocking the standard
`gitstatus` functions.

Add the following to your `~/.zshrc` _before_ your theme initializes:

```zsh
# Enable shbot to automatically populate standard VCS_STATUS_* variables
typeset -g SHBOT_SET_VCS_STATUS=1

# Mock the gitstatus API for themes like powerlevel10k
function gitstatus_start() { return 0 }
function gitstatus_stop()  { return 0 }
function gitstatus_check() { return 0 }
function gitstatus_query() {
    emulate -L zsh -o no_aliases -o extended_glob -o typeset_silent
    if [[ -n "$VCS_STATUS_WORKDIR" ]]; then
        typeset -g VCS_STATUS_RESULT=ok-sync
    else
        typeset -g VCS_STATUS_RESULT=norepo-sync
    fi
    return 0
}
```

The array indices map exactly to the standard `gitstatus` plugin variables:

1. `workdir`
2. `commit` (hash)
3. `local_branch`
4. `remote_branch`
5. `remote_name`
6. `remote_url`
7. `action` (e.g., rebase, merge)
8. `index_size`
9. `num_staged`
10. `num_unstaged`
11. `num_conflicted`
12. `num_untracked`
13. `commits_ahead`
14. `commits_behind`
15. `stashes`
16. `tag`
17. `num_unstaged_deleted`
18. `num_staged_new`
19. `num_staged_deleted`
20. `push_remote_name`
21. `push_remote_url`
22. `push_commits_ahead`
23. `push_commits_behind`
24. `num_skip_worktree`
25. `num_assume_unchanged`
26. `commit_encoding`
27. `commit_summary`

---

## License

`shbot` is licensed under the GPL-3.0-or-later license. See the
[COPYING](COPYING) file for more details.

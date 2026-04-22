# **shbot Implementation Specification**

## **1\. System Architecture**

shbot is a highly concurrent, low-latency background daemon designed to
calculate and distribute shell prompt state to multiple Zsh clients. It moves
slow, blocking I/O tasks out of the shell's critical path.

The project consists of:

1. **A Rust Binary (shbot)**: Handles daemon operations, git parsing, file
   watching, and status reporting.
2. **A Zsh Plugin (shbot.plugin.zsh)**: Connects to the daemon, registers hooks,
   and asynchronously updates shell state.

### **1.1 Execution Modes**

- **shbot daemon**: The long-running process. It binds to a UNIX socket, listens
  for clients, monitors the filesystem, calculates state, and broadcasts
  updates.
- **shbot connect**: A transient command executed by Zsh to safely bootstrap the
  daemon if it isn't running, using file-locking to prevent race conditions.
- **shbot status**: Connects to the daemon and requests an internal state dump
  (connected clients, tracked directories, update latencies) for observability.

## **2\. Daemon Lifecycle & Socket Activation**

To minimize resource usage, shbot employs an "exit on idle" strategy and
supports systemd.

- **Systemd Socket Activation**: Upon startup, the daemon must check for systemd
  socket activation (e.g., using the listenfd crate or checking LISTEN\_FDS). If
  a socket is passed by systemd, it uses that.
- **Fallback Socket**: If not socket-activated, it manually binds to
  \~/.local/state/shbot/socket/${HOSTNAME}.socket.
- **Die on Idle**: The daemon tracks connected clients. If the client count
  drops to 0, it starts a timer (configurable, default: 5 minutes). If a new
  client connects before the timer expires, the timer is cancelled. If the timer
  expires, the daemon gracefully exits (std::process::exit(0)).

## **3\. Communication Protocol**

The protocol is asynchronous, line-based, and newline-delimited (\\n) over the
UNIX domain socket.

### **3.1 Client \-\> Server Messages**

- hello \<PID\>: Registers the client. PID must be an integer.
- cd \<PATH\>: Updates the client's current working directory. The daemon uses
  this to manage reference counts for Git repositories.
- invalidate \<STATE\>: Forces an immediate refresh of a state (e.g., invalidate
  custom\_git).
- status: Special command. The daemon responds with a formatted status dump and
  then closes the connection.

### **3.2 Server \-\> Client Messages**

- update \<STATE\_ID\> \<VALUE\>: Sent when a state changes. VALUE is the
  remainder of the line. VALUE must not contain raw newlines. For complex data
  (like Git), use a delimited string (e.g., main|2|0|1|4|0|0).

## **4\. Git Integration & Performance**

Parsing git repositories (especially large ones) is the most resource-intensive
task.

- **Library**: Use the git2 crate (libgit2 bindings).
- **Tracking**: When a client sends cd \<PATH\>, shbot walks up the directory
  tree to find the .git root. It maps GitRepo(RootPath) \-\> Set\<Client PIDs\>.
- **File Watching & Debouncing**:
  - Attach a filesystem watcher (using the notify crate) to the .git/ directory
    (specifically .git/HEAD, .git/index, .git/refs/) and the working tree.
  - **Crucial:** File events must be coalesced/debounced. If 10 files are saved
    simultaneously, the daemon must not calculate git status 10 times. Wait for
    a quiet period (e.g., 50-100ms) after the last filesystem event before
    executing the git status calculation in a background thread.
- **Output Format**: Broadcast as a pipe-delimited string: update git
  \<branch\>|\<ahead\>|\<behind\>|\<staged\>|\<unstaged\>|\<untracked\>|\<conflicted\>

## **5\. Configuration & Custom Triggers**

Custom state is defined in \~/.config/shbot/shbot.toml.

\[daemon\]\
idle\_timeout\_sec \= 300

\[triggers.weather\]\
type \= "periodic"\
interval\_sec \= 1800 \# 30 minutes\
command \= "curl \-s 'wttr.in/?format=1'"

\[triggers.npm\_packages\]\
type \= "file-change"\
watch \= "package.json" \# Relative to client CWDs\
command \= "node check\_deps.js"

\[triggers.aws\_profile\]\
type \= "explicit" \# Only runs when client sends \`invalidate aws\_profile\`\
command \= "cat \~/.aws/current\_profile"

## **6\. Observability & Logging**

- **Logging Framework**: Use the tracing and tracing-subscriber crates.
- **Output**: Log to stdout by default. If running under systemd, systemd will
  automatically capture this into the user journal. Provide a \--log-file
  \<PATH\> flag to redirect output to a file for non-systemd usage.
- **shbot status**: When this CLI command runs, it connects to the socket, sends
  status, and receives a payload containing:
  - Total active clients (PIDs).
  - List of actively tracked directories/Git repos.
  - Known states, their current string values, timestamp of last update, and
    calculation latency (e.g., git: 45ms, weather: 1.2s).

## **7\. Zsh Client Implementation (shbot.plugin.zsh)**

The agent should bundle this Zsh script to handle the frontend integration.

zmodload zsh/net/socket\
zmodload zsh/system

typeset \-g SHBOT\_FD=0\
typeset \-gA SHBOT\_STATE

function \_shbot\_connect() {\
local
socket\_path="${SHBOT\_SOCKET:-$HOME/.local/state/shbot/socket/$HOST.socket}"

    if \[\[ $SHBOT\_FD \-ne 0 \]\]; then  
        zle \-F $SHBOT\_FD 2\>/dev/null  
        exec {SHBOT\_FD}\<&-  
        SHBOT\_FD=0  
    fi

    if \[\[ \! \-S "$socket\_path" \]\]; then  
        command shbot connect || return 1  
    fi

    zsocket \-d SHBOT\_FD "$socket\_path" 2\>/dev/null || return 1

    syswrite \-O $SHBOT\_FD "hello $$\\n"  
    syswrite \-O $SHBOT\_FD "cd $PWD\\n"

    zle \-F $SHBOT\_FD \_shbot\_read\_handler

}

function \_shbot\_read\_handler() {\
local fd=$1\
local line

    if \! read \-r \-u $fd line; then  
        zle \-F $fd  
        exec {fd}\<&-  
        SHBOT\_FD=0  
        return 1  
    fi

    local msg\_type="${line%% \*}"  
    local rest="${line\#\* }"  
      
    if \[\[ "$msg\_type" \== "update" \]\]; then  
        local state\_id="${rest%% \*}"  
        local payload="${rest\#\* }"  
        SHBOT\_STATE\[$state\_id\]="$payload"  
        zle reset-prompt  
    fi

}

function \_shbot\_chpwd\_hook() {\
if \[\[ $SHBOT\_FD \-ne 0 \]\]; then\
syswrite \-O $SHBOT\_FD "cd $PWD\\n"\
fi\
}\
add-zsh-hook chpwd \_shbot\_chpwd\_hook

function \_shbot\_precmd\_hook() {\
if \[\[ $SHBOT\_FD \-eq 0 \]\]; then\
\_shbot\_connect\
fi\
}\
add-zsh-hook precmd \_shbot\_precmd\_hook

function \_shbot\_zshexit\_hook() {\
if \[\[ $SHBOT\_FD \-ne 0 \]\]; then\
zle \-F $SHBOT\_FD 2\>/dev/null\
exec {SHBOT\_FD}\<&-\
fi\
}\
add-zsh-hook zshexit \_shbot\_zshexit\_hook

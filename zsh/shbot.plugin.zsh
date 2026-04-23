zmodload zsh/net/socket
zmodload zsh/system
autoload -Uz add-zsh-hook

typeset -g SHBOT_FD=0
typeset -gA SHBOT_STATE

function _shbot_debug() {
	if [[ -n "$SHBOT_DEBUG" ]]; then
		print -u2 "[shbot-debug] $*"
	fi
}

function _shbot_send() {
	if [[ $SHBOT_FD -eq 0 ]]; then
		_shbot_debug "Cannot send '$1': no active connection"
		return 1
	fi
	_shbot_debug "Sending command: $1"
	syswrite -o $SHBOT_FD "$1"$'\n' || _shbot_debug "Write failed: $1"
}

function _shbot_connect() {
	local socket_path="${SHBOT_SOCKET:-$HOME/.local/state/shbot/$HOST.socket}"
	_shbot_debug "Connecting to socket: $socket_path"

	if [[ $SHBOT_FD -ne 0 ]]; then
		_shbot_debug "Closing existing connection (FD: $SHBOT_FD)"
		zle -F $SHBOT_FD 2>/dev/null
		exec {SHBOT_FD}<&-
		SHBOT_FD=0
	fi

	# Optimistically try connecting
	if ! zsocket "$socket_path" 2>/dev/null; then
		_shbot_debug "Socket not responding, starting daemon..."
		local shbot_cmd="${SHBOT_BIN:-shbot}"
		command $shbot_cmd connect || {
			_shbot_debug "Daemon start failed"
			return 1
		}
		zsocket "$socket_path" 2>/dev/null || {
			_shbot_debug "Failed to connect to $socket_path after start"
			return 1
		}
	fi

	SHBOT_FD=$REPLY
	_shbot_debug "Connected to socket (FD: $SHBOT_FD)"

	_shbot_send "hello $$"
	_shbot_send "cd $PWD"

	_shbot_debug "Setting up read handler"
	zle -F $SHBOT_FD _shbot_read_handler
}

function _shbot_read_handler() {
	local fd=$1
	local line

	if ! read -r -u $fd line; then
		_shbot_debug "Failed to read from FD $fd (disconnecting)"
		zle -F $fd
		exec {fd}<&-
		SHBOT_FD=0
		return 1
	fi

	_shbot_debug "Read line: $line"

	local msg_type="${line%% *}"
	local rest="${line#* }"

	if [[ "$msg_type" == "update" ]]; then
		local state_id="${rest%% *}"
		local payload="${rest#* }"
		SHBOT_STATE[$state_id]="$payload"
		_shbot_debug "Updated state: $state_id = $payload"
		zle reset-prompt
	fi
}

function _shbot_chpwd_hook() {
	_shbot_debug "chpwd triggered ($PWD)"
	_shbot_send "cd $PWD"
}
add-zsh-hook chpwd _shbot_chpwd_hook

function _shbot_precmd_hook() {
	if [[ $SHBOT_FD -eq 0 ]]; then
		_shbot_connect
	fi
}
add-zsh-hook precmd _shbot_precmd_hook

function _shbot_zshexit_hook() {
	_shbot_debug "zshexit triggered"
	if [[ $SHBOT_FD -ne 0 ]]; then
		_shbot_debug "Closing connection"
		zle -F $SHBOT_FD 2>/dev/null
		exec {SHBOT_FD}<&-
	fi
}
add-zsh-hook zshexit _shbot_zshexit_hook

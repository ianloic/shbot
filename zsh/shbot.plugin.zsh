zmodload zsh/net/socket
zmodload zsh/system
autoload -Uz add-zsh-hook

typeset -g SHBOT_FD=0
typeset -gA SHBOT_STATE

function _shbot_connect() {
	local socket_path="${SHBOT_SOCKET:-$HOME/.local/state/shbot/socket/$HOST.socket}"

	if [[ $SHBOT_FD -ne 0 ]]; then
		zle -F $SHBOT_FD 2>/dev/null
		exec {SHBOT_FD}<&-
		SHBOT_FD=0
	fi

	if [[ ! -S "$socket_path" ]]; then
		command shbot connect || return 1
		# Give the daemon a moment to boot
		sleep 0.1
	fi

	zsocket "$socket_path" 2>/dev/null || return 1
	SHBOT_FD=$REPLY

	syswrite -o $SHBOT_FD "hello $$\n"
	syswrite -o $SHBOT_FD "cd $PWD\n"

	zle -F $SHBOT_FD _shbot_read_handler
}

function _shbot_read_handler() {
	local fd=$1
	local line

	if ! read -r -u $fd line; then
		zle -F $fd
		exec {fd}<&-
		SHBOT_FD=0
		return 1
	fi

	local msg_type="${line%% *}"
	local rest="${line#* }"

	if [[ "$msg_type" == "update" ]]; then
		local state_id="${rest%% *}"
		local payload="${rest#* }"
		SHBOT_STATE[$state_id]="$payload"
		zle reset-prompt
	fi
}

function _shbot_chpwd_hook() {
	if [[ $SHBOT_FD -ne 0 ]]; then
		syswrite -o $SHBOT_FD "cd $PWD\n"
	fi
}
add-zsh-hook chpwd _shbot_chpwd_hook

function _shbot_precmd_hook() {
	if [[ $SHBOT_FD -eq 0 ]]; then
		_shbot_connect
	fi
}
add-zsh-hook precmd _shbot_precmd_hook

function _shbot_zshexit_hook() {
	if [[ $SHBOT_FD -ne 0 ]]; then
		zle -F $SHBOT_FD 2>/dev/null
		exec {SHBOT_FD}<&-
	fi
}
add-zsh-hook zshexit _shbot_zshexit_hook

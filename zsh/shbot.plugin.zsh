# Copyright (C) 2026 Ian McKellar
#
# This program is free software: you can redistribute it and/or modify
# it under the terms of the GNU General Public License as published by
# the Free Software Foundation, either version 3 of the License, or
# (at your option) any later version.
#
# This program is distributed in the hope that it will be useful,
# but WITHOUT ANY WARRANTY; without even the implied warranty of
# MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
# GNU General Public License for more details.
#
# You should have received a copy of the GNU General Public License
# along with this program.  If not, see <https://www.gnu.org/licenses/>.

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

function _shbot_update_vcs_status() {
	_shbot_debug "Updating VCS status: $1"
	local payload="$1"
	if [[ -z "$payload" ]]; then
		unset VCS_STATUS_{WORKDIR,COMMIT,LOCAL_BRANCH,REMOTE_BRANCH,REMOTE_NAME,REMOTE_URL,ACTION,INDEX_SIZE,NUM_STAGED,NUM_UNSTAGED,NUM_CONFLICTED,NUM_UNTRACKED,HAS_STAGED,HAS_UNSTAGED,HAS_CONFLICTED,HAS_UNTRACKED,COMMITS_AHEAD,COMMITS_BEHIND,STASHES,TAG,NUM_UNSTAGED_DELETED,NUM_STAGED_NEW,NUM_STAGED_DELETED,PUSH_REMOTE_NAME,PUSH_REMOTE_URL,PUSH_COMMITS_AHEAD,PUSH_COMMITS_BEHIND,NUM_SKIP_WORKTREE,NUM_ASSUME_UNCHANGED,COMMIT_ENCODING,COMMIT_SUMMARY}
		return 0
	fi

	local sep=$'\x1f'
	local -a resp
	eval "resp=(\"\${(@s.$sep.)payload}\")"

	if [[ -z "${resp[1]}" ]]; then
		unset VCS_STATUS_{WORKDIR,COMMIT,LOCAL_BRANCH,REMOTE_BRANCH,REMOTE_NAME,REMOTE_URL,ACTION,INDEX_SIZE,NUM_STAGED,NUM_UNSTAGED,NUM_CONFLICTED,NUM_UNTRACKED,HAS_STAGED,HAS_UNSTAGED,HAS_CONFLICTED,HAS_UNTRACKED,COMMITS_AHEAD,COMMITS_BEHIND,STASHES,TAG,NUM_UNSTAGED_DELETED,NUM_STAGED_NEW,NUM_STAGED_DELETED,PUSH_REMOTE_NAME,PUSH_REMOTE_URL,PUSH_COMMITS_AHEAD,PUSH_COMMITS_BEHIND,NUM_SKIP_WORKTREE,NUM_ASSUME_UNCHANGED,COMMIT_ENCODING,COMMIT_SUMMARY}
		return 0
	fi

	typeset -g VCS_STATUS_WORKDIR="${resp[1]}"
	typeset -g VCS_STATUS_COMMIT="${resp[2]}"
	typeset -g VCS_STATUS_LOCAL_BRANCH="${resp[3]}"
	typeset -g VCS_STATUS_REMOTE_BRANCH="${resp[4]}"
	typeset -g VCS_STATUS_REMOTE_NAME="${resp[5]}"
	typeset -g VCS_STATUS_REMOTE_URL="${resp[6]}"
	typeset -g VCS_STATUS_ACTION="${resp[7]}"
	typeset -g VCS_STATUS_INDEX_SIZE="${resp[8]}"
	typeset -g VCS_STATUS_NUM_STAGED="${resp[9]}"
	typeset -g VCS_STATUS_NUM_UNSTAGED="${resp[10]}"
	typeset -g VCS_STATUS_NUM_CONFLICTED="${resp[11]}"
	typeset -g VCS_STATUS_NUM_UNTRACKED="${resp[12]}"
	typeset -g VCS_STATUS_COMMITS_AHEAD="${resp[13]}"
	typeset -g VCS_STATUS_COMMITS_BEHIND="${resp[14]}"
	typeset -g VCS_STATUS_STASHES="${resp[15]}"
	typeset -g VCS_STATUS_TAG="${resp[16]}"
	typeset -g VCS_STATUS_NUM_UNSTAGED_DELETED="${resp[17]}"
	typeset -g VCS_STATUS_NUM_STAGED_NEW="${resp[18]}"
	typeset -g VCS_STATUS_NUM_STAGED_DELETED="${resp[19]}"
	typeset -g VCS_STATUS_PUSH_REMOTE_NAME="${resp[20]}"
	typeset -g VCS_STATUS_PUSH_REMOTE_URL="${resp[21]}"
	typeset -g VCS_STATUS_PUSH_COMMITS_AHEAD="${resp[22]}"
	typeset -g VCS_STATUS_PUSH_COMMITS_BEHIND="${resp[23]}"
	typeset -g VCS_STATUS_NUM_SKIP_WORKTREE="${resp[24]}"
	typeset -g VCS_STATUS_NUM_ASSUME_UNCHANGED="${resp[25]}"
	typeset -g VCS_STATUS_COMMIT_ENCODING="${resp[26]}"
	typeset -g VCS_STATUS_COMMIT_SUMMARY="${resp[27]}"

	typeset -gi VCS_STATUS_{INDEX_SIZE,NUM_STAGED,NUM_UNSTAGED,NUM_CONFLICTED,NUM_UNTRACKED,COMMITS_AHEAD,COMMITS_BEHIND,STASHES,NUM_UNSTAGED_DELETED,NUM_STAGED_NEW,NUM_STAGED_DELETED,PUSH_COMMITS_AHEAD,PUSH_COMMITS_BEHIND,NUM_SKIP_WORKTREE,NUM_ASSUME_UNCHANGED}
	typeset -gi VCS_STATUS_HAS_STAGED=$((VCS_STATUS_NUM_STAGED > 0))
	typeset -gi VCS_STATUS_HAS_UNSTAGED=$((VCS_STATUS_NUM_UNSTAGED > 0))
	typeset -gi VCS_STATUS_HAS_CONFLICTED=$((VCS_STATUS_NUM_CONFLICTED > 0))
	typeset -gi VCS_STATUS_HAS_UNTRACKED=$((VCS_STATUS_NUM_UNTRACKED > 0))

	if [[ -n $VCS_STATUS_WORKDIR ]]; then
		typeset -g VCS_STATUS_RESULT=ok-sync
	else
		typeset -g VCS_STATUS_RESULT=norepo-sync
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

		if [[ "$state_id" == "git" && -n "$SHBOT_SET_VCS_STATUS" ]]; then
			_shbot_update_vcs_status "$payload"
		fi

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

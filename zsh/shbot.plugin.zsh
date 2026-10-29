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
typeset -g SHBOT_VERSION="0.1.0"
typeset -g SHBOT_REPO="${SHBOT_REPO:-ianloic/shbot}"

function _shbot_debug() {
	if [[ -n "$SHBOT_DEBUG" ]]; then
		print -u2 "[shbot-debug] $*"
	fi
}

function _shbot_platform() {
	local uname_s="${SHBOT_UNAME_S:-$(command uname -s 2>/dev/null)}"
	local uname_m="${SHBOT_UNAME_M:-$(command uname -m 2>/dev/null)}"
	uname_s="${uname_s:l}"
	uname_m="${uname_m:l}"
	if [[ "$uname_s" == "linux" && "$uname_m" == "arm64" ]]; then
		uname_m="aarch64"
	elif [[ "$uname_m" == "amd64" ]]; then
		uname_m="x86_64"
	fi
	REPLY="${uname_s}-${uname_m}"
}

function _shbot_download_bin() {
	local target_plat="$1"
	local dest_bin="$2"
	local cache_dir="${dest_bin:h}"
	local archive="shbot-v${SHBOT_VERSION}-${target_plat}.tar.gz"
	local base_url="${SHBOT_RELEASE_BASE_URL:-https://github.com/${SHBOT_REPO}/releases/download/v${SHBOT_VERSION}}"

	command mkdir -p -m 0700 "$cache_dir" 2>/dev/null || return 1
	local tmp_dir
	tmp_dir="$(command mktemp -d "${cache_dir}/.tmp.XXXXXX" 2>/dev/null)" || return 1

	_shbot_debug "Downloading ${base_url}/${archive}"
	if command -v curl >/dev/null 2>&1; then
		if ! command curl -fsSL "${base_url}/${archive}" -o "${tmp_dir}/${archive}" 2>/dev/null ||
			! command curl -fsSL "${base_url}/sha256sums.txt" -o "${tmp_dir}/sha256sums.txt" 2>/dev/null; then
			_shbot_debug "Failed to download release assets from ${base_url}"
			command rm -rf "$tmp_dir"
			return 1
		fi
	elif command -v wget >/dev/null 2>&1; then
		if ! command wget -qO "${tmp_dir}/${archive}" "${base_url}/${archive}" 2>/dev/null ||
			! command wget -qO "${tmp_dir}/sha256sums.txt" "${base_url}/sha256sums.txt" 2>/dev/null; then
			_shbot_debug "Failed to download release assets from ${base_url}"
			command rm -rf "$tmp_dir"
			return 1
		fi
	else
		_shbot_debug "Neither curl nor wget found"
		command rm -rf "$tmp_dir"
		return 1
	fi

	local expected_hash=""
	local sum_line
	while IFS= read -r sum_line; do
		local h="${sum_line%% *}"
		local f="${sum_line##* }"
		f="${f#\*}"
		if [[ "$f" == "$archive" && -n "$h" ]]; then
			expected_hash="$h"
			break
		fi
	done <"${tmp_dir}/sha256sums.txt"

	local actual_hash=""
	if command -v sha256sum >/dev/null 2>&1; then
		actual_hash="$(command sha256sum "${tmp_dir}/${archive}" 2>/dev/null)"
		actual_hash="${actual_hash%% *}"
	elif command -v shasum >/dev/null 2>&1; then
		actual_hash="$(command shasum -a 256 "${tmp_dir}/${archive}" 2>/dev/null)"
		actual_hash="${actual_hash%% *}"
	else
		_shbot_debug "Neither sha256sum nor shasum found"
		command rm -rf "$tmp_dir"
		return 1
	fi

	if [[ -z "$expected_hash" || "${actual_hash:l}" != "${expected_hash:l}" ]]; then
		_shbot_debug "SHA-256 mismatch for ${archive}: expected ${expected_hash}, got ${actual_hash}"
		command rm -rf "$tmp_dir"
		return 1
	fi

	if ! command tar -xzf "${tmp_dir}/${archive}" -C "$tmp_dir" shbot 2>/dev/null || [[ ! -f "${tmp_dir}/shbot" ]]; then
		_shbot_debug "Failed to extract shbot from ${archive}"
		command rm -rf "$tmp_dir"
		return 1
	fi

	command chmod 0700 "${tmp_dir}/shbot"
	if ! command mv -f "${tmp_dir}/shbot" "$dest_bin" 2>/dev/null; then
		command rm -rf "$tmp_dir"
		return 1
	fi
	command rm -rf "$tmp_dir"
	return 0
}

function _shbot_resolve_bin() {
	if [[ -n "$SHBOT_BIN" ]]; then
		if [[ -x "$SHBOT_BIN" ]]; then
			REPLY="$SHBOT_BIN"
			return 0
		elif command -v "$SHBOT_BIN" >/dev/null 2>&1; then
			REPLY="$(command -v "$SHBOT_BIN")"
			return 0
		fi
		_shbot_debug "Configured SHBOT_BIN is not executable: $SHBOT_BIN"
		return 1
	fi

	local dev_bin="${XDG_STATE_HOME:-$HOME/.local/state}/shbot/dev-bin"
	if [[ -x "$dev_bin" ]]; then
		_shbot_debug "Using local dev binary: $dev_bin"
		REPLY="$dev_bin"
		return 0
	fi

	_shbot_platform
	local target_plat="$REPLY"
	local cache_root="${SHBOT_CACHE_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/shbot}"
	local cached_bin="${cache_root}/v${SHBOT_VERSION}/shbot-${target_plat}"

	if [[ -x "$cached_bin" ]]; then
		REPLY="$cached_bin"
		return 0
	fi

	if _shbot_download_bin "$target_plat" "$cached_bin"; then
		REPLY="$cached_bin"
		return 0
	fi

	return 1
}

function shbot() {
	if ! _shbot_resolve_bin; then
		print -u2 "shbot: failed to locate or download shbot binary"
		return 1
	fi
	command "$REPLY" "$@"
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
	local rest="$payload"
	while true; do
		resp+=("${rest%%$sep*}")
		if [[ "$rest" == *$sep* ]]; then
			rest="${rest#*$sep}"
		else
			break
		fi
	done

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
	if [[ "$1" == *$'\n'* || "$1" == *$'\r'* ]]; then
		_shbot_debug "Refusing to send command containing a newline"
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
		if ! _shbot_resolve_bin; then
			_shbot_debug "Failed to resolve shbot binary"
			return 1
		fi
		local shbot_cmd="$REPLY"
		command "$shbot_cmd" connect || {
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
		local marker=$'\x01' nl=$'\n' cr=$'\r' us=$'\x1f'
		payload="${payload//'%25'/$marker}"
		payload="${payload//'%0A'/$nl}"
		payload="${payload//'%0D'/$cr}"
		payload="${payload//'%1F'/$us}"
		payload="${payload//'%00'/}"
		payload="${payload//"$marker"/%}"
		payload="${payload//'%01'/$marker}"
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

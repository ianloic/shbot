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
zmodload zsh/datetime
zmodload zsh/parameter 2>/dev/null
autoload -Uz add-zsh-hook

typeset -g SHBOT_FD=0
typeset -gA SHBOT_STATE
typeset -g SHBOT_VERSION="0.2.1"
typeset -g SHBOT_REPO="${SHBOT_REPO:-ianloic/shbot}"
typeset -gi SHBOT_LAST_DOWNLOAD_FAILURE=0
typeset -ga SHBOT_SUPPORTED_PLATFORMS
SHBOT_SUPPORTED_PLATFORMS=("linux-x86_64" "linux-aarch64")
typeset -g _SHBOT_CACHED_UNAME_S=""
typeset -g _SHBOT_CACHED_UNAME_M=""

function _shbot_debug() {
	if [[ -n "$SHBOT_DEBUG" ]]; then
		print -u2 "[shbot-debug] $*"
	fi
}

function _shbot_platform() {
	local uname_s="$SHBOT_UNAME_S"
	if [[ -z "$uname_s" ]]; then
		if [[ -z "$_SHBOT_CACHED_UNAME_S" ]]; then
			_SHBOT_CACHED_UNAME_S="$(command uname -s 2>/dev/null)"
		fi
		uname_s="$_SHBOT_CACHED_UNAME_S"
	fi
	local uname_m="$SHBOT_UNAME_M"
	if [[ -z "$uname_m" ]]; then
		if [[ -z "$_SHBOT_CACHED_UNAME_M" ]]; then
			_SHBOT_CACHED_UNAME_M="$(command uname -m 2>/dev/null)"
		fi
		uname_m="$_SHBOT_CACHED_UNAME_M"
	fi
	uname_s="${uname_s:l}"
	uname_m="${uname_m:l}"
	if [[ "$uname_s" == "linux" && "$uname_m" == "arm64" ]]; then
		uname_m="aarch64"
	elif [[ "$uname_m" == "amd64" ]]; then
		uname_m="x86_64"
	fi
	REPLY="${uname_s}-${uname_m}"
}

function _shbot_platform_supported() {
	local target_plat="$1"
	local plat
	for plat in "${SHBOT_SUPPORTED_PLATFORMS[@]}"; do
		if [[ "$plat" == "$target_plat" ]]; then
			return 0
		fi
	done
	return 1
}

function _shbot_semver_compatible() {
	local req="$1"
	local cand="$2"
	local req_maj="${req%%.*}"
	local req_rest="${req#*.}"
	local req_min="${req_rest%%.*}"
	local req_pat="${req_rest#*.}"
	if [[ "$req_rest" == "$req" || "$req_pat" == "$req_rest" || "$req_pat" == *.* ]]; then
		return 1
	fi
	if [[ -z "$req_maj" || -z "$req_min" || -z "$req_pat" ]]; then
		return 1
	fi
	if [[ "$req_maj" == *[!0-9]* || "$req_min" == *[!0-9]* || "$req_pat" == *[!0-9]* ]]; then
		return 1
	fi

	local cand_maj="${cand%%.*}"
	local cand_rest="${cand#*.}"
	local cand_min="${cand_rest%%.*}"
	local cand_pat="${cand_rest#*.}"
	if [[ "$cand_rest" == "$cand" || "$cand_pat" == "$cand_rest" || "$cand_pat" == *.* ]]; then
		return 1
	fi
	if [[ -z "$cand_maj" || -z "$cand_min" || -z "$cand_pat" ]]; then
		return 1
	fi
	if [[ "$cand_maj" == *[!0-9]* || "$cand_min" == *[!0-9]* || "$cand_pat" == *[!0-9]* ]]; then
		return 1
	fi

	if ((req_maj != cand_maj)); then
		return 1
	fi
	if ((req_maj == 0)); then
		((req_min == cand_min)) && return 0
		return 1
	fi
	return 0
}

function _shbot_semver_older() {
	local running="$1"
	local target="$2"
	[[ -z "$running" ]] && return 0

	local r_maj="${running%%.*}"
	local r_rest="${running#*.}"
	local r_min="${r_rest%%.*}"
	local r_pat="${r_rest#*.}"
	if [[ "$r_rest" == "$running" || "$r_pat" == "$r_rest" || "$r_pat" == *.* ]]; then
		return 0
	fi
	if [[ -z "$r_maj" || -z "$r_min" || -z "$r_pat" || "$r_maj" == *[!0-9]* || "$r_min" == *[!0-9]* || "$r_pat" == *[!0-9]* ]]; then
		return 0
	fi

	local t_maj="${target%%.*}"
	local t_rest="${target#*.}"
	local t_min="${t_rest%%.*}"
	local t_pat="${t_rest#*.}"
	if [[ "$t_rest" == "$target" || "$t_pat" == "$t_rest" || "$t_pat" == *.* ]]; then
		return 1
	fi
	if [[ -z "$t_maj" || -z "$t_min" || -z "$t_pat" || "$t_maj" == *[!0-9]* || "$t_min" == *[!0-9]* || "$t_pat" == *[!0-9]* ]]; then
		return 1
	fi

	if ((r_maj < t_maj || (r_maj == t_maj && r_min < t_min) || (r_maj == t_maj && r_min == t_min && r_pat < t_pat))); then
		return 0
	fi
	return 1
}

function _shbot_find_fallback_bin() {
	setopt local_options null_glob
	local cache_root="$1"
	local target_plat="$2"
	local best_bin=""
	local best_maj=-1 best_min=-1 best_pat=-1
	local dir bin ver_dir ver
	for dir in "${cache_root}"/v*; do
		[[ -d "$dir" ]] || continue
		bin="${dir}/shbot-${target_plat}"
		[[ -x "$bin" ]] || continue
		ver_dir="${dir:t}"
		ver="${ver_dir#v}"
		if _shbot_semver_compatible "$SHBOT_VERSION" "$ver"; then
			local c_rest="${ver#*.}"
			local c_maj="${ver%%.*}"
			local c_min="${c_rest%%.*}"
			local c_pat="${c_rest#*.}"
			if ((c_maj > best_maj || (c_maj == best_maj && c_min > best_min) || (c_maj == best_maj && c_min == best_min && c_pat > best_pat))); then
				best_maj=$c_maj
				best_min=$c_min
				best_pat=$c_pat
				best_bin="$bin"
			fi
		fi
	done
	if [[ -n "$best_bin" ]]; then
		_shbot_debug "Falling back to SemVer-compatible cached binary v${best_maj}.${best_min}.${best_pat}: $best_bin"
		REPLY="$best_bin"
		return 0
	fi
	return 1
}

function _shbot_download_bin_unlocked() {
	local target_plat="$1"
	local dest_bin="$2"
	local cache_dir="${dest_bin:h}"
	local archive="shbot-v${SHBOT_VERSION}-${target_plat}.tar.gz"
	local base_url="${SHBOT_RELEASE_BASE_URL:-https://github.com/${SHBOT_REPO}/releases/download/v${SHBOT_VERSION}}"
	local connect_timeout="${SHBOT_DOWNLOAD_CONNECT_TIMEOUT:-2}"
	local max_time="${SHBOT_DOWNLOAD_MAX_TIME:-10}"

	command mkdir -p -m 0700 "$cache_dir" 2>/dev/null || return 1
	local tmp_dir
	tmp_dir="$(command mktemp -d "${cache_dir}/.tmp.XXXXXX" 2>/dev/null)" || return 1

	_shbot_debug "Downloading ${base_url}/${archive}"
	if command -v curl >/dev/null 2>&1; then
		if ! command curl -fsSL --connect-timeout "$connect_timeout" --max-time "$max_time" "${base_url}/${archive}" -o "${tmp_dir}/${archive}" 2>/dev/null ||
			! command curl -fsSL --connect-timeout "$connect_timeout" --max-time "$max_time" "${base_url}/sha256sums.txt" -o "${tmp_dir}/sha256sums.txt" 2>/dev/null; then
			_shbot_debug "Failed to download release assets from ${base_url}"
			command rm -rf "$tmp_dir"
			return 1
		fi
	elif command -v wget >/dev/null 2>&1; then
		if ! command wget -q -T "$connect_timeout" -O "${tmp_dir}/${archive}" "${base_url}/${archive}" 2>/dev/null ||
			! command wget -q -T "$connect_timeout" -O "${tmp_dir}/sha256sums.txt" "${base_url}/sha256sums.txt" 2>/dev/null; then
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

function _shbot_download_bin() {
	local target_plat="$1"
	local dest_bin="$2"
	local cache_root="${dest_bin:h:h}"
	local cooldown="${SHBOT_DOWNLOAD_RETRY_COOLDOWN:-300}"

	if ! _shbot_platform_supported "$target_plat"; then
		_shbot_debug "Skipping download: platform ${target_plat} is not in SHBOT_SUPPORTED_PLATFORMS"
		return 1
	fi

	if ((SHBOT_LAST_DOWNLOAD_FAILURE > 0 && EPOCHSECONDS - SHBOT_LAST_DOWNLOAD_FAILURE < cooldown)); then
		_shbot_debug "Skipping download (in retry cooldown)"
		return 1
	fi

	command mkdir -p -m 0700 "$cache_root" 2>/dev/null || return 1
	local lock_file="${cache_root}/.download.lock"
	: >>"$lock_file" 2>/dev/null || return 1

	local lock_fd=""
	if ! zsystem flock -f lock_fd "$lock_file" 2>/dev/null; then
		_shbot_debug "Failed to acquire download lock: $lock_file"
		return 1
	fi

	if [[ -x "$dest_bin" ]]; then
		_shbot_debug "Binary appeared while waiting for lock: $dest_bin"
		exec {lock_fd}<&-
		SHBOT_LAST_DOWNLOAD_FAILURE=0
		return 0
	fi

	local rc=0
	_shbot_download_bin_unlocked "$target_plat" "$dest_bin" || rc=$?
	exec {lock_fd}<&-

	if ((rc == 0)); then
		SHBOT_LAST_DOWNLOAD_FAILURE=0
		return 0
	fi

	SHBOT_LAST_DOWNLOAD_FAILURE=$EPOCHSECONDS
	return 1
}

function _shbot_resolve_bin() {
	if [[ -n "$SHBOT_BIN" ]]; then
		if [[ -x "$SHBOT_BIN" ]]; then
			REPLY="$SHBOT_BIN"
			return 0
		elif [[ -n "${commands[$SHBOT_BIN]:-}" ]]; then
			REPLY="${commands[$SHBOT_BIN]}"
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

	if _shbot_find_fallback_bin "$cache_root" "$target_plat"; then
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
	setopt local_options sh_word_split no_glob
	[[ -n "$SHBOT_DEBUG" ]] && _shbot_debug "Updating VCS status: $1"
	local payload="$1"
	if [[ -z "$payload" ]]; then
		unset VCS_STATUS_{WORKDIR,COMMIT,LOCAL_BRANCH,REMOTE_BRANCH,REMOTE_NAME,REMOTE_URL,ACTION,INDEX_SIZE,NUM_STAGED,NUM_UNSTAGED,NUM_CONFLICTED,NUM_UNTRACKED,HAS_STAGED,HAS_UNSTAGED,HAS_CONFLICTED,HAS_UNTRACKED,COMMITS_AHEAD,COMMITS_BEHIND,STASHES,TAG,NUM_UNSTAGED_DELETED,NUM_STAGED_NEW,NUM_STAGED_DELETED,PUSH_REMOTE_NAME,PUSH_REMOTE_URL,PUSH_COMMITS_AHEAD,PUSH_COMMITS_BEHIND,NUM_SKIP_WORKTREE,NUM_ASSUME_UNCHANGED,COMMIT_ENCODING,COMMIT_SUMMARY}
		typeset -g VCS_STATUS_RESULT=norepo-sync
		return 0
	fi

	local padded="${payload}"$'\x1f'
	local IFS=$'\x1f'
	local -a resp=($padded)

	if [[ -z "${resp[1]}" ]]; then
		unset VCS_STATUS_{WORKDIR,COMMIT,LOCAL_BRANCH,REMOTE_BRANCH,REMOTE_NAME,REMOTE_URL,ACTION,INDEX_SIZE,NUM_STAGED,NUM_UNSTAGED,NUM_CONFLICTED,NUM_UNTRACKED,HAS_STAGED,HAS_UNSTAGED,HAS_CONFLICTED,HAS_UNTRACKED,COMMITS_AHEAD,COMMITS_BEHIND,STASHES,TAG,NUM_UNSTAGED_DELETED,NUM_STAGED_NEW,NUM_STAGED_DELETED,PUSH_REMOTE_NAME,PUSH_REMOTE_URL,PUSH_COMMITS_AHEAD,PUSH_COMMITS_BEHIND,NUM_SKIP_WORKTREE,NUM_ASSUME_UNCHANGED,COMMIT_ENCODING,COMMIT_SUMMARY}
		typeset -g VCS_STATUS_RESULT=norepo-sync
		return 0
	fi

	typeset -g \
		VCS_STATUS_WORKDIR="${resp[1]}" \
		VCS_STATUS_COMMIT="${resp[2]}" \
		VCS_STATUS_LOCAL_BRANCH="${resp[3]}" \
		VCS_STATUS_REMOTE_BRANCH="${resp[4]}" \
		VCS_STATUS_REMOTE_NAME="${resp[5]}" \
		VCS_STATUS_REMOTE_URL="${resp[6]}" \
		VCS_STATUS_ACTION="${resp[7]}" \
		VCS_STATUS_TAG="${resp[16]}" \
		VCS_STATUS_PUSH_REMOTE_NAME="${resp[20]}" \
		VCS_STATUS_PUSH_REMOTE_URL="${resp[21]}" \
		VCS_STATUS_COMMIT_ENCODING="${resp[26]}" \
		VCS_STATUS_COMMIT_SUMMARY="${resp[27]}" \
		VCS_STATUS_RESULT=ok-sync

	typeset -gi \
		VCS_STATUS_INDEX_SIZE="${resp[8]}" \
		VCS_STATUS_NUM_STAGED="${resp[9]}" \
		VCS_STATUS_NUM_UNSTAGED="${resp[10]}" \
		VCS_STATUS_NUM_CONFLICTED="${resp[11]}" \
		VCS_STATUS_NUM_UNTRACKED="${resp[12]}" \
		VCS_STATUS_COMMITS_AHEAD="${resp[13]}" \
		VCS_STATUS_COMMITS_BEHIND="${resp[14]}" \
		VCS_STATUS_STASHES="${resp[15]}" \
		VCS_STATUS_NUM_UNSTAGED_DELETED="${resp[17]}" \
		VCS_STATUS_NUM_STAGED_NEW="${resp[18]}" \
		VCS_STATUS_NUM_STAGED_DELETED="${resp[19]}" \
		VCS_STATUS_PUSH_COMMITS_AHEAD="${resp[22]}" \
		VCS_STATUS_PUSH_COMMITS_BEHIND="${resp[23]}" \
		VCS_STATUS_NUM_SKIP_WORKTREE="${resp[24]}" \
		VCS_STATUS_NUM_ASSUME_UNCHANGED="${resp[25]}"

	typeset -gi \
		VCS_STATUS_HAS_STAGED=$((VCS_STATUS_NUM_STAGED > 0)) \
		VCS_STATUS_HAS_UNSTAGED=$((VCS_STATUS_NUM_UNSTAGED > 0)) \
		VCS_STATUS_HAS_CONFLICTED=$((VCS_STATUS_NUM_CONFLICTED > 0)) \
		VCS_STATUS_HAS_UNTRACKED=$((VCS_STATUS_NUM_UNTRACKED > 0))
}

function _shbot_send() {
	if [[ $SHBOT_FD -eq 0 ]]; then
		[[ -n "$SHBOT_DEBUG" ]] && _shbot_debug "Cannot send '$1': no active connection"
		return 1
	fi
	if [[ "$1" == *$'\n'* || "$1" == *$'\r'* ]]; then
		[[ -n "$SHBOT_DEBUG" ]] && _shbot_debug "Refusing to send command containing a newline"
		return 1
	fi
	[[ -n "$SHBOT_DEBUG" ]] && _shbot_debug "Sending command: $1"
	if ! syswrite -o $SHBOT_FD "$1"$'\n'; then
		[[ -n "$SHBOT_DEBUG" ]] && _shbot_debug "Write failed: $1"
		return 1
	fi
}

function _shbot_connect() {
	local socket_path="${SHBOT_SOCKET:-${XDG_STATE_HOME:-$HOME/.local/state}/shbot/$HOST.socket}"
	_shbot_debug "Connecting to socket: $socket_path"

	if [[ $SHBOT_FD -ne 0 ]]; then
		_shbot_debug "Closing existing connection (FD: $SHBOT_FD)"
		zle -F $SHBOT_FD 2>/dev/null
		exec {SHBOT_FD}<&-
		SHBOT_FD=0
	fi

	local needs_connect=0
	if [[ -z "$SHBOT_SOCKET" ]]; then
		local version_file="${socket_path%.socket}.version"
		local running_version=""
		[[ -r "$version_file" ]] && IFS= read -r running_version <"$version_file"
		if _shbot_semver_older "$running_version" "$SHBOT_VERSION"; then
			_shbot_debug "Daemon version '${running_version:-unknown}' is older than ${SHBOT_VERSION}; upgrading..."
			needs_connect=1
		fi
	fi

	# Optimistically try connecting when daemon is up-to-date
	if ((needs_connect)) || ! zsocket "$socket_path" 2>/dev/null; then
		_shbot_debug "Starting or upgrading daemon..."
		if _shbot_resolve_bin; then
			local shbot_cmd="$REPLY"
			command "$shbot_cmd" connect || _shbot_debug "Daemon start failed"
		else
			_shbot_debug "Failed to resolve shbot binary"
		fi
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

	if ! IFS= read -r -u $fd line; then
		_shbot_debug "Failed to read from FD $fd (disconnecting)"
		zle -F $fd
		exec {fd}<&-
		SHBOT_FD=0
		return 1
	fi

	local updated=0
	local marker=$'\x01' nl=$'\n' cr=$'\r' us=$'\x1f'
	while true; do
		[[ -n "$SHBOT_DEBUG" ]] && _shbot_debug "Read line: $line"

		local msg_type="${line%% *}"
		local rest="${line#* }"

		if [[ "$msg_type" == "update" ]]; then
			local state_id="${rest%% *}"
			local payload=""
			if [[ "$rest" == *" "* ]]; then
				payload="${rest#* }"
			fi
			if [[ "$payload" == *%* ]]; then
				payload="${payload//'%25'/$marker}"
				payload="${payload//'%0A'/$nl}"
				payload="${payload//'%0D'/$cr}"
				payload="${payload//'%1F'/$us}"
				payload="${payload//'%00'/}"
				payload="${payload//"$marker"/%}"
				payload="${payload//'%01'/$marker}"
			fi
			SHBOT_STATE[$state_id]="$payload"
			[[ -n "$SHBOT_DEBUG" ]] && _shbot_debug "Updated state: $state_id = $payload"

			if [[ "$state_id" == "git" && -n "$SHBOT_SET_VCS_STATUS" ]]; then
				_shbot_update_vcs_status "$payload"
			fi

			updated=1
		fi

		IFS= read -r -t 0 -u $fd line || break
	done

	if ((updated)); then
		zle reset-prompt
	fi
}

function _shbot_chpwd_hook() {
	[[ -n "$SHBOT_DEBUG" ]] && _shbot_debug "chpwd triggered ($PWD)"
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

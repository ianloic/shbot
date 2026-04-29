use crate::error::Result;
use git2::{Repository, StatusOptions};
use std::path::{Path, PathBuf};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct GitState {
    pub workdir: String,
    pub commit: String,
    pub local_branch: String,
    pub remote_branch: String,
    pub remote_name: String,
    pub remote_url: String,
    pub action: String,
    pub index_size: usize,
    pub num_staged: usize,
    pub num_unstaged: usize,
    pub num_conflicted: usize,
    pub num_untracked: usize,
    pub commits_ahead: usize,
    pub commits_behind: usize,
    pub stashes: usize,
    pub tag: String,
    pub num_unstaged_deleted: usize,
    pub num_staged_new: usize,
    pub num_staged_deleted: usize,
    pub push_remote_name: String,
    pub push_remote_url: String,
    pub push_commits_ahead: usize,
    pub push_commits_behind: usize,
    pub num_skip_worktree: usize,
    pub num_assume_unchanged: usize,
    pub commit_encoding: String,
    pub commit_summary: String,
}

impl GitState {
    pub fn to_payload(&self) -> String {
        format!(
            "{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}",
            self.workdir,
            self.commit,
            self.local_branch,
            self.remote_branch,
            self.remote_name,
            self.remote_url,
            self.action,
            self.index_size,
            self.num_staged,
            self.num_unstaged,
            self.num_conflicted,
            self.num_untracked,
            self.commits_ahead,
            self.commits_behind,
            self.stashes,
            self.tag,
            self.num_unstaged_deleted,
            self.num_staged_new,
            self.num_staged_deleted,
            self.push_remote_name,
            self.push_remote_url,
            self.push_commits_ahead,
            self.push_commits_behind,
            self.num_skip_worktree,
            self.num_assume_unchanged,
            self.commit_encoding,
            self.commit_summary
        )
    }
}

pub fn get_git_state(path: &Path) -> Result<Option<(PathBuf, GitState)>> {
    tracing::debug!("Evaluating git state for path: {:?}", path);
    // Discover repository. It traverses upwards automatically.
    let mut repo = match Repository::discover(path) {
        Ok(r) => r,
        Err(_) => return Ok(None),
    };

    let Some(workdir) = repo.workdir() else {
        return Ok(None); // Bare repo
    };

    let root = workdir.to_path_buf();

    let mut state = GitState::default();
    state.workdir = root.to_string_lossy().into_owned();

    // Determine branch name
    if let Ok(head) = repo.head() {
        if let Some(name) = head.shorthand() {
            state.local_branch = name.to_string();
        }

        if let Some(target) = head.target() {
            let mut oid_str = target.to_string();
            state.commit = oid_str.clone();

            if state.local_branch.is_empty() {
                oid_str.truncate(7);
                state.local_branch = oid_str;
            }

            // Tag
            if let Ok(describe) = repo.describe(
                git2::DescribeOptions::new()
                    .describe_tags()
                    .max_candidates_tags(0),
            ) {
                if let Ok(format) = describe.format(None) {
                    state.tag = format;
                }
            }

            // Commit encoding & summary
            if let Ok(commit) = repo.find_commit(target) {
                if let Some(enc) = commit.message_encoding() {
                    state.commit_encoding = enc.to_string();
                } else {
                    state.commit_encoding = "UTF-8".to_string();
                }
                if let Some(summary) = commit.summary() {
                    state.commit_summary = summary.to_string();
                }
            }
        }

        // Calculate ahead/behind if there is an upstream
        if let Some(name) = head.name() {
            if let Ok(local) = repo.branch_upstream_name(name) {
                if let Some(local_str) = local.as_str() {
                    if let Ok(upstream_obj) = repo.revparse_single(local_str) {
                        if let Ok(upstream_commit) = upstream_obj.peel_to_commit() {
                            let local_oid = head.target().unwrap();
                            let upstream_oid = upstream_commit.id();
                            if let Ok((ahead, behind)) =
                                repo.graph_ahead_behind(local_oid, upstream_oid)
                            {
                                state.commits_ahead = ahead;
                                state.commits_behind = behind;
                            }
                        }
                    }

                    if let Ok(buf) = repo.branch_upstream_remote(name) {
                        if let Some(remote_name) = buf.as_str() {
                            state.remote_name = remote_name.to_string();

                            if let Ok(remote) = repo.find_remote(remote_name) {
                                if let Some(url) = remote.url() {
                                    state.remote_url = url.to_string();
                                }
                            }

                            let prefix = format!("refs/remotes/{}/", remote_name);
                            if local_str.starts_with(&prefix) {
                                state.remote_branch = local_str[prefix.len()..].to_string();
                            } else {
                                state.remote_branch = local_str.to_string();
                            }
                        }
                    }
                }
            }
        }
    } else {
        state.local_branch = "main".to_string(); // Initial repo, no commits
    }

    // Determine Action
    let git_dir = repo.path();
    if git_dir.join("rebase-merge").exists() || git_dir.join("rebase-apply").exists() {
        state.action = "rebase".to_string();
    } else if git_dir.join("MERGE_HEAD").exists() {
        state.action = "merge".to_string();
    } else if git_dir.join("CHERRY_PICK_HEAD").exists() {
        state.action = "cherry".to_string();
    } else if git_dir.join("REVERT_HEAD").exists() {
        state.action = "revert".to_string();
    } else if git_dir.join("BISECT_LOG").exists() {
        state.action = "bisect".to_string();
    }

    // Determine statuses
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .renames_head_to_index(true)
        .include_ignored(false)
        .update_index(false);

    if let Ok(statuses) = repo.statuses(Some(&mut opts)) {
        for entry in statuses.iter() {
            let s = entry.status();

            if s.is_conflicted() {
                state.num_conflicted += 1;
                continue;
            }

            if s.intersects(
                git2::Status::INDEX_NEW
                    | git2::Status::INDEX_MODIFIED
                    | git2::Status::INDEX_DELETED
                    | git2::Status::INDEX_RENAMED
                    | git2::Status::INDEX_TYPECHANGE,
            ) {
                state.num_staged += 1;
                if s.intersects(git2::Status::INDEX_NEW) {
                    state.num_staged_new += 1;
                }
                if s.intersects(git2::Status::INDEX_DELETED) {
                    state.num_staged_deleted += 1;
                }
            }

            if s.intersects(
                git2::Status::WT_MODIFIED
                    | git2::Status::WT_DELETED
                    | git2::Status::WT_TYPECHANGE
                    | git2::Status::WT_RENAMED,
            ) {
                state.num_unstaged += 1;
                if s.intersects(git2::Status::WT_DELETED) {
                    state.num_unstaged_deleted += 1;
                }
            }

            if s.intersects(git2::Status::WT_NEW) {
                state.num_untracked += 1;
            }
        }
    }

    // Stashes
    let mut stashes = 0;
    let _ = repo.stash_foreach(|_, _, _| {
        stashes += 1;
        true
    });
    state.stashes = stashes;

    // Index size
    if let Ok(index) = repo.index() {
        state.index_size = index.len();
        // Skip-worktree and assume-unchanged counts left at 0 as they require flag checking
    }

    // Push remote
    if let Ok(head) = repo.head() {
        if let Some(name) = head.name() {
            let branch_name = name.trim_start_matches("refs/heads/");
            let mut push_remote = None;

            if let Ok(cfg) = repo.config() {
                let push_remote_key = format!("branch.{}.pushRemote", branch_name);
                if let Ok(pr) = cfg.get_string(&push_remote_key) {
                    push_remote = Some(pr);
                } else if let Ok(pr) = cfg.get_string("remote.pushDefault") {
                    push_remote = Some(pr);
                } else if let Ok(buf) = repo.branch_upstream_remote(name) {
                    if let Some(r) = buf.as_str() {
                        push_remote = Some(r.to_string());
                    }
                }
            }

            if let Some(r) = push_remote {
                state.push_remote_name = r.clone();
                if let Ok(remote) = repo.find_remote(&r) {
                    if let Some(url) = remote.url() {
                        state.push_remote_url = url.to_string();
                    }
                }

                // push commits ahead/behind
                let push_ref = format!("refs/remotes/{}/{}", r, branch_name);
                if let Ok(upstream_obj) = repo.revparse_single(&push_ref) {
                    if let Ok(upstream_commit) = upstream_obj.peel_to_commit() {
                        if let Some(local_oid) = head.target() {
                            if let Ok((ahead, behind)) =
                                repo.graph_ahead_behind(local_oid, upstream_commit.id())
                            {
                                state.push_commits_ahead = ahead;
                                state.push_commits_behind = behind;
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(Some((root, state)))
}

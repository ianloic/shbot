use anyhow::Result;
use git2::{Repository, StatusOptions};
use std::path::{Path, PathBuf};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct GitState {
    pub branch: String,
    pub ahead: usize,
    pub behind: usize,
    pub staged: usize,
    pub unstaged: usize,
    pub untracked: usize,
    pub conflicted: usize,
}

impl GitState {
    pub fn to_payload(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}|{}",
            self.branch,
            self.ahead,
            self.behind,
            self.staged,
            self.unstaged,
            self.untracked,
            self.conflicted
        )
    }
}

pub fn get_git_state(path: &Path) -> Result<Option<(PathBuf, GitState)>> {
    // Discover repository. It traverses upwards automatically.
    let repo = match Repository::discover(path) {
        Ok(r) => r,
        Err(_) => return Ok(None),
    };

    let Some(workdir) = repo.workdir() else {
        return Ok(None); // Bare repo
    };

    let root = workdir.to_path_buf();

    let mut state = GitState::default();

    // Determine branch name
    if let Ok(head) = repo.head() {
        if let Some(name) = head.shorthand().map(|s| s.to_string()) {
            state.branch = name;
        } else {
            // Detached HEAD or similar
            let target = head.target().unwrap_or(git2::Oid::zero());
            let mut oid_str = target.to_string();
            oid_str.truncate(7);
            state.branch = oid_str;
        }

        // Calculate ahead/behind if there is an upstream
        if let Ok(local) = repo.branch_upstream_name(head.name().unwrap_or("")) {
            if let Some(local_str) = local.as_str() {
                if let Ok(upstream_obj) = repo.revparse_single(local_str) {
                    if let Ok(upstream_commit) = upstream_obj.peel_to_commit() {
                        let local_oid = head.target().unwrap();
                        let upstream_oid = upstream_commit.id();
                        if let Ok((ahead, behind)) =
                            repo.graph_ahead_behind(local_oid, upstream_oid)
                        {
                            state.ahead = ahead;
                            state.behind = behind;
                        }
                    }
                }
            }
        }
    } else {
        state.branch = "main".to_string(); // Initial repo, no commits
    }

    // Determine statuses
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .renames_head_to_index(true)
        .include_ignored(false);

    if let Ok(statuses) = repo.statuses(Some(&mut opts)) {
        for entry in statuses.iter() {
            let s = entry.status();

            if s.is_conflicted() {
                state.conflicted += 1;
                continue;
            }

            if s.intersects(
                git2::Status::INDEX_NEW
                    | git2::Status::INDEX_MODIFIED
                    | git2::Status::INDEX_DELETED
                    | git2::Status::INDEX_RENAMED
                    | git2::Status::INDEX_TYPECHANGE,
            ) {
                state.staged += 1;
            }

            if s.intersects(
                git2::Status::WT_MODIFIED
                    | git2::Status::WT_DELETED
                    | git2::Status::WT_TYPECHANGE
                    | git2::Status::WT_RENAMED,
            ) {
                state.unstaged += 1;
            }

            if s.intersects(git2::Status::WT_NEW) {
                state.untracked += 1;
            }
        }
    }

    Ok(Some((root, state)))
}

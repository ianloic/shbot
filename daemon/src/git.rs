// Copyright (C) 2026 Ian McKellar
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

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

            if state.local_branch.is_empty() || state.local_branch == "HEAD" {
                oid_str.truncate(7);
                state.local_branch = oid_str;
            }

            // Tag
            if let Ok(describe) = repo.describe(
                git2::DescribeOptions::new()
                    .describe_tags()
                    .max_candidates_tags(0),
            ) && let Ok(format) = describe.format(None)
            {
                state.tag = format;
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
        if let Some(name) = head.name()
            && let Ok(local) = repo.branch_upstream_name(name)
            && let Some(local_str) = local.as_str()
        {
            if let Ok(upstream_obj) = repo.revparse_single(local_str)
                && let Ok(upstream_commit) = upstream_obj.peel_to_commit()
            {
                let local_oid = head.target().unwrap();
                let upstream_oid = upstream_commit.id();
                if let Ok((ahead, behind)) = repo.graph_ahead_behind(local_oid, upstream_oid) {
                    state.commits_ahead = ahead;
                    state.commits_behind = behind;
                }
            }

            if let Ok(buf) = repo.branch_upstream_remote(name)
                && let Some(remote_name) = buf.as_str()
            {
                state.remote_name = remote_name.to_string();

                if let Ok(remote) = repo.find_remote(remote_name)
                    && let Some(url) = remote.url()
                {
                    state.remote_url = url.to_string();
                }

                let prefix = format!("refs/remotes/{}/", remote_name);
                if local_str.starts_with(&prefix) {
                    state.remote_branch = local_str[prefix.len()..].to_string();
                } else {
                    state.remote_branch = local_str.to_string();
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
    if let Ok(head) = repo.head()
        && let Some(name) = head.name()
    {
        let branch_name = name.trim_start_matches("refs/heads/");
        let mut push_remote = None;

        if let Ok(cfg) = repo.config() {
            let push_remote_key = format!("branch.{}.pushRemote", branch_name);
            if let Ok(pr) = cfg.get_string(&push_remote_key) {
                push_remote = Some(pr);
            } else if let Ok(pr) = cfg.get_string("remote.pushDefault") {
                push_remote = Some(pr);
            } else if let Ok(buf) = repo.branch_upstream_remote(name)
                && let Some(r) = buf.as_str()
            {
                push_remote = Some(r.to_string());
            }
        }

        if let Some(r) = push_remote {
            state.push_remote_name = r.clone();
            if let Ok(remote) = repo.find_remote(&r)
                && let Some(url) = remote.url()
            {
                state.push_remote_url = url.to_string();
            }

            // push commits ahead/behind
            let push_ref = format!("refs/remotes/{}/{}", r, branch_name);
            if let Ok(upstream_obj) = repo.revparse_single(&push_ref)
                && let Ok(upstream_commit) = upstream_obj.peel_to_commit()
                && let Some(local_oid) = head.target()
                && let Ok((ahead, behind)) =
                    repo.graph_ahead_behind(local_oid, upstream_commit.id())
            {
                state.push_commits_ahead = ahead;
                state.push_commits_behind = behind;
            }
        }
    }

    Ok(Some((root, state)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::tempdir;

    fn run_git(args: &[&str], cwd: &Path) {
        let status = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .expect("Failed to execute git");
        assert!(status.success());
    }

    #[test]
    fn test_git_state_uninitialized() {
        let dir = tempdir().unwrap();
        let state = get_git_state(dir.path()).unwrap();
        assert!(state.is_none());
    }

    #[test]
    fn test_git_state_initialized() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        run_git(&["init"], root);
        run_git(&["config", "user.name", "Test User"], root);
        run_git(&["config", "user.email", "test@example.com"], root);

        let (_, state) = get_git_state(root).unwrap().unwrap();
        assert_eq!(state.local_branch, "main");
        assert_eq!(state.num_staged, 0);
        assert_eq!(state.num_unstaged, 0);
        assert_eq!(state.num_untracked, 0);

        // Add an untracked file
        fs::write(root.join("test.txt"), "hello").unwrap();
        let (_, state) = get_git_state(root).unwrap().unwrap();
        assert_eq!(state.num_untracked, 1);

        // Stage the file
        run_git(&["add", "test.txt"], root);
        let (_, state) = get_git_state(root).unwrap().unwrap();
        assert_eq!(state.num_staged, 1);
        assert_eq!(state.num_untracked, 0);

        // Commit the file
        run_git(&["commit", "-m", "Initial commit"], root);
        let (_, state) = get_git_state(root).unwrap().unwrap();
        assert_eq!(state.num_staged, 0);
        assert_eq!(state.num_untracked, 0);
        assert_eq!(state.commit_summary, "Initial commit");

        // Modify the file
        fs::write(root.join("test.txt"), "hello world").unwrap();
        let (_, state) = get_git_state(root).unwrap().unwrap();
        assert_eq!(state.num_unstaged, 1);

        // Stage the modification
        run_git(&["add", "test.txt"], root);
        let (_, state) = get_git_state(root).unwrap().unwrap();
        assert_eq!(state.num_staged, 1);
        assert_eq!(state.num_unstaged, 0);

        // Test detached HEAD
        run_git(&["checkout", "--detach", "HEAD"], root);
        let (_, state) = get_git_state(root).unwrap().unwrap();
        assert_eq!(state.local_branch, state.commit[..7].to_string());
    }

    #[test]
    fn test_git_state_to_payload() {
        let state = GitState {
            workdir: "/work".to_string(),
            commit: "1234567890abcdef".to_string(),
            local_branch: "main".to_string(),
            ..Default::default()
        };
        let payload = state.to_payload();
        assert_eq!(payload.matches('\x1f').count(), 26);
        assert!(payload.starts_with("/work\x1f1234567890abcdef\x1fmain\x1f"));
    }

    #[test]
    fn test_git_action_states() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        run_git(&["init"], root);
        let git_dir = root.join(".git");

        // Test merge
        fs::write(git_dir.join("MERGE_HEAD"), "dummy").unwrap();
        let (_, state) = get_git_state(root).unwrap().unwrap();
        assert_eq!(state.action, "merge");
        fs::remove_file(git_dir.join("MERGE_HEAD")).unwrap();

        // Test rebase
        fs::create_dir(git_dir.join("rebase-merge")).unwrap();
        let (_, state) = get_git_state(root).unwrap().unwrap();
        assert_eq!(state.action, "rebase");
        fs::remove_dir(git_dir.join("rebase-merge")).unwrap();

        // Test cherry-pick
        fs::write(git_dir.join("CHERRY_PICK_HEAD"), "dummy").unwrap();
        let (_, state) = get_git_state(root).unwrap().unwrap();
        assert_eq!(state.action, "cherry");
        fs::remove_file(git_dir.join("CHERRY_PICK_HEAD")).unwrap();

        // Test revert
        fs::write(git_dir.join("REVERT_HEAD"), "dummy").unwrap();
        let (_, state) = get_git_state(root).unwrap().unwrap();
        assert_eq!(state.action, "revert");
        fs::remove_file(git_dir.join("REVERT_HEAD")).unwrap();

        // Test bisect
        fs::write(git_dir.join("BISECT_LOG"), "dummy").unwrap();
        let (_, state) = get_git_state(root).unwrap().unwrap();
        assert_eq!(state.action, "bisect");
        fs::remove_file(git_dir.join("BISECT_LOG")).unwrap();
    }

    #[test]
    fn test_git_remote_tracking() {
        let remote_dir = tempdir().unwrap();
        let remote_root = remote_dir.path();
        run_git(&["init", "--bare", "--initial-branch=main"], remote_root);

        let local_dir = tempdir().unwrap();
        let local_root = local_dir.path();
        run_git(&["init", "--initial-branch=main"], local_root);
        run_git(&["config", "user.name", "Test User"], local_root);
        run_git(&["config", "user.email", "test@example.com"], local_root);

        // create a commit
        fs::write(local_root.join("test.txt"), "hello").unwrap();
        run_git(&["add", "test.txt"], local_root);
        run_git(&["commit", "-m", "Initial commit"], local_root);

        // Add remote
        let remote_url = format!("file://{}", remote_root.display());
        run_git(&["remote", "add", "origin", &remote_url], local_root);

        // Push to set upstream
        run_git(&["push", "-u", "origin", "main"], local_root);

        // check state
        let (_, state) = get_git_state(local_root).unwrap().unwrap();
        assert_eq!(state.remote_name, "origin");
        assert_eq!(state.remote_url, remote_url);
        assert_eq!(state.remote_branch, "main");
        assert_eq!(state.commits_ahead, 0);
        assert_eq!(state.commits_behind, 0);

        // Make another commit local to be ahead
        fs::write(local_root.join("test.txt"), "hello ahead").unwrap();
        run_git(&["add", "test.txt"], local_root);
        run_git(&["commit", "-m", "Ahead commit"], local_root);

        let (_, state) = get_git_state(local_root).unwrap().unwrap();
        assert_eq!(state.commits_ahead, 1);
        assert_eq!(state.commits_behind, 0);

        // set push remote differently
        run_git(&["config", "branch.main.pushRemote", "origin"], local_root);
        let (_, state) = get_git_state(local_root).unwrap().unwrap();
        assert_eq!(state.push_remote_name, "origin");
        assert_eq!(state.push_commits_ahead, 1);

        // test remote.pushDefault
        run_git(&["config", "--unset", "branch.main.pushRemote"], local_root);
        run_git(&["config", "remote.pushDefault", "origin"], local_root);
        let (_, state) = get_git_state(local_root).unwrap().unwrap();
        assert_eq!(state.push_remote_name, "origin");
        assert_eq!(state.push_commits_ahead, 1);
    }

    #[test]
    fn test_git_bare_repo() {
        let dir = tempdir().unwrap();
        run_git(&["init", "--bare"], dir.path());
        let state = get_git_state(dir.path()).unwrap();
        assert!(state.is_none());
    }

    #[test]
    fn test_git_tag_and_encoding() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        run_git(&["init"], root);
        run_git(&["config", "user.name", "Test User"], root);
        run_git(&["config", "user.email", "test@example.com"], root);
        run_git(&["config", "i18n.commitEncoding", "ISO-8859-1"], root);

        fs::write(root.join("test.txt"), "hello").unwrap();
        run_git(&["add", "test.txt"], root);
        run_git(&["commit", "-m", "Initial commit"], root);

        // Create an annotated tag
        run_git(&["tag", "-a", "v1.0", "-m", "Version 1.0"], root);

        let (_, state) = get_git_state(root).unwrap().unwrap();
        assert_eq!(state.tag, "v1.0");
        assert_eq!(state.commit_encoding, "ISO-8859-1");
    }

    #[test]
    fn test_git_status_conflicted() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        run_git(&["init", "--initial-branch=main"], root);
        run_git(&["config", "user.name", "Test User"], root);
        run_git(&["config", "user.email", "test@example.com"], root);

        fs::write(root.join("test.txt"), "base").unwrap();
        run_git(&["add", "test.txt"], root);
        run_git(&["commit", "-m", "base"], root);

        run_git(&["checkout", "-b", "feature"], root);
        fs::write(root.join("test.txt"), "feature").unwrap();
        run_git(&["commit", "-am", "feature"], root);

        run_git(&["checkout", "main"], root);
        fs::write(root.join("test.txt"), "main").unwrap();
        run_git(&["commit", "-am", "main"], root);

        // This will cause a conflict
        let status = Command::new("git")
            .args(["merge", "feature"])
            .current_dir(root)
            .status()
            .expect("Failed to execute git");
        assert!(!status.success()); // Merge should fail due to conflict

        let (_, state) = get_git_state(root).unwrap().unwrap();
        assert_eq!(state.num_conflicted, 1);
        assert_eq!(state.action, "merge");
    }

    #[test]
    fn test_git_status_rename_delete() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        run_git(&["init", "--initial-branch=main"], root);
        run_git(&["config", "user.name", "Test User"], root);
        run_git(&["config", "user.email", "test@example.com"], root);

        fs::write(root.join("file1.txt"), "content").unwrap();
        run_git(&["add", "file1.txt"], root);
        run_git(&["commit", "-m", "initial"], root);

        // Rename staged
        run_git(&["mv", "file1.txt", "file2.txt"], root);

        let (_, state) = get_git_state(root).unwrap().unwrap();
        assert_eq!(state.num_staged, 1); // Rename counts as staged

        run_git(&["commit", "-m", "rename"], root);

        // Delete unstaged
        fs::remove_file(root.join("file2.txt")).unwrap();
        let (_, state) = get_git_state(root).unwrap().unwrap();
        assert_eq!(state.num_unstaged_deleted, 1);
        assert_eq!(state.num_unstaged, 1);

        // Stage the delete
        run_git(&["add", "file2.txt"], root);
        let (_, state) = get_git_state(root).unwrap().unwrap();
        assert_eq!(state.num_staged_deleted, 1);
        assert_eq!(state.num_staged, 1);
    }

    #[test]
    fn test_git_missing_commit_encoding() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        run_git(&["init"], root);
        run_git(&["config", "user.name", "Test User"], root);
        run_git(&["config", "user.email", "test@example.com"], root);

        fs::write(root.join("test.txt"), "hello").unwrap();
        run_git(&["add", "test.txt"], root);
        run_git(&["commit", "-m", "Initial commit"], root);

        let (_, state) = get_git_state(root).unwrap().unwrap();
        // Since we didn't specify an encoding, it defaults to none in libgit2,
        // and our code falls back to UTF-8.
        assert_eq!(state.commit_encoding, "UTF-8");
    }

    #[test]
    fn test_git_push_remote_edge_cases() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        run_git(&["init", "--initial-branch=main"], root);
        run_git(&["config", "user.name", "Test"], root);
        run_git(&["config", "user.email", "test@test.com"], root);

        fs::write(root.join("f.txt"), "a").unwrap();
        run_git(&["add", "f.txt"], root);
        run_git(&["commit", "-m", "msg"], root);

        // Setup pushRemote to a non-existent remote
        run_git(&["config", "branch.main.pushRemote", "fake_remote"], root);

        let (_, state) = get_git_state(root).unwrap().unwrap();
        assert_eq!(state.push_remote_name, "fake_remote");
        assert_eq!(state.push_remote_url, "");
        assert_eq!(state.push_commits_ahead, 0);

        // Remove config
        run_git(&["config", "--unset", "branch.main.pushRemote"], root);

        // Add a real remote but no tracking branch locally
        run_git(
            &["remote", "add", "origin", "http://example.com/repo.git"],
            root,
        );
        run_git(&["config", "branch.main.pushRemote", "origin"], root);
        let (_, state) = get_git_state(root).unwrap().unwrap();
        assert_eq!(state.push_remote_name, "origin");
        assert_eq!(state.push_remote_url, "http://example.com/repo.git");
        assert_eq!(state.push_commits_ahead, 0); // No upstream tracking branch exists locally yet
    }

    #[test]
    fn test_git_upstream_missing() {
        let local_dir = tempdir().unwrap();
        let local_root = local_dir.path();
        run_git(&["init", "--initial-branch=main"], local_root);
        run_git(&["config", "user.name", "Test User"], local_root);
        run_git(&["config", "user.email", "test@example.com"], local_root);

        fs::write(local_root.join("test.txt"), "hello").unwrap();
        run_git(&["add", "test.txt"], local_root);
        run_git(&["commit", "-m", "Initial commit"], local_root);

        // Create remote and push to establish upstream tracking
        let remote_dir = tempdir().unwrap();
        run_git(
            &["init", "--bare", "--initial-branch=main"],
            remote_dir.path(),
        );
        let remote_url = format!("file://{}", remote_dir.path().display());
        run_git(&["remote", "add", "origin", &remote_url], local_root);
        run_git(&["push", "-u", "origin", "main"], local_root);

        // Verify it tracked properly first
        let (_, state) = get_git_state(local_root).unwrap().unwrap();
        assert_eq!(state.remote_name, "origin");

        // Now remove the local tracking ref to simulate a missing upstream ref
        let ref_path = local_root.join(".git/refs/remotes/origin/main");
        if ref_path.exists() {
            fs::remove_file(ref_path).unwrap();
        }

        // Now get state again. `branch_upstream_name` works from config,
        // but `revparse_single` on the ref fails.
        let (_, state) = get_git_state(local_root).unwrap().unwrap();
        assert_eq!(state.remote_name, "origin");
        assert_eq!(state.remote_branch, "main");
        assert_eq!(state.commits_ahead, 0); // Revparse failed, so ahead/behind wasn't calculated.
    }

    #[test]
    fn test_git_corrupted_index() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        run_git(&["init", "--initial-branch=main"], root);
        fs::write(root.join(".git").join("index"), "corrupted").unwrap();

        let (_, state) = get_git_state(root).unwrap().unwrap();
        assert_eq!(state.index_size, 0); // Error branch leaves it 0
    }
}

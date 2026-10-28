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
use gix::bstr::{BString, ByteSlice};
use gix::commit::describe::SelectRef;
use gix::diff::index::Change as TreeIndexChange;
use gix::dir::entry::Status as DirEntryStatus;
use gix::head::Kind as HeadKind;
use gix::index::entry::Stage;
use gix::progress::Discard;
use gix::refs::FullName;
use gix::remote::Direction;
use gix::status::index_worktree::Item as IndexWorktreeItem;
use gix::status::plumbing::index_as_worktree::{Change as WorktreeChange, EntryStatus};
use gix::status::tree_index::TrackRenames;
use gix::status::{Item as StatusItem, Submodule, UntrackedFiles};
use gix::{ObjectId, Repository};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tracing::{debug, info};

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

fn ahead_behind(
    repo: &Repository,
    local_id: ObjectId,
    upstream_id: ObjectId,
) -> Option<(usize, usize)> {
    if local_id == upstream_id {
        return Some((0, 0));
    }
    let ahead = repo
        .rev_walk([local_id])
        .with_hidden([upstream_id])
        .all()
        .ok()?
        .flatten()
        .count();
    let behind = repo
        .rev_walk([upstream_id])
        .with_hidden([local_id])
        .all()
        .ok()?
        .flatten()
        .count();
    Some((ahead, behind))
}

pub fn get_git_state(path: &Path) -> Result<Option<(PathBuf, GitState)>> {
    debug!("Evaluating git state for path: {:?}", path);
    // Discover repository. It traverses upwards automatically.
    let repo = match gix::discover(path) {
        Ok(r) => r,
        Err(_) => return Ok(None),
    };

    let Some(workdir) = repo.workdir() else {
        return Ok(None); // Bare repo
    };

    let root = workdir.to_path_buf();

    let mut state = GitState::default();
    state.workdir = root.to_string_lossy().into_owned();

    let mut head_ref_name: Option<FullName> = None;
    let mut head_commit_id: Option<ObjectId> = None;

    // Determine branch name and HEAD commit metadata
    if let Ok(mut head) = repo.head() {
        if let HeadKind::Symbolic(r) = &head.kind {
            head_ref_name = Some(r.name.clone());
            state.local_branch = r.name.shorten().to_str_lossy().into_owned();
        }

        if let Ok(commit) = head.peel_to_commit() {
            let target = commit.id;
            head_commit_id = Some(target);
            let mut oid_str = target.to_string();
            state.commit = oid_str.clone();

            if state.local_branch.is_empty() || state.local_branch == "HEAD" {
                oid_str.truncate(7);
                state.local_branch = oid_str;
            }

            // Tag (exact match on HEAD)
            if let Ok(Some(resolution)) = commit
                .describe()
                .names(SelectRef::AllTags)
                .max_candidates(0)
                .try_resolve()
                && resolution.outcome.depth == 0
                && let Some(name) = resolution.outcome.name
            {
                state.tag = name.to_str_lossy().into_owned();
            }

            // Commit encoding & summary
            if let Ok(commit_ref) = commit.decode() {
                if let Some(enc) = commit_ref.encoding {
                    state.commit_encoding = enc.to_str_lossy().into_owned();
                } else {
                    state.commit_encoding = "UTF-8".to_string();
                }
                if let Some(first_line) = commit_ref.message.lines().next() {
                    state.commit_summary = first_line.to_str_lossy().into_owned();
                }
            }
        } else {
            state.local_branch = "main".to_string();
        }
    } else {
        state.local_branch = "main".to_string(); // Initial repo, no commits
    }

    // Calculate ahead/behind if there is an upstream
    if let Some(ref full_name) = head_ref_name
        && let Some(local_oid) = head_commit_id
        && let Some(Ok(tracking_ref_name)) =
            repo.branch_remote_tracking_ref_name(full_name.as_ref(), Direction::Fetch)
    {
        let local_str = tracking_ref_name.as_bstr().to_str_lossy().into_owned();

        if let Ok(mut upstream_ref) = repo.find_reference(local_str.as_str())
            && let Ok(upstream_commit) = upstream_ref.peel_to_commit()
            && let Some((ahead, behind)) = ahead_behind(&repo, local_oid, upstream_commit.id)
        {
            state.commits_ahead = ahead;
            state.commits_behind = behind;
        }

        let short_branch = full_name.shorten();
        if let Some(remote_name_cow) = repo.branch_remote_name(short_branch, Direction::Fetch) {
            let remote_name = remote_name_cow.as_bstr().to_str_lossy().into_owned();
            state.remote_name = remote_name.clone();

            if let Ok(remote) = repo.find_remote(remote_name.as_str())
                && let Some(url) = remote.url(Direction::Fetch)
            {
                state.remote_url = url.to_bstring().to_str_lossy().into_owned();
            }

            let prefix = format!("refs/remotes/{}/", remote_name);
            if local_str.starts_with(&prefix) {
                state.remote_branch = local_str[prefix.len()..].to_string();
            } else {
                state.remote_branch = local_str;
            }
        }
    }

    // Determine Action
    let git_dir = repo.git_dir();
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

    // Index size & conflicted entries
    let mut conflicted_paths: HashSet<BString> = HashSet::new();
    if let Ok(index) = repo.open_index() {
        state.index_size = index.entries().len();
        for entry in index.entries() {
            if entry.flags.stage() != Stage::Unconflicted {
                conflicted_paths.insert(entry.path(&index).to_owned());
            }
        }
    }
    state.num_conflicted = conflicted_paths.len();

    // Determine statuses
    let workdir = repo.workdir().unwrap_or(repo.git_dir());
    info!("Starting gix status crawl for {:?}", workdir);

    if let Ok(platform) = repo.status(Discard) {
        let platform = platform
            .untracked_files(UntrackedFiles::Collapsed)
            .index_worktree_submodules(Submodule::AsConfigured { check_dirty: true })
            .tree_index_track_renames(TrackRenames::Given(Default::default()));

        if let Ok(iter) = platform.into_iter(Vec::<BString>::new()) {
            for item in iter.flatten() {
                match item {
                    StatusItem::TreeIndex(change) => {
                        if conflicted_paths.contains(change.location()) {
                            continue;
                        }
                        match change {
                            TreeIndexChange::Addition { .. } => {
                                state.num_staged += 1;
                                state.num_staged_new += 1;
                            }
                            TreeIndexChange::Deletion { .. } => {
                                state.num_staged += 1;
                                state.num_staged_deleted += 1;
                            }
                            TreeIndexChange::Modification { .. }
                            | TreeIndexChange::Rewrite { .. } => {
                                state.num_staged += 1;
                            }
                        }
                    }
                    StatusItem::IndexWorktree(wt_item) => match wt_item {
                        IndexWorktreeItem::Modification {
                            rela_path, status, ..
                        } => {
                            if conflicted_paths.contains(&rela_path) {
                                continue;
                            }
                            match status {
                                EntryStatus::Change(WorktreeChange::Removed) => {
                                    state.num_unstaged += 1;
                                    state.num_unstaged_deleted += 1;
                                }
                                EntryStatus::Change(
                                    WorktreeChange::Type { .. }
                                    | WorktreeChange::Modification { .. }
                                    | WorktreeChange::SubmoduleModification(_),
                                ) => {
                                    state.num_unstaged += 1;
                                }
                                EntryStatus::Conflict { .. }
                                | EntryStatus::NeedsUpdate(_)
                                | EntryStatus::IntentToAdd => {}
                            }
                        }
                        IndexWorktreeItem::DirectoryContents { entry, .. } => {
                            if conflicted_paths.contains(&entry.rela_path) {
                                continue;
                            }
                            if matches!(entry.status, DirEntryStatus::Untracked) {
                                state.num_untracked += 1;
                            }
                        }
                        IndexWorktreeItem::Rewrite { dirwalk_entry, .. } => {
                            if !conflicted_paths.contains(&dirwalk_entry.rela_path) {
                                state.num_unstaged += 1;
                            }
                        }
                    },
                }
            }
        }
    }

    info!("Finished gix status crawl for {:?}", workdir);

    // Stashes
    if let Ok(stash_ref) = repo.find_reference("refs/stash")
        && let Ok(Some(log_iter)) = stash_ref.log_iter().rev()
    {
        state.stashes = log_iter.flatten().count();
    }

    // Push remote
    if let Some(ref full_name) = head_ref_name {
        let branch_name = full_name.shorten().to_str_lossy().into_owned();
        let cfg = repo.config_snapshot();
        let push_remote_key = format!("branch.{}.pushRemote", branch_name);
        let push_remote = cfg
            .string(&push_remote_key)
            .map(|s| s.to_str_lossy().into_owned())
            .or_else(|| {
                cfg.string("remote.pushDefault")
                    .map(|s| s.to_str_lossy().into_owned())
            })
            .or_else(|| {
                repo.branch_remote_name(branch_name.as_str(), Direction::Fetch)
                    .map(|r| r.as_bstr().to_str_lossy().into_owned())
            });

        if let Some(r) = push_remote {
            state.push_remote_name = r.clone();
            if let Ok(remote) = repo.find_remote(r.as_str())
                && let Some(url) = remote
                    .url(Direction::Push)
                    .or_else(|| remote.url(Direction::Fetch))
            {
                state.push_remote_url = url.to_bstring().to_str_lossy().into_owned();
            }

            // push commits ahead/behind
            let push_ref = format!("refs/remotes/{}/{}", r, branch_name);
            if let Some(local_oid) = head_commit_id
                && let Ok(mut upstream_ref) = repo.find_reference(&push_ref)
                && let Ok(upstream_commit) = upstream_ref.peel_to_commit()
                && let Some((ahead, behind)) = ahead_behind(&repo, local_oid, upstream_commit.id)
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
    use std::os::unix::fs::symlink;
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

        // Stage the file (while repository still has an unborn HEAD)
        run_git(&["add", "test.txt"], root);
        let (_, state) = get_git_state(root).unwrap().unwrap();
        assert_eq!(state.num_staged, 1);
        assert_eq!(state.num_staged_new, 1);
        assert_eq!(state.num_untracked, 0);
        assert_eq!(state.index_size, 1);

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

        // check state (including push_remote fallback to upstream remote)
        let (_, state) = get_git_state(local_root).unwrap().unwrap();
        assert_eq!(state.remote_name, "origin");
        assert_eq!(state.remote_url, remote_url);
        assert_eq!(state.remote_branch, "main");
        assert_eq!(state.commits_ahead, 0);
        assert_eq!(state.commits_behind, 0);
        assert_eq!(state.push_remote_name, "origin");
        assert_eq!(state.push_remote_url, remote_url);
        assert_eq!(state.push_commits_ahead, 0);
        assert_eq!(state.push_commits_behind, 0);

        // Make another commit local to be ahead
        fs::write(local_root.join("test.txt"), "hello ahead").unwrap();
        run_git(&["add", "test.txt"], local_root);
        run_git(&["commit", "-m", "Ahead commit"], local_root);

        let (_, state) = get_git_state(local_root).unwrap().unwrap();
        assert_eq!(state.commits_ahead, 1);
        assert_eq!(state.commits_behind, 0);
        assert_eq!(state.push_commits_ahead, 1);
        assert_eq!(state.push_commits_behind, 0);

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

    #[test]
    fn test_git_discover_from_subdirectory() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        run_git(&["init", "--initial-branch=main"], root);
        run_git(&["config", "user.name", "Test User"], root);
        run_git(&["config", "user.email", "test@example.com"], root);

        fs::write(root.join("test.txt"), "hello").unwrap();
        run_git(&["add", "test.txt"], root);
        run_git(&["commit", "-m", "Initial commit"], root);

        let sub = root.join("a").join("b").join("c");
        fs::create_dir_all(&sub).unwrap();

        let (root_from_top, state_from_top) = get_git_state(root).unwrap().unwrap();
        let (root_from_sub, state_from_sub) = get_git_state(&sub).unwrap().unwrap();

        assert_eq!(root_from_sub, root_from_top);
        assert_eq!(state_from_sub, state_from_top);
    }

    #[test]
    fn test_git_branch_with_slashes_and_multiline_commit_summary() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        run_git(&["init", "--initial-branch=main"], root);
        run_git(&["config", "user.name", "Test User"], root);
        run_git(&["config", "user.email", "test@example.com"], root);

        run_git(&["checkout", "-b", "feature/sub/topic"], root);
        fs::write(root.join("test.txt"), "hello").unwrap();
        run_git(&["add", "test.txt"], root);
        run_git(
            &[
                "commit",
                "-m",
                "First line summary\n\nSecond paragraph\nThird line",
            ],
            root,
        );

        let rev_parse = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(root)
            .output()
            .unwrap();
        let expected_sha = String::from_utf8_lossy(&rev_parse.stdout)
            .trim()
            .to_string();

        let (_, state) = get_git_state(root).unwrap().unwrap();
        assert_eq!(state.local_branch, "feature/sub/topic");
        assert_eq!(state.commit, expected_sha);
        assert_eq!(state.commit.len(), 40);
        assert_eq!(state.commit_summary, "First line summary");
    }

    #[test]
    fn test_git_tags_lightweight_annotated_and_non_head() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        run_git(&["init", "--initial-branch=main"], root);
        run_git(&["config", "user.name", "Test User"], root);
        run_git(&["config", "user.email", "test@example.com"], root);

        fs::write(root.join("test.txt"), "v1").unwrap();
        run_git(&["add", "test.txt"], root);
        run_git(&["commit", "-m", "commit 1"], root);
        run_git(&["tag", "-a", "v1.0", "-m", "Annotated 1.0"], root);

        let (_, state) = get_git_state(root).unwrap().unwrap();
        assert_eq!(state.tag, "v1.0");

        // Advance HEAD past v1.0; exact-match tag should now be empty
        fs::write(root.join("test.txt"), "v2").unwrap();
        run_git(&["commit", "-am", "commit 2"], root);

        let (_, state) = get_git_state(root).unwrap().unwrap();
        assert_eq!(state.tag, "");

        // Create a lightweight tag on HEAD
        run_git(&["tag", "v2.0-light"], root);
        let (_, state) = get_git_state(root).unwrap().unwrap();
        assert_eq!(state.tag, "v2.0-light");
    }

    #[test]
    fn test_git_stashes() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        run_git(&["init", "--initial-branch=main"], root);
        run_git(&["config", "user.name", "Test User"], root);
        run_git(&["config", "user.email", "test@example.com"], root);

        fs::write(root.join("test.txt"), "initial").unwrap();
        run_git(&["add", "test.txt"], root);
        run_git(&["commit", "-m", "initial"], root);

        let (_, state) = get_git_state(root).unwrap().unwrap();
        assert_eq!(state.stashes, 0);

        fs::write(root.join("test.txt"), "change 1").unwrap();
        run_git(&["stash", "push", "-m", "first stash"], root);
        let (_, state) = get_git_state(root).unwrap().unwrap();
        assert_eq!(state.stashes, 1);

        fs::write(root.join("test.txt"), "change 2").unwrap();
        run_git(&["stash", "push", "-m", "second stash"], root);
        let (_, state) = get_git_state(root).unwrap().unwrap();
        assert_eq!(state.stashes, 2);

        run_git(&["stash", "pop"], root);
        let (_, state) = get_git_state(root).unwrap().unwrap();
        assert_eq!(state.stashes, 1);
    }

    #[test]
    fn test_git_index_size_and_mixed_staged_unstaged() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        run_git(&["init", "--initial-branch=main"], root);
        run_git(&["config", "user.name", "Test User"], root);
        run_git(&["config", "user.email", "test@example.com"], root);

        fs::write(root.join("a.txt"), "a1").unwrap();
        fs::write(root.join("b.txt"), "b1").unwrap();
        run_git(&["add", "a.txt", "b.txt"], root);
        run_git(&["commit", "-m", "initial"], root);

        let (_, state) = get_git_state(root).unwrap().unwrap();
        assert_eq!(state.index_size, 2);
        assert_eq!(state.num_staged, 0);
        assert_eq!(state.num_staged_new, 0);
        assert_eq!(state.num_staged_deleted, 0);
        assert_eq!(state.num_unstaged, 0);
        assert_eq!(state.num_unstaged_deleted, 0);

        // Stage a brand new file c.txt, then also modify it in worktree (AM)
        fs::write(root.join("c.txt"), "c1").unwrap();
        run_git(&["add", "c.txt"], root);
        fs::write(root.join("c.txt"), "c2").unwrap();

        // Stage a modification to a.txt, then modify it again in worktree (MM)
        fs::write(root.join("a.txt"), "a2").unwrap();
        run_git(&["add", "a.txt"], root);
        fs::write(root.join("a.txt"), "a3").unwrap();

        // Stage a modification to b.txt, then delete it in worktree (MD)
        fs::write(root.join("b.txt"), "b2").unwrap();
        run_git(&["add", "b.txt"], root);
        fs::remove_file(root.join("b.txt")).unwrap();

        let (_, state) = get_git_state(root).unwrap().unwrap();
        assert_eq!(state.index_size, 3);
        assert_eq!(state.num_staged, 3);
        assert_eq!(state.num_staged_new, 1);
        assert_eq!(state.num_staged_deleted, 0);
        assert_eq!(state.num_unstaged, 3);
        assert_eq!(state.num_unstaged_deleted, 1);
    }

    #[test]
    fn test_git_gitignore_and_untracked_dirs() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        run_git(&["init", "--initial-branch=main"], root);
        run_git(&["config", "user.name", "Test User"], root);
        run_git(&["config", "user.email", "test@example.com"], root);

        fs::write(root.join(".gitignore"), "*.log\nbuild/\n").unwrap();
        let tracked_dir = root.join("tracked_dir");
        fs::create_dir_all(&tracked_dir).unwrap();
        fs::write(tracked_dir.join("tracked.txt"), "ok").unwrap();
        run_git(&["add", ".gitignore", "tracked_dir/tracked.txt"], root);
        run_git(&["commit", "-m", "initial"], root);

        // Create ignored file and ignored directory
        fs::write(root.join("debug.log"), "ignored").unwrap();
        fs::create_dir_all(root.join("build")).unwrap();
        fs::write(root.join("build").join("out.bin"), "ignored").unwrap();

        let (_, state) = get_git_state(root).unwrap().unwrap();
        assert_eq!(state.num_untracked, 0);

        // Create an entirely untracked directory with 2 files inside -> should count as 1
        let untracked_dir = root.join("untracked_dir");
        fs::create_dir_all(&untracked_dir).unwrap();
        fs::write(untracked_dir.join("u1.txt"), "1").unwrap();
        fs::write(untracked_dir.join("u2.txt"), "2").unwrap();

        // Create 2 untracked files inside the already-tracked directory -> should count as 2
        fs::write(tracked_dir.join("new1.txt"), "1").unwrap();
        fs::write(tracked_dir.join("new2.txt"), "2").unwrap();

        let (_, state) = get_git_state(root).unwrap().unwrap();
        assert_eq!(state.num_untracked, 3);
    }

    #[test]
    fn test_git_ahead_behind_diverged_and_separate_push_remote() {
        let origin_dir = tempdir().unwrap();
        let origin_root = origin_dir.path();
        run_git(&["init", "--bare", "--initial-branch=main"], origin_root);

        let fork_dir = tempdir().unwrap();
        let fork_root = fork_dir.path();
        run_git(&["init", "--bare", "--initial-branch=main"], fork_root);

        // Seed origin/main from a helper clone
        let seed_dir = tempdir().unwrap();
        let seed_root = seed_dir.path();
        run_git(&["init", "--initial-branch=main"], seed_root);
        run_git(&["config", "user.name", "Seed"], seed_root);
        run_git(&["config", "user.email", "seed@example.com"], seed_root);
        let origin_url = format!("file://{}", origin_root.display());
        run_git(&["remote", "add", "origin", &origin_url], seed_root);

        fs::write(seed_root.join("base.txt"), "base").unwrap();
        run_git(&["add", "base.txt"], seed_root);
        run_git(&["commit", "-m", "base"], seed_root);
        run_git(&["push", "-u", "origin", "main"], seed_root);

        // Set up local repo tracking origin/main from local branch `feature`
        let local_dir = tempdir().unwrap();
        let local_root = local_dir.path();
        run_git(&["init", "--initial-branch=feature"], local_root);
        run_git(&["config", "user.name", "Local"], local_root);
        run_git(&["config", "user.email", "local@example.com"], local_root);
        run_git(&["remote", "add", "origin", &origin_url], local_root);
        let fork_url = format!("file://{}", fork_root.display());
        run_git(&["remote", "add", "fork", &fork_url], local_root);

        run_git(&["fetch", "origin"], local_root);
        run_git(&["reset", "--hard", "origin/main"], local_root);
        run_git(
            &["branch", "--set-upstream-to=origin/main", "feature"],
            local_root,
        );
        run_git(&["config", "branch.feature.pushRemote", "fork"], local_root);

        // Push current state to fork/feature, then add 1 extra commit on fork/feature via seed repo
        run_git(&["push", "fork", "feature:feature"], local_root);
        run_git(&["remote", "add", "fork", &fork_url], seed_root);
        run_git(&["fetch", "fork"], seed_root);
        run_git(&["checkout", "-b", "fork-work", "fork/feature"], seed_root);
        fs::write(seed_root.join("fork_only.txt"), "fork1").unwrap();
        run_git(&["add", "fork_only.txt"], seed_root);
        run_git(&["commit", "-m", "fork commit 1"], seed_root);
        run_git(&["push", "fork", "fork-work:feature"], seed_root);

        // Advance origin/main by 2 commits via seed repo
        run_git(&["checkout", "main"], seed_root);
        fs::write(seed_root.join("origin1.txt"), "o1").unwrap();
        run_git(&["add", "origin1.txt"], seed_root);
        run_git(&["commit", "-m", "origin 1"], seed_root);
        fs::write(seed_root.join("origin2.txt"), "o2").unwrap();
        run_git(&["add", "origin2.txt"], seed_root);
        run_git(&["commit", "-m", "origin 2"], seed_root);
        run_git(&["push", "origin", "main"], seed_root);

        // Fetch both remotes in local_root and make 3 local commits on feature
        run_git(&["fetch", "--all"], local_root);
        for i in 1..=3 {
            fs::write(local_root.join(format!("local{i}.txt")), format!("{i}")).unwrap();
            run_git(&["add", &format!("local{i}.txt")], local_root);
            run_git(&["commit", "-m", &format!("local {i}")], local_root);
        }

        let (_, state) = get_git_state(local_root).unwrap().unwrap();
        assert_eq!(state.local_branch, "feature");
        assert_eq!(state.remote_name, "origin");
        assert_eq!(state.remote_branch, "main");
        assert_eq!(state.remote_url, origin_url);
        assert_eq!(state.commits_ahead, 3);
        assert_eq!(state.commits_behind, 2);
        assert_eq!(state.push_remote_name, "fork");
        assert_eq!(state.push_remote_url, fork_url);
        assert_eq!(state.push_commits_ahead, 3);
        assert_eq!(state.push_commits_behind, 1);
    }

    #[test]
    fn test_git_rebase_apply_action() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        run_git(&["init", "--initial-branch=main"], root);
        let rebase_apply = root.join(".git").join("rebase-apply");
        fs::create_dir(&rebase_apply).unwrap();

        let (_, state) = get_git_state(root).unwrap().unwrap();
        assert_eq!(state.action, "rebase");
    }

    #[test]
    fn test_git_packed_objects_and_refs() {
        let remote_dir = tempdir().unwrap();
        run_git(
            &["init", "--bare", "--initial-branch=main"],
            remote_dir.path(),
        );

        let dir = tempdir().unwrap();
        let root = dir.path();
        run_git(&["init", "--initial-branch=main"], root);
        run_git(&["config", "user.name", "Test User"], root);
        run_git(&["config", "user.email", "test@example.com"], root);

        let remote_url = format!("file://{}", remote_dir.path().display());
        run_git(&["remote", "add", "origin", &remote_url], root);

        fs::write(root.join("file.txt"), "v1").unwrap();
        run_git(&["add", "file.txt"], root);
        run_git(&["commit", "-m", "commit 1"], root);
        run_git(&["push", "-u", "origin", "main"], root);

        fs::write(root.join("file.txt"), "v2").unwrap();
        run_git(&["commit", "-am", "Packed commit summary"], root);
        run_git(&["tag", "-a", "v2.0", "-m", "Release 2.0"], root);

        // Pack all objects and refs
        run_git(&["gc", "--prune=now"], root);
        run_git(&["pack-refs", "--all"], root);

        let (_, state) = get_git_state(root).unwrap().unwrap();
        assert_eq!(state.local_branch, "main");
        assert_eq!(state.remote_name, "origin");
        assert_eq!(state.remote_branch, "main");
        assert_eq!(state.commits_ahead, 1);
        assert_eq!(state.commits_behind, 0);
        assert_eq!(state.tag, "v2.0");
        assert_eq!(state.commit_summary, "Packed commit summary");
        assert_eq!(state.index_size, 1);
    }

    #[test]
    fn test_git_linked_worktree() {
        let dir = tempdir().unwrap();
        let main_root = dir.path().join("main-repo");
        let wt_root = dir.path().join("linked-wt");
        fs::create_dir_all(&main_root).unwrap();

        run_git(&["init", "--initial-branch=main"], &main_root);
        run_git(&["config", "user.name", "Test User"], &main_root);
        run_git(&["config", "user.email", "test@example.com"], &main_root);

        fs::write(main_root.join("shared.txt"), "hello").unwrap();
        run_git(&["add", "shared.txt"], &main_root);
        run_git(&["commit", "-m", "Main commit"], &main_root);

        run_git(
            &[
                "worktree",
                "add",
                "-b",
                "wt-branch",
                wt_root.to_str().unwrap(),
                "main",
            ],
            &main_root,
        );

        fs::write(wt_root.join("untracked.txt"), "new").unwrap();

        let (discovered_root, state) = get_git_state(&wt_root).unwrap().unwrap();
        assert_eq!(
            discovered_root.canonicalize().unwrap(),
            wt_root.canonicalize().unwrap()
        );
        assert_eq!(state.local_branch, "wt-branch");
        assert_eq!(state.commit_summary, "Main commit");
        assert_eq!(state.index_size, 1);
        assert_eq!(state.num_untracked, 1);
    }

    #[test]
    fn test_git_status_typechange() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        run_git(&["init", "--initial-branch=main"], root);
        run_git(&["config", "user.name", "Test User"], root);
        run_git(&["config", "user.email", "test@example.com"], root);

        fs::write(root.join("target.txt"), "target").unwrap();
        fs::write(root.join("link_or_file.txt"), "regular file").unwrap();
        run_git(&["add", "target.txt", "link_or_file.txt"], root);
        run_git(&["commit", "-m", "initial"], root);

        // Replace regular file with a symlink (unstaged typechange)
        fs::remove_file(root.join("link_or_file.txt")).unwrap();
        symlink("target.txt", root.join("link_or_file.txt")).unwrap();

        let (_, state) = get_git_state(root).unwrap().unwrap();
        assert_eq!(state.num_unstaged, 1);
        assert_eq!(state.num_unstaged_deleted, 0);
        assert_eq!(state.num_staged, 0);

        // Stage the typechange
        run_git(&["add", "link_or_file.txt"], root);
        let (_, state) = get_git_state(root).unwrap().unwrap();
        assert_eq!(state.num_staged, 1);
        assert_eq!(state.num_staged_new, 0);
        assert_eq!(state.num_staged_deleted, 0);
        assert_eq!(state.num_unstaged, 0);
    }
}

//! Git worktree operations module.
//!
//! Layout:
//!   - `remote`   — repo cloning, origin-URL parsing
//!   - `worktree` — `GitWorktree` lifecycle, branch ops, template paths
//!   - `diff`     — diff rendering for the UI
//!   - `cleanup`  — stale-worktree cleanup
//!   - `template` — path-template expansion
//!   - this file  — module declarations, re-exports, and the shared
//!     `open_repo_at` helper used by sibling submodules.
//!
//! `remote` and `worktree` were extracted from a single 1,797-line `mod.rs`;
//! `diff`, `cleanup`, and `template` predate the split.

use std::ffi::OsStr;
use std::path::Path;

pub mod cleanup;
pub(crate) mod command;
pub mod diff;
pub mod error;
mod remote;
pub mod template;
mod worktree;

pub use remote::{
    clone_bare_repo, clone_repo, get_remote_owner, get_remote_owner_with_key, get_remote_slug,
    get_remote_url,
};
pub use worktree::{GitWorktree, WorktreeEntry};

/// Open a git repository at the given path without searching parent directories.
/// Unlike `git2::Repository::discover`, this does not walk up the directory tree,
/// preventing unrelated ancestor repos (e.g., a dotfile-managed home directory)
/// from being found.
pub(crate) fn open_repo_at(path: &Path) -> std::result::Result<git2::Repository, git2::Error> {
    git2::Repository::open_ext(
        path,
        git2::RepositoryOpenFlags::NO_SEARCH | git2::RepositoryOpenFlags::FROM_ENV,
        std::iter::empty::<&OsStr>(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::test_support::EnvGuard;
    use std::fs;

    #[test]
    fn open_repo_at_uses_explicit_global_config() {
        let temp = tempfile::tempdir().unwrap();
        let repo_path = temp.path().join("repo");
        git2::Repository::init(&repo_path).unwrap();
        let service_config = temp.path().join("service.gitconfig");
        fs::write(
            &service_config,
            "[maya]\n\tservice-boundary-test = expected\n",
        )
        .unwrap();
        let _env = EnvGuard::set(&[
            ("GIT_CONFIG_GLOBAL", service_config.as_os_str()),
            ("GIT_CONFIG_NOSYSTEM", OsStr::new("1")),
        ]);

        let repo = open_repo_at(&repo_path).unwrap();
        assert_eq!(
            repo.config()
                .unwrap()
                .get_string("maya.service-boundary-test")
                .unwrap(),
            "expected"
        );
    }
}

// SPDX-License-Identifier: MIT
//! Resolve Git paths once from a launch CWD, including a team's nested content folder.
use std::path::{Path, PathBuf};

pub(crate) struct GitWorktree {
    pub root: PathBuf,
    pub git_dir: PathBuf,
    pub prefix: PathBuf,
    pub linked: bool,
}

impl GitWorktree {
    pub fn discover(cwd: &Path) -> Result<Option<Self>, String> {
        let repo = match git2::Repository::discover(cwd) {
            Ok(repo) => repo,
            Err(error) if error.code() == git2::ErrorCode::NotFound => {
                if cwd.ancestors().any(|root| root.join(".git").exists()) {
                    return Err(format!("Git repository could not be opened: {error}"));
                }
                return Ok(None);
            }
            Err(error) => return Err(error.to_string()),
        };
        let root = repo
            .workdir()
            .ok_or("Git repository has no working tree")?
            .to_path_buf();
        let cwd = cwd.canonicalize().map_err(|e| e.to_string())?;
        let root = root.canonicalize().map_err(|e| e.to_string())?;
        let prefix = cwd
            .strip_prefix(&root)
            .map_err(|e| e.to_string())?
            .to_path_buf();
        Ok(Some(Self {
            root,
            git_dir: repo.path().to_path_buf(),
            prefix,
            linked: repo.is_worktree(),
        }))
    }
    pub fn relative(&self, path: &str) -> String {
        self.prefix.join(path).to_string_lossy().replace('\\', "/")
    }
    pub fn nested_linked(&self) -> bool {
        self.linked && !self.prefix.as_os_str().is_empty()
    }
    pub fn exclude(&self) -> PathBuf {
        self.git_dir.join("info/exclude")
    }
}

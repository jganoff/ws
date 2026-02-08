use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::config;
use crate::git;
use crate::giturl::Parsed;

pub fn dir(parsed: &Parsed) -> Result<PathBuf> {
    let mirrors_dir = config::mirrors_dir()?;
    Ok(mirrors_dir.join(parsed.mirror_path()))
}

pub fn clone(parsed: &Parsed, url: &str) -> Result<()> {
    let dest = dir(parsed)?;
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    git::clone_bare(url, &dest)
}

pub fn fetch(parsed: &Parsed) -> Result<()> {
    let d = dir(parsed)?;
    git::fetch(&d)
}

pub fn remove(parsed: &Parsed) -> Result<()> {
    let d = dir(parsed)?;
    fs::remove_dir_all(d)?;
    Ok(())
}

pub fn exists(parsed: &Parsed) -> Result<bool> {
    let d = dir(parsed).context("resolving mirror dir")?;
    Ok(d.exists())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn create_test_repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().to_str().unwrap();
        let cmds: Vec<Vec<&str>> = vec![
            vec!["git", "init", "--initial-branch=main"],
            vec!["git", "config", "user.email", "test@test.com"],
            vec!["git", "config", "user.name", "Test"],
            vec!["git", "config", "commit.gpgsign", "false"],
            vec!["git", "commit", "--allow-empty", "-m", "initial"],
        ];
        for args in cmds {
            let output = Command::new(args[0])
                .args(&args[1..])
                .current_dir(d)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "command {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        tmp
    }

    #[test]
    fn test_clone_and_exists() {
        let tmp_data = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_DATA_HOME", tmp_data.path().to_str().unwrap()) };

        let repo = create_test_repo();
        let parsed = Parsed {
            host: "test.local".into(),
            owner: "user".into(),
            repo: "test-repo".into(),
        };

        clone(&parsed, repo.path().to_str().unwrap()).unwrap();

        assert!(exists(&parsed).unwrap());

        let d = dir(&parsed).unwrap();
        assert!(d.exists());

        unsafe { std::env::remove_var("XDG_DATA_HOME") };
    }

    #[test]
    fn test_fetch() {
        let tmp_data = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_DATA_HOME", tmp_data.path().to_str().unwrap()) };

        let repo = create_test_repo();
        let parsed = Parsed {
            host: "test.local".into(),
            owner: "user".into(),
            repo: "test-repo".into(),
        };

        clone(&parsed, repo.path().to_str().unwrap()).unwrap();
        fetch(&parsed).unwrap();

        unsafe { std::env::remove_var("XDG_DATA_HOME") };
    }

    #[test]
    fn test_remove() {
        let tmp_data = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_DATA_HOME", tmp_data.path().to_str().unwrap()) };

        let repo = create_test_repo();
        let parsed = Parsed {
            host: "test.local".into(),
            owner: "user".into(),
            repo: "test-repo".into(),
        };

        clone(&parsed, repo.path().to_str().unwrap()).unwrap();
        assert!(exists(&parsed).unwrap());

        remove(&parsed).unwrap();
        assert!(!exists(&parsed).unwrap());

        unsafe { std::env::remove_var("XDG_DATA_HOME") };
    }

    #[test]
    fn test_dir() {
        unsafe { std::env::set_var("XDG_DATA_HOME", "/data") };
        let parsed = Parsed {
            host: "github.com".into(),
            owner: "user".into(),
            repo: "repo-a".into(),
        };
        let d = dir(&parsed).unwrap();
        assert_eq!(
            d,
            PathBuf::from("/data/ws/mirrors/github.com/user/repo-a.git")
        );
        unsafe { std::env::remove_var("XDG_DATA_HOME") };
    }
}

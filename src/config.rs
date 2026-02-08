use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoEntry {
    pub url: String,
    pub added: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupEntry {
    pub repos: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub repos: BTreeMap<String, RepoEntry>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub groups: BTreeMap<String, GroupEntry>,
}

impl Config {
    pub fn load() -> Result<Config> {
        let cfg_path = config_path()?;

        if !cfg_path.exists() {
            return Ok(Config::default());
        }

        let data = fs::read_to_string(&cfg_path)?;
        let cfg: Config = serde_yml::from_str(&data)?;
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        let cfg_path = config_path()?;

        if let Some(dir) = cfg_path.parent() {
            fs::create_dir_all(dir)?;
        }

        let data = serde_yml::to_string(self)?;
        fs::write(&cfg_path, data)?;
        Ok(())
    }
}

/// Resolves the ws data directory. Accepts injectable overrides for testing.
pub fn data_dir_with(xdg_data_home: Option<&str>, home: Option<&Path>) -> Result<PathBuf> {
    if let Some(xdg) = xdg_data_home.filter(|s| !s.is_empty()) {
        return Ok(PathBuf::from(xdg).join("ws"));
    }
    let home = home.context("cannot determine home directory")?;
    Ok(home.join(".local").join("share").join("ws"))
}

pub fn data_dir() -> Result<PathBuf> {
    data_dir_with(
        std::env::var("XDG_DATA_HOME").ok().as_deref(),
        dirs::home_dir().as_deref(),
    )
}

pub fn config_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("config.yaml"))
}

pub fn mirrors_dir() -> Result<PathBuf> {
    Ok(data_dir()?.join("mirrors"))
}

/// Resolves the default workspaces directory. Accepts injectable home for testing.
pub fn default_workspaces_dir_with(home: Option<&Path>) -> Result<PathBuf> {
    let home = home.context("cannot determine home directory")?;
    Ok(home.join("dev").join("workspaces"))
}

pub fn default_workspaces_dir() -> Result<PathBuf> {
    default_workspaces_dir_with(dirs::home_dir().as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn test_data_dir_xdg_set() {
        let dir = data_dir_with(Some("/custom/data"), None).unwrap();
        assert_eq!(dir, PathBuf::from("/custom/data/ws"));
    }

    #[test]
    fn test_data_dir_xdg_empty_falls_back_to_home() {
        let dir = data_dir_with(Some(""), Some(Path::new("/home/user"))).unwrap();
        assert_eq!(dir, PathBuf::from("/home/user/.local/share/ws"));
    }

    #[test]
    fn test_data_dir_no_xdg_uses_home() {
        let dir = data_dir_with(None, Some(Path::new("/home/user"))).unwrap();
        assert_eq!(dir, PathBuf::from("/home/user/.local/share/ws"));
    }

    #[test]
    fn test_data_dir_no_home_errors() {
        assert!(data_dir_with(None, None).is_err());
    }

    #[test]
    fn test_config_path() {
        // Uses real env, just verify it ends with the right suffix
        let p = data_dir_with(Some("/custom/data"), None)
            .unwrap()
            .join("config.yaml");
        assert_eq!(p, PathBuf::from("/custom/data/ws/config.yaml"));
    }

    #[test]
    fn test_mirrors_dir() {
        let dir = data_dir_with(Some("/custom/data"), None)
            .unwrap()
            .join("mirrors");
        assert_eq!(dir, PathBuf::from("/custom/data/ws/mirrors"));
    }

    #[test]
    fn test_default_workspaces_dir() {
        let dir = default_workspaces_dir_with(Some(Path::new("/home/user"))).unwrap();
        assert_eq!(dir, PathBuf::from("/home/user/dev/workspaces"));
    }

    #[test]
    fn test_load_save_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_DATA_HOME", tmp.path().to_str().unwrap()) };

        // Load should return empty config when file doesn't exist
        let mut cfg = Config::load().unwrap();
        assert!(cfg.repos.is_empty());
        assert!(cfg.groups.is_empty());

        // Add data
        let now = Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap();
        cfg.repos.insert(
            "github.com/user/repo-a".into(),
            RepoEntry {
                url: "git@github.com:user/repo-a.git".into(),
                added: now,
            },
        );
        cfg.repos.insert(
            "github.com/user/repo-b".into(),
            RepoEntry {
                url: "git@github.com:user/repo-b.git".into(),
                added: now,
            },
        );
        cfg.groups.insert(
            "backend".into(),
            GroupEntry {
                repos: vec![
                    "github.com/user/repo-a".into(),
                    "github.com/user/repo-b".into(),
                ],
            },
        );

        cfg.save().unwrap();

        // Verify file exists
        let cfg_path = tmp.path().join("ws").join("config.yaml");
        assert!(cfg_path.exists());

        // Load again
        let cfg2 = Config::load().unwrap();
        assert_eq!(cfg2.repos.len(), 2);
        assert_eq!(
            cfg2.repos["github.com/user/repo-a"].url,
            "git@github.com:user/repo-a.git"
        );
        assert_eq!(cfg2.repos["github.com/user/repo-a"].added, now);
        assert_eq!(cfg2.groups.len(), 1);
        assert_eq!(
            cfg2.groups["backend"].repos,
            vec!["github.com/user/repo-a", "github.com/user/repo-b"]
        );

        unsafe { std::env::remove_var("XDG_DATA_HOME") };
    }

    #[test]
    fn test_load_nonexistent_file() {
        let tmp = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_DATA_HOME", tmp.path().to_str().unwrap()) };

        let cfg = Config::load().unwrap();
        assert!(cfg.repos.is_empty());
        assert!(cfg.groups.is_empty());

        unsafe { std::env::remove_var("XDG_DATA_HOME") };
    }
}

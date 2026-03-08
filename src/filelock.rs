use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use fs2::FileExt;

use crate::config::Config;
use crate::workspace::{Metadata, load_metadata, save_metadata};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const RETRY_INTERVAL: Duration = Duration::from_millis(50);

/// Advisory file lock using `flock` via the `fs2` crate.
///
/// Acquires an exclusive lock on a `.lock` file adjacent to the target path.
/// Writes the current PID into the lock file for diagnostics.
/// The lock is released when the `FileLock` is dropped (or the process exits).
#[derive(Debug)]
pub struct FileLock {
    _file: File,
    _lock_path: PathBuf,
}

impl FileLock {
    /// Returns the lock file path for a given target path (appends `.lock`).
    pub fn lock_path_for(path: &Path) -> PathBuf {
        let mut p = path.as_os_str().to_os_string();
        p.push(".lock");
        PathBuf::from(p)
    }

    /// Acquire an exclusive advisory lock on the `.lock` file for `path`.
    ///
    /// Retries with a short sleep until `timeout` elapses. On timeout, reads
    /// the PID from the lock file (if any) to include in the error message.
    pub fn acquire(path: &Path, timeout: Duration) -> Result<Self> {
        let lock_path = Self::lock_path_for(path);

        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent).context("creating directory for lock file")?;
        }

        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("opening lock file {}", lock_path.display()))?;

        let start = Instant::now();
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => break,
                Err(_) if start.elapsed() < timeout => {
                    std::thread::sleep(RETRY_INTERVAL);
                }
                Err(_) => {
                    let holder = fs::read_to_string(&lock_path)
                        .ok()
                        .and_then(|s| {
                            let trimmed = s.trim().to_string();
                            if trimmed.is_empty() {
                                None
                            } else {
                                Some(trimmed)
                            }
                        })
                        .unwrap_or_else(|| "unknown".into());
                    bail!(
                        "timed out waiting for lock on {} (held by PID {})",
                        path.display(),
                        holder
                    );
                }
            }
        }

        // Write our PID for diagnostics
        let pid = std::process::id();
        // Truncate and write PID
        file.set_len(0).ok();
        // We need a mutable reference through &File — use std::io::Write on &File
        (&file).write_all(pid.to_string().as_bytes()).ok();

        Ok(FileLock {
            _file: file,
            _lock_path: lock_path,
        })
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        // flock is released when the File is dropped (fd closed).
        // Do NOT remove the lock file: if we unlink it while another process
        // has the same path open (spinning in the retry loop), that process
        // acquires flock on the unlinked inode while a third process creates
        // a new file at the same path and acquires its own flock — both
        // proceed concurrently. Leaving the lock file is standard practice
        // for flock-based locking (it's a few bytes, harmless on disk).
    }
}

/// Acquire an exclusive lock on the config file, load it, call `f` to modify
/// it, save it, then release the lock. Returns the modified config.
pub fn with_config<F>(config_path: &Path, f: F) -> Result<Config>
where
    F: FnOnce(&mut Config) -> Result<()>,
{
    let _lock = FileLock::acquire(config_path, DEFAULT_TIMEOUT)?;
    let mut cfg = Config::load_from(config_path)?;
    f(&mut cfg)?;
    cfg.save_to(config_path)?;
    Ok(cfg)
}

/// Acquire an exclusive lock, load the config, and return a snapshot.
/// Does not write back. Use this when you only need to read the current state
/// under the lock (e.g., for phase 1 of a 3-phase lock pattern).
pub fn read_config(config_path: &Path) -> Result<Config> {
    let _lock = FileLock::acquire(config_path, DEFAULT_TIMEOUT)?;
    Config::load_from(config_path)
}

/// Acquire an exclusive lock on the metadata file, load it, call `f` to modify
/// it, save it, then release the lock. Returns the modified metadata.
pub fn with_metadata<F>(ws_dir: &Path, f: F) -> Result<Metadata>
where
    F: FnOnce(&mut Metadata) -> Result<()>,
{
    let metadata_path = ws_dir.join(".wsp.yaml");
    let _lock = FileLock::acquire(&metadata_path, DEFAULT_TIMEOUT)?;
    let mut meta = load_metadata(ws_dir)?;
    f(&mut meta)?;
    save_metadata(ws_dir, &meta)?;
    Ok(meta)
}

/// Acquire an exclusive lock, load the metadata, and return a snapshot.
/// Does not write back. Use this when you only need to read the current state
/// under the lock (e.g., for phase 1 of a 3-phase lock pattern).
pub fn read_metadata(ws_dir: &Path) -> Result<Metadata> {
    let metadata_path = ws_dir.join(".wsp.yaml");
    let _lock = FileLock::acquire(&metadata_path, DEFAULT_TIMEOUT)?;
    load_metadata(ws_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::WorkspaceRepoRef;
    use chrono::Utc;
    use std::collections::BTreeMap;

    #[test]
    fn lock_path_appends_dot_lock() {
        let cases = vec![
            ("/tmp/config.yaml", "/tmp/config.yaml.lock"),
            ("/a/b/.wsp.yaml", "/a/b/.wsp.yaml.lock"),
        ];
        for (input, want) in cases {
            assert_eq!(
                FileLock::lock_path_for(Path::new(input)),
                PathBuf::from(want),
                "lock_path_for({:?})",
                input
            );
        }
    }

    #[test]
    fn acquire_writes_pid() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("test.yaml");
        fs::write(&target, "").unwrap();

        let lock = FileLock::acquire(&target, Duration::from_secs(1)).unwrap();
        let lock_path = FileLock::lock_path_for(&target);
        let contents = fs::read_to_string(&lock_path).unwrap();
        let pid: u32 = contents.trim().parse().unwrap();
        assert_eq!(pid, std::process::id());
        drop(lock);
    }

    #[test]
    fn acquire_timeout_on_held_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("test.yaml");
        fs::write(&target, "").unwrap();

        let _lock = FileLock::acquire(&target, Duration::from_secs(5)).unwrap();

        // Same process, try to acquire again with short timeout
        let result = FileLock::acquire(&target, Duration::from_millis(100));
        assert!(result.is_err(), "expected timeout error");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("timed out"),
            "error should mention timeout: {}",
            err
        );
    }

    #[test]
    fn with_config_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("config.yaml");

        // Create initial config
        let cfg = Config::default();
        cfg.save_to(&cfg_path).unwrap();

        // Modify via with_config
        let result = with_config(&cfg_path, |cfg| {
            cfg.branch_prefix = Some("feat/".into());
            Ok(())
        })
        .unwrap();

        assert_eq!(result.branch_prefix.as_deref(), Some("feat/"));

        // Verify persisted
        let loaded = Config::load_from(&cfg_path).unwrap();
        assert_eq!(loaded.branch_prefix.as_deref(), Some("feat/"));
    }

    #[test]
    fn with_metadata_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let ws_dir = tmp.path();

        // Create initial metadata
        let meta = Metadata {
            version: 0,
            name: "test-ws".into(),
            branch: "test-branch".into(),
            repos: BTreeMap::new(),
            created: Utc::now(),
            description: None,
            last_used: None,
            created_from: None,
            dirs: BTreeMap::new(),
        };
        save_metadata(ws_dir, &meta).unwrap();

        // Modify via with_metadata
        let result = with_metadata(ws_dir, |m| {
            m.repos.insert(
                "github.com/user/repo".into(),
                Some(WorkspaceRepoRef {
                    r#ref: "v1.0".into(),
                    url: None,
                }),
            );
            Ok(())
        })
        .unwrap();

        assert!(result.repos.contains_key("github.com/user/repo"));

        // Verify persisted
        let loaded = load_metadata(ws_dir).unwrap();
        assert!(loaded.repos.contains_key("github.com/user/repo"));
    }
}

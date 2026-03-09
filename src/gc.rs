use std::fs;
use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::Paths;

// EXDEV: cross-device link (errno 18 on macOS and Linux)
fn is_cross_device(e: &std::io::Error) -> bool {
    e.raw_os_error() == Some(18)
}

const GC_META_FILE: &str = ".wsp-gc.yaml";
const DEFAULT_RETENTION_DAYS: u32 = 7;
const GC_COOLDOWN_SECS: u64 = 3600; // 1 hour between auto-gc runs

/// Metadata stored inside each gc'd workspace directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GcEntry {
    pub name: String,
    pub branch: String,
    pub trashed_at: DateTime<Utc>,
    pub original_path: String,
}

/// Move a directory, falling back to recursive copy + delete if rename
/// fails with EXDEV (cross-filesystem). An incomplete copy is cleaned up
/// on failure so the gc area doesn't accumulate garbage.
fn move_dir(src: &Path, dest: &Path) -> Result<()> {
    match fs::rename(src, dest) {
        Ok(()) => Ok(()),
        Err(e) if is_cross_device(&e) => {
            copy_dir_recursive(src, dest).inspect_err(|_| {
                // Clean up partial copy before propagating the error
                let _ = fs::remove_dir_all(dest);
            })?;
            fs::remove_dir_all(src)?;
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

fn copy_dir_recursive(src: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest)?;
    for item in fs::read_dir(src)? {
        let item = item?;
        let ft = item.file_type()?;
        let src_path = item.path();
        let dest_path = dest.join(item.file_name());
        if ft.is_symlink() {
            let target = fs::read_link(&src_path)?;
            std::os::unix::fs::symlink(&target, &dest_path)?;
        } else if ft.is_dir() {
            copy_dir_recursive(&src_path, &dest_path)?;
        } else {
            fs::copy(&src_path, &dest_path)?;
        }
    }
    Ok(())
}

/// Load gc metadata from a workspace directory, if present.
/// Returns `Some(GcEntry)` when the workspace has been garbage-collected.
pub fn load_entry(ws_dir: &Path) -> Option<GcEntry> {
    let meta_path = ws_dir.join(GC_META_FILE);
    let data = fs::read_to_string(&meta_path).ok()?;
    serde_yaml_ng::from_str(&data).ok()
}

/// Check whether a workspace directory is gc'd and handle accordingly.
///
/// - `read_only = true`: prints a warning to stderr, returns `Ok(())`
/// - `read_only = false`: returns an error (blocks mutating commands)
pub fn check_workspace(ws_dir: &Path, read_only: bool) -> Result<()> {
    if let Some(entry) = load_entry(ws_dir) {
        let date = entry.trashed_at.format("%Y-%m-%d %H:%M UTC");
        if read_only {
            eprintln!(
                "warning: this workspace was removed on {}. Use `wsp recover {}` to restore it.",
                date, entry.name
            );
            Ok(())
        } else {
            anyhow::bail!(
                "this workspace was removed on {}. Use `wsp recover {}` to restore it.",
                date,
                entry.name
            );
        }
    } else {
        Ok(())
    }
}

/// Move a workspace directory to the gc area for deferred deletion.
///
/// Writes metadata inside the workspace dir first, then moves the whole
/// directory. Uses rename when possible, falls back to copy+delete for
/// cross-filesystem moves.
pub fn move_to_gc(paths: &Paths, name: &str, branch: &str) -> Result<()> {
    let ws_dir = crate::workspace::dir(&paths.workspaces_dir, name);
    let timestamp = Utc::now().format("%Y%m%dT%H%M%S%.3f").to_string();
    let gc_name = format!("{}__{}", name, timestamp);
    let dest = paths.gc_dir.join(&gc_name);

    fs::create_dir_all(&paths.gc_dir)?;

    let entry = GcEntry {
        name: name.to_string(),
        branch: branch.to_string(),
        trashed_at: Utc::now(),
        original_path: ws_dir.display().to_string(),
    };
    let yaml = serde_yaml_ng::to_string(&entry)?;
    fs::write(ws_dir.join(GC_META_FILE), yaml)?;

    move_dir(&ws_dir, &dest)?;
    Ok(())
}

/// List all recoverable workspaces in the gc area.
pub fn list(gc_dir: &Path) -> Result<Vec<GcEntry>> {
    if !gc_dir.exists() {
        return Ok(vec![]);
    }

    let mut entries = Vec::new();
    for item in fs::read_dir(gc_dir)? {
        let item = item?;
        let path = item.path();
        if !path.is_dir() {
            continue;
        }
        let meta_path = path.join(GC_META_FILE);
        if let Ok(data) = fs::read_to_string(&meta_path)
            && let Ok(entry) = serde_yaml_ng::from_str::<GcEntry>(&data)
        {
            entries.push(entry);
        }
    }

    entries.sort_by(|a, b| b.trashed_at.cmp(&a.trashed_at));
    Ok(entries)
}

/// Restore a workspace from the gc area back to the workspaces directory.
pub fn restore(paths: &Paths, name: &str) -> Result<()> {
    let entries = find_entries(&paths.gc_dir, name)?;
    if entries.is_empty() {
        anyhow::bail!("no recoverable workspace named {:?}", name);
    }

    // Use the most recent entry
    let (gc_name, entry) = &entries[0];

    // Validate the deserialized name to prevent path traversal from tampered metadata
    crate::workspace::validate_name(&entry.name)?;

    let dest = crate::workspace::dir(&paths.workspaces_dir, &entry.name);
    // fs::rename on Unix fails atomically if dest is a non-empty directory,
    // so this check is a courtesy error message, not a security gate.
    if dest.exists() {
        anyhow::bail!(
            "workspace {:?} already exists; remove or rename it first",
            entry.name
        );
    }

    let src = paths.gc_dir.join(gc_name);
    move_dir(&src, &dest)?;

    // Clean up gc metadata from the restored workspace
    let _ = fs::remove_file(dest.join(GC_META_FILE));

    Ok(())
}

/// Purge gc entries older than the retention period.
pub fn purge(gc_dir: &Path, retention_days: u32) -> Result<u32> {
    if !gc_dir.exists() {
        return Ok(0);
    }

    let cutoff = Utc::now() - chrono::Duration::days(retention_days as i64);
    let mut removed = 0;

    for item in fs::read_dir(gc_dir)? {
        let item = item?;
        let path = item.path();
        if !path.is_dir() {
            continue;
        }

        let meta_path = path.join(GC_META_FILE);
        let data = match fs::read_to_string(&meta_path) {
            Ok(d) => d,
            Err(_) => continue,
        };
        let entry = match serde_yaml_ng::from_str::<GcEntry>(&data) {
            Ok(e) => e,
            Err(_) => continue,
        };

        if entry.trashed_at < cutoff {
            // Best-effort: continue purging others if one fails
            if let Err(e) = fs::remove_dir_all(&path) {
                eprintln!("  warning: gc purge failed for {}: {}", entry.name, e);
            } else {
                removed += 1;
            }
        }
    }

    Ok(removed)
}

/// Run gc if enough time has passed since the last run.
/// Called opportunistically from hot paths (new, rm, sync, ls).
pub fn maybe_run(paths: &Paths, retention_days: Option<u32>) {
    let marker = paths.gc_dir.join(".gc-last");

    // Skip if gc dir doesn't exist (nothing to gc)
    if !paths.gc_dir.exists() {
        return;
    }

    // Skip if we ran recently
    if let Ok(meta) = fs::metadata(&marker)
        && let Ok(modified) = meta.modified()
        && modified.elapsed().unwrap_or(Duration::ZERO) < Duration::from_secs(GC_COOLDOWN_SECS)
    {
        return;
    }

    let days = retention_days.unwrap_or(DEFAULT_RETENTION_DAYS);
    if let Err(e) = purge(&paths.gc_dir, days) {
        eprintln!("  warning: gc failed: {}", e);
    }

    // Touch the marker file
    let _ = fs::write(&marker, "");
}

/// Find gc entries matching a workspace name, most recent first.
fn find_entries(gc_dir: &Path, name: &str) -> Result<Vec<(String, GcEntry)>> {
    if !gc_dir.exists() {
        return Ok(vec![]);
    }

    let mut matches = Vec::new();
    for item in fs::read_dir(gc_dir)? {
        let item = item?;
        let path = item.path();
        if !path.is_dir() {
            continue;
        }

        let meta_path = path.join(GC_META_FILE);
        let data = match fs::read_to_string(&meta_path) {
            Ok(d) => d,
            Err(_) => continue,
        };
        if let Ok(entry) = serde_yaml_ng::from_str::<GcEntry>(&data)
            && entry.name == name
        {
            let gc_name = path.file_name().unwrap().to_string_lossy().to_string();
            matches.push((gc_name, entry));
        }
    }

    matches.sort_by(|a, b| b.1.trashed_at.cmp(&a.1.trashed_at));
    Ok(matches)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Paths;

    fn test_paths(tmp: &Path) -> Paths {
        Paths {
            config_path: tmp.join("config.yaml"),
            mirrors_dir: tmp.join("mirrors"),
            gc_dir: tmp.join("gc"),
            templates_dir: tmp.join("templates"),
            workspaces_dir: tmp.join("workspaces"),
        }
    }

    fn create_workspace(paths: &Paths, name: &str) {
        let ws_dir = paths.workspaces_dir.join(name);
        fs::create_dir_all(&ws_dir).unwrap();
        let meta = crate::workspace::Metadata {
            version: 0,
            name: name.to_string(),
            branch: format!("test/{}", name),
            repos: std::collections::BTreeMap::new(),
            created: Utc::now(),
            description: None,
            last_used: None,
            created_from: None,
            dirs: std::collections::BTreeMap::new(),
        };
        let yaml = serde_yaml_ng::to_string(&meta).unwrap();
        fs::write(ws_dir.join(".wsp.yaml"), yaml).unwrap();
    }

    #[test]
    fn test_move_and_restore() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        create_workspace(&paths, "my-feature");

        assert!(paths.workspaces_dir.join("my-feature").exists());

        move_to_gc(&paths, "my-feature", "test/my-feature").unwrap();
        assert!(!paths.workspaces_dir.join("my-feature").exists());

        let entries = list(&paths.gc_dir).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "my-feature");
        assert_eq!(entries[0].branch, "test/my-feature");

        restore(&paths, "my-feature").unwrap();
        assert!(paths.workspaces_dir.join("my-feature").exists());
        // gc metadata should be cleaned up after restore
        assert!(
            !paths
                .workspaces_dir
                .join("my-feature")
                .join(GC_META_FILE)
                .exists()
        );

        let entries = list(&paths.gc_dir).unwrap();
        assert_eq!(entries.len(), 0);
    }

    #[test]
    fn test_purge_expired() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        create_workspace(&paths, "old-ws");

        move_to_gc(&paths, "old-ws", "test/old-ws").unwrap();

        // Backdate the entry to 10 days ago
        backdate_gc_entries(&paths.gc_dir, 10);

        let removed = purge(&paths.gc_dir, 7).unwrap();
        assert_eq!(removed, 1);

        let entries = list(&paths.gc_dir).unwrap();
        assert_eq!(entries.len(), 0);
    }

    #[test]
    fn test_purge_keeps_recent() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        create_workspace(&paths, "new-ws");

        move_to_gc(&paths, "new-ws", "test/new-ws").unwrap();

        let removed = purge(&paths.gc_dir, 7).unwrap();
        assert_eq!(removed, 0);

        let entries = list(&paths.gc_dir).unwrap();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn test_restore_conflict() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        create_workspace(&paths, "conflict");

        move_to_gc(&paths, "conflict", "test/conflict").unwrap();
        create_workspace(&paths, "conflict"); // recreate

        let err = restore(&paths, "conflict").unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn test_restore_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());

        let err = restore(&paths, "nonexistent").unwrap_err();
        assert!(err.to_string().contains("no recoverable workspace"));
    }

    #[test]
    fn test_maybe_run_cooldown() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        fs::create_dir_all(&paths.gc_dir).unwrap();

        // First run should touch the marker
        maybe_run(&paths, Some(7));
        assert!(paths.gc_dir.join(".gc-last").exists());

        // Create and gc an entry, then backdate it
        create_workspace(&paths, "ws1");
        move_to_gc(&paths, "ws1", "test/ws1").unwrap();
        backdate_gc_entries(&paths.gc_dir, 10);

        // Second run within cooldown should skip gc
        maybe_run(&paths, Some(7));
        assert_eq!(
            list(&paths.gc_dir).unwrap().len(),
            1,
            "gc should be skipped within cooldown"
        );
    }

    #[test]
    fn test_soft_delete_round_trip() {
        // Exercise workspace::remove with permanent=false
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        create_workspace(&paths, "soft-del");

        // remove with permanent=false should move to gc
        crate::workspace::remove(&paths, "soft-del", true, false).unwrap();
        assert!(!paths.workspaces_dir.join("soft-del").exists());

        let entries = list(&paths.gc_dir).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "soft-del");

        // restore should bring it back
        restore(&paths, "soft-del").unwrap();
        assert!(paths.workspaces_dir.join("soft-del").exists());
    }

    #[test]
    fn test_load_entry_present() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        create_workspace(&paths, "gc-test");

        move_to_gc(&paths, "gc-test", "test/gc-test").unwrap();

        // Find the gc'd directory
        let gc_dirs: Vec<_> = fs::read_dir(&paths.gc_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .collect();
        assert_eq!(gc_dirs.len(), 1);

        let entry = load_entry(&gc_dirs[0].path());
        assert!(entry.is_some());
        let entry = entry.unwrap();
        assert_eq!(entry.name, "gc-test");
        assert_eq!(entry.branch, "test/gc-test");
    }

    #[test]
    fn test_load_entry_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        create_workspace(&paths, "normal-ws");

        let ws_dir = paths.workspaces_dir.join("normal-ws");
        assert!(load_entry(&ws_dir).is_none());
    }

    #[test]
    fn test_check_workspace_read_only_warns() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        create_workspace(&paths, "warn-test");
        move_to_gc(&paths, "warn-test", "test/warn-test").unwrap();

        let gc_dirs: Vec<_> = fs::read_dir(&paths.gc_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .collect();

        // Read-only check should succeed (warn only)
        assert!(check_workspace(&gc_dirs[0].path(), true).is_ok());
    }

    #[test]
    fn test_check_workspace_mutating_blocks() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        create_workspace(&paths, "block-test");
        move_to_gc(&paths, "block-test", "test/block-test").unwrap();

        let gc_dirs: Vec<_> = fs::read_dir(&paths.gc_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .collect();

        // Mutating check should fail
        let err = check_workspace(&gc_dirs[0].path(), false).unwrap_err();
        assert!(err.to_string().contains("was removed on"));
        assert!(err.to_string().contains("wsp recover"));
    }

    #[test]
    fn test_check_workspace_normal_passes() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        create_workspace(&paths, "normal");

        let ws_dir = paths.workspaces_dir.join("normal");
        assert!(check_workspace(&ws_dir, true).is_ok());
        assert!(check_workspace(&ws_dir, false).is_ok());
    }

    #[test]
    fn test_copy_dir_recursive_preserves_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let dest = tmp.path().join("dest");
        fs::create_dir_all(&src).unwrap();

        // Regular file
        fs::write(src.join("file.txt"), "hello").unwrap();

        // Relative symlink
        std::os::unix::fs::symlink("file.txt", src.join("link.txt")).unwrap();

        // Subdirectory with a file
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("sub/nested.txt"), "nested").unwrap();

        copy_dir_recursive(&src, &dest).unwrap();

        // Regular file copied
        assert_eq!(fs::read_to_string(dest.join("file.txt")).unwrap(), "hello");

        // Symlink preserved (not resolved to regular file)
        let link_meta = dest.join("link.txt").symlink_metadata().unwrap();
        assert!(link_meta.file_type().is_symlink());
        assert_eq!(fs::read_link(dest.join("link.txt")).unwrap().to_str().unwrap(), "file.txt");

        // Subdirectory recursed
        assert_eq!(
            fs::read_to_string(dest.join("sub/nested.txt")).unwrap(),
            "nested"
        );
    }

    /// Backdate all gc entries by the given number of days.
    fn backdate_gc_entries(gc_dir: &Path, days: i64) {
        for item in fs::read_dir(gc_dir).unwrap() {
            let path = item.unwrap().path();
            if !path.is_dir() {
                continue;
            }
            let meta_path = path.join(GC_META_FILE);
            if let Ok(data) = fs::read_to_string(&meta_path) {
                let mut entry: GcEntry = serde_yaml_ng::from_str(&data).unwrap();
                entry.trashed_at = Utc::now() - chrono::Duration::days(days);
                fs::write(&meta_path, serde_yaml_ng::to_string(&entry).unwrap()).unwrap();
            }
        }
    }
}

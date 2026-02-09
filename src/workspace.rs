use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config;
use crate::git;
use crate::giturl;
use crate::mirror;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceRepoRef {
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub r#ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metadata {
    pub name: String,
    pub branch: String,
    pub repos: BTreeMap<String, Option<WorkspaceRepoRef>>,
    pub created: DateTime<Utc>,
}

pub const METADATA_FILE: &str = ".ws.yaml";

pub fn dir(name: &str) -> Result<PathBuf> {
    let ws_root = config::default_workspaces_dir()?;
    Ok(ws_root.join(name))
}

pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("workspace name cannot be empty");
    }
    if name.contains('/') || name.contains('\\') {
        bail!("workspace name {:?} cannot contain path separators", name);
    }
    if name == "." || name == ".." {
        bail!("workspace name {:?} is not allowed", name);
    }
    Ok(())
}

pub fn load_metadata(ws_dir: &Path) -> Result<Metadata> {
    let data = fs::read_to_string(ws_dir.join(METADATA_FILE))?;
    let m: Metadata = serde_yml::from_str(&data)?;
    Ok(m)
}

pub fn save_metadata(ws_dir: &Path, m: &Metadata) -> Result<()> {
    let data = serde_yml::to_string(m)?;
    fs::write(ws_dir.join(METADATA_FILE), data)?;
    Ok(())
}

pub fn detect(start_dir: &Path) -> Result<PathBuf> {
    let mut dir = start_dir.to_path_buf();
    loop {
        if dir.join(METADATA_FILE).exists() {
            return Ok(dir);
        }
        match dir.parent() {
            Some(parent) if parent != dir => {
                dir = parent.to_path_buf();
            }
            _ => bail!("not in a workspace (no {} found)", METADATA_FILE),
        }
    }
}

pub fn create(name: &str, repo_refs: &BTreeMap<String, String>) -> Result<()> {
    validate_name(name)?;

    let ws_dir = dir(name)?;
    if ws_dir.exists() {
        bail!("workspace {:?} already exists", name);
    }

    fs::create_dir_all(&ws_dir)?;

    match create_inner(name, &ws_dir, repo_refs) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Clean up workspace dir on failure (best-effort)
            let _ = fs::remove_dir_all(&ws_dir);
            Err(e)
        }
    }
}

fn create_inner(name: &str, ws_dir: &Path, repo_refs: &BTreeMap<String, String>) -> Result<()> {
    let mut repos: BTreeMap<String, Option<WorkspaceRepoRef>> = BTreeMap::new();
    for (identity, r) in repo_refs {
        if r.is_empty() {
            repos.insert(identity.clone(), None);
        } else {
            repos.insert(
                identity.clone(),
                Some(WorkspaceRepoRef { r#ref: r.clone() }),
            );
        }
    }

    let meta = Metadata {
        name: name.to_string(),
        branch: name.to_string(),
        repos,
        created: Utc::now(),
    };

    for (identity, r) in repo_refs {
        add_worktree(ws_dir, identity, name, r)
            .map_err(|e| anyhow::anyhow!("adding worktree for {}: {}", identity, e))?;
    }

    save_metadata(ws_dir, &meta)?;
    Ok(())
}

pub fn add_repos(ws_dir: &Path, repo_refs: &BTreeMap<String, String>) -> Result<()> {
    let mut meta = load_metadata(ws_dir)?;

    for (identity, r) in repo_refs {
        if meta.repos.contains_key(identity) {
            println!("  {} already in workspace, skipping", identity);
            continue;
        }
        add_worktree(ws_dir, identity, &meta.branch, r)
            .map_err(|e| anyhow::anyhow!("adding worktree for {}: {}", identity, e))?;
        if r.is_empty() {
            meta.repos.insert(identity.clone(), None);
        } else {
            meta.repos.insert(
                identity.clone(),
                Some(WorkspaceRepoRef { r#ref: r.clone() }),
            );
        }
    }

    save_metadata(ws_dir, &meta)
}

pub fn has_pending_changes(ws_dir: &Path) -> Result<Vec<String>> {
    let meta = load_metadata(ws_dir)?;
    let mut dirty = Vec::new();

    for identity in meta.repos.keys() {
        let parsed = match parse_identity(identity) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let repo_dir = ws_dir.join(&parsed.repo);

        let changed = git::changed_file_count(&repo_dir).unwrap_or(0);
        let ahead = git::ahead_count(&repo_dir).unwrap_or(0);

        if changed > 0 || ahead > 0 {
            dirty.push(parsed.repo);
        }
    }

    Ok(dirty)
}

pub fn remove(name: &str, force: bool) -> Result<()> {
    let ws_dir = dir(name)?;
    let meta =
        load_metadata(&ws_dir).map_err(|e| anyhow::anyhow!("reading workspace metadata: {}", e))?;

    // Collect active repos (no fixed ref) that need branch cleanup
    struct ActiveRepo {
        identity: String,
        parsed: giturl::Parsed,
        mirror_dir: std::path::PathBuf,
        fetch_failed: bool,
    }

    let mut active_repos: Vec<ActiveRepo> = Vec::new();
    let mut context_repos: Vec<(giturl::Parsed, std::path::PathBuf)> = Vec::new();

    for (identity, entry) in &meta.repos {
        let parsed = match parse_identity(identity) {
            Ok(p) => p,
            Err(_) => {
                println!(
                    "  warning: cannot parse {}, skipping worktree cleanup",
                    identity
                );
                continue;
            }
        };
        let mirror_dir = match mirror::dir(&parsed) {
            Ok(d) => d,
            Err(e) => {
                println!("  warning: cannot resolve mirror for {}: {}", identity, e);
                continue;
            }
        };

        let is_active = match entry {
            None => true,
            Some(re) => re.r#ref.is_empty(),
        };

        if is_active {
            // Best-effort fetch to detect remote merges (e.g. PR merged on GitHub)
            let fetch_failed = git::fetch(&mirror_dir).is_err();
            if fetch_failed {
                println!("  warning: fetch failed for {}, using local data", identity);
            }
            active_repos.push(ActiveRepo {
                identity: identity.clone(),
                parsed,
                mirror_dir,
                fetch_failed,
            });
        } else {
            context_repos.push((parsed, mirror_dir));
        }
    }

    // Pre-flight: check if all active branches are merged
    if !force {
        let mut unmerged: Vec<(String, bool)> = Vec::new();
        for ar in &active_repos {
            if !git::branch_exists(&ar.mirror_dir, &meta.branch) {
                continue; // branch already gone, nothing to check
            }
            let default_branch = match git::default_branch(&ar.mirror_dir) {
                Ok(b) => b,
                Err(e) => {
                    println!(
                        "  warning: cannot detect default branch for {}: {}",
                        ar.identity, e
                    );
                    continue;
                }
            };
            let merged =
                git::branch_is_merged(&ar.mirror_dir, &meta.branch, &default_branch)
                    .unwrap_or(false);
            if !merged {
                unmerged.push((ar.identity.clone(), ar.fetch_failed));
            }
        }

        if !unmerged.is_empty() {
            let mut list = String::new();
            let mut any_fetch_failed = false;
            for (repo, fetch_failed) in &unmerged {
                list.push_str(&format!("\n  - {}", repo));
                if *fetch_failed {
                    list.push_str(" (fetch failed, local data may be stale)");
                    any_fetch_failed = true;
                }
            }
            let mut msg = format!(
                "workspace {:?} has unmerged branches ({}):{}\n\nUse --force to remove anyway",
                name, meta.branch, list
            );
            if any_fetch_failed {
                msg.push_str(
                    "\n\nNote: some fetches failed; the branch may already be merged remotely",
                );
            }
            bail!("{}", msg);
        }
    }

    // Pass 2: actual removal
    // Remove worktrees for all repos
    for ar in &active_repos {
        let worktree_path = ws_dir.join(&ar.parsed.repo);
        if let Err(e) = git::worktree_remove(&ar.mirror_dir, &worktree_path) {
            println!("  warning: removing worktree for {}: {}", ar.identity, e);
        }
    }
    for (parsed, mirror_dir) in &context_repos {
        let worktree_path = ws_dir.join(&parsed.repo);
        if let Err(e) = git::worktree_remove(mirror_dir, &worktree_path) {
            println!("  warning: removing worktree for {}: {}", parsed.repo, e);
        }
    }

    // Delete branches from active repos
    for ar in &active_repos {
        if !git::branch_exists(&ar.mirror_dir, &meta.branch) {
            continue;
        }
        if let Err(e) = git::branch_delete(&ar.mirror_dir, &meta.branch) {
            println!(
                "  warning: deleting branch {} in {}: {}",
                meta.branch, ar.identity, e
            );
        }
    }

    fs::remove_dir_all(&ws_dir)?;
    Ok(())
}

pub fn list_all() -> Result<Vec<String>> {
    let ws_root = config::default_workspaces_dir()?;
    if !ws_root.exists() {
        return Ok(Vec::new());
    }

    let mut names = Vec::new();
    for entry in fs::read_dir(&ws_root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let meta_path = entry.path().join(METADATA_FILE);
        if meta_path.exists()
            && let Some(name) = entry.file_name().to_str()
        {
            names.push(name.to_string());
        }
    }
    names.sort();
    Ok(names)
}

fn add_worktree(ws_dir: &Path, identity: &str, branch: &str, git_ref: &str) -> Result<()> {
    let parsed = parse_identity(identity)?;
    let mirror_dir = mirror::dir(&parsed)?;
    let worktree_path = ws_dir.join(&parsed.repo);

    // Context repo: check out at the specified ref
    if !git_ref.is_empty() {
        if git::branch_exists(&mirror_dir, git_ref) {
            return git::worktree_add_existing(&mirror_dir, &worktree_path, git_ref);
        }
        let remote_ref = format!("refs/remotes/origin/{}", git_ref);
        if git::ref_exists(&mirror_dir, &remote_ref) {
            let origin_ref = format!("origin/{}", git_ref);
            return git::worktree_add_existing(&mirror_dir, &worktree_path, &origin_ref);
        }
        // Tag or SHA: detached HEAD
        return git::worktree_add_detached(&mirror_dir, &worktree_path, git_ref);
    }

    // Active repo: create/checkout workspace branch
    if git::branch_exists(&mirror_dir, branch) {
        return git::worktree_add_existing(&mirror_dir, &worktree_path, branch);
    }

    let default_branch = git::default_branch(&mirror_dir)?;

    // In bare clones, branches are at refs/heads/<name>, not refs/remotes/origin/<name>.
    // Try origin/<branch> first; fall back to just <branch> for bare clones.
    let start_point_candidate = format!("origin/{}", default_branch);
    let start_point = if git::ref_exists(&mirror_dir, &start_point_candidate) {
        start_point_candidate
    } else {
        default_branch
    };

    git::worktree_add(&mirror_dir, &worktree_path, branch, &start_point)
}

fn parse_identity(identity: &str) -> Result<giturl::Parsed> {
    giturl::Parsed::from_identity(identity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// Sets up a test environment: temp XDG_DATA_HOME and HOME, creates a source repo,
    /// bare-clones it as a mirror, and sets HEAD ref. Returns TempDirs (keep alive!) and identity.
    fn setup_test_env() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        tempfile::TempDir,
        String,
    ) {
        let tmp_data = tempfile::tempdir().unwrap();
        let tmp_home = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("XDG_DATA_HOME", tmp_data.path().to_str().unwrap());
            std::env::set_var("HOME", tmp_home.path().to_str().unwrap());
        }

        // Create workspaces dir
        let ws_root = tmp_home.path().join("dev").join("workspaces");
        fs::create_dir_all(&ws_root).unwrap();

        // Create a source repo
        let repo_dir = tempfile::tempdir().unwrap();
        let cmds: Vec<Vec<&str>> = vec![
            vec!["git", "init", "--initial-branch=main"],
            vec!["git", "config", "user.email", "test@test.com"],
            vec!["git", "config", "user.name", "Test"],
            vec!["git", "config", "commit.gpgsign", "false"],
            vec!["git", "commit", "--allow-empty", "-m", "initial"],
        ];
        for args in &cmds {
            let output = Command::new(args[0])
                .args(&args[1..])
                .current_dir(repo_dir.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "command {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        }

        // Bare clone into mirrors
        let parsed = giturl::Parsed {
            host: "test.local".into(),
            owner: "user".into(),
            repo: "test-repo".into(),
        };
        crate::mirror::clone(&parsed, repo_dir.path().to_str().unwrap()).unwrap();

        // Set up HEAD ref so DefaultBranch works
        let mirror_dir = crate::mirror::dir(&parsed).unwrap();
        let output = Command::new("git")
            .args([
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/heads/main",
            ])
            .current_dir(&mirror_dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "setting HEAD ref: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        (tmp_data, tmp_home, repo_dir, parsed.identity())
    }

    fn cleanup_env() {
        unsafe {
            std::env::remove_var("XDG_DATA_HOME");
            std::env::remove_var("HOME");
        }
    }

    #[test]
    fn test_create_and_load_metadata() {
        let (_d, _h, _r, identity) = setup_test_env();

        let refs = BTreeMap::from([(identity.clone(), String::new())]);
        create("test-ws", &refs).unwrap();

        let ws_dir = dir("test-ws").unwrap();
        let meta = load_metadata(&ws_dir).unwrap();

        assert_eq!(meta.name, "test-ws");
        assert_eq!(meta.branch, "test-ws");
        assert!(meta.repos.contains_key(&identity));

        // Worktree directory should exist
        assert!(ws_dir.join("test-repo").exists());

        cleanup_env();
    }

    #[test]
    fn test_create_duplicate() {
        let (_d, _h, _r, identity) = setup_test_env();

        let refs = BTreeMap::from([(identity.clone(), String::new())]);
        create("test-ws-dup", &refs).unwrap();
        assert!(create("test-ws-dup", &refs).is_err());

        cleanup_env();
    }

    #[test]
    fn test_detect() {
        let (_d, _h, _r, identity) = setup_test_env();

        let refs = BTreeMap::from([(identity, String::new())]);
        create("test-ws-detect", &refs).unwrap();

        let ws_dir = dir("test-ws-detect").unwrap();

        // From workspace root
        let found = detect(&ws_dir).unwrap();
        assert_eq!(found, ws_dir);

        // From a repo subdirectory
        let repo_dir = ws_dir.join("test-repo");
        let found = detect(&repo_dir).unwrap();
        assert_eq!(found, ws_dir);

        cleanup_env();
    }

    #[test]
    fn test_detect_not_in_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(detect(tmp.path()).is_err());
    }

    #[test]
    fn test_remove_deletes_merged_branch() {
        let (_d, _h, _r, identity) = setup_test_env();

        let refs = BTreeMap::from([(identity.clone(), String::new())]);
        create("rm-merged", &refs).unwrap();

        let ws_dir = dir("rm-merged").unwrap();
        assert!(ws_dir.exists());

        // Branch was created from main with no extra commits, so it's merged
        let parsed = parse_identity(&identity).unwrap();
        let mirror_dir = crate::mirror::dir(&parsed).unwrap();
        assert!(git::branch_exists(&mirror_dir, "rm-merged"));

        remove("rm-merged", false).unwrap();
        assert!(!ws_dir.exists());
        assert!(!git::branch_exists(&mirror_dir, "rm-merged"));

        cleanup_env();
    }

    #[test]
    fn test_remove_blocks_unmerged_branch() {
        let (_d, _h, _r, identity) = setup_test_env();

        let refs = BTreeMap::from([(identity.clone(), String::new())]);
        create("rm-unmerged", &refs).unwrap();

        let ws_dir = dir("rm-unmerged").unwrap();
        let repo_dir = ws_dir.join("test-repo");

        // Add a commit to the workspace branch so it diverges from main
        let cmds: Vec<Vec<&str>> = vec![
            vec!["git", "config", "user.email", "test@test.com"],
            vec!["git", "config", "user.name", "Test"],
            vec!["git", "config", "commit.gpgsign", "false"],
            vec!["git", "commit", "--allow-empty", "-m", "diverge"],
        ];
        for args in &cmds {
            let output = Command::new(args[0])
                .args(&args[1..])
                .current_dir(&repo_dir)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "command {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let result = remove("rm-unmerged", false);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unmerged branches"),
            "expected 'unmerged branches' in error: {}",
            err
        );

        // Workspace and branch should still exist
        assert!(ws_dir.exists());
        let parsed = parse_identity(&identity).unwrap();
        let mirror_dir = crate::mirror::dir(&parsed).unwrap();
        assert!(git::branch_exists(&mirror_dir, "rm-unmerged"));

        cleanup_env();
    }

    #[test]
    fn test_remove_force_deletes_unmerged_branch() {
        let (_d, _h, _r, identity) = setup_test_env();

        let refs = BTreeMap::from([(identity.clone(), String::new())]);
        create("rm-force", &refs).unwrap();

        let ws_dir = dir("rm-force").unwrap();
        let repo_dir = ws_dir.join("test-repo");

        // Add a commit to the workspace branch so it diverges from main
        let cmds: Vec<Vec<&str>> = vec![
            vec!["git", "config", "user.email", "test@test.com"],
            vec!["git", "config", "user.name", "Test"],
            vec!["git", "config", "commit.gpgsign", "false"],
            vec!["git", "commit", "--allow-empty", "-m", "diverge"],
        ];
        for args in &cmds {
            let output = Command::new(args[0])
                .args(&args[1..])
                .current_dir(&repo_dir)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "command {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        }

        // Force remove should succeed despite unmerged branch
        remove("rm-force", true).unwrap();
        assert!(!ws_dir.exists());

        let parsed = parse_identity(&identity).unwrap();
        let mirror_dir = crate::mirror::dir(&parsed).unwrap();
        assert!(!git::branch_exists(&mirror_dir, "rm-force"));

        cleanup_env();
    }

    #[test]
    fn test_list_all() {
        let (_d, _h, _r, identity) = setup_test_env();

        // Initially empty
        let names = list_all().unwrap();
        assert!(names.is_empty());

        // Create a workspace
        let refs = BTreeMap::from([(identity, String::new())]);
        create("ws-1-list", &refs).unwrap();

        let names = list_all().unwrap();
        assert_eq!(names, vec!["ws-1-list"]);

        cleanup_env();
    }

    #[test]
    fn test_save_and_load_metadata_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let meta = Metadata {
            name: "my-ws".into(),
            branch: "my-ws".into(),
            repos: BTreeMap::from([
                ("github.com/user/repo-a".into(), None),
                ("github.com/user/repo-b".into(), None),
            ]),
            created: Utc::now(),
        };

        save_metadata(tmp.path(), &meta).unwrap();
        let loaded = load_metadata(tmp.path()).unwrap();

        assert_eq!(loaded.name, meta.name);
        assert_eq!(loaded.branch, meta.branch);
        assert_eq!(loaded.repos.len(), meta.repos.len());
        for k in meta.repos.keys() {
            assert!(loaded.repos.contains_key(k));
        }
    }

    #[test]
    fn test_save_and_load_metadata_round_trip_with_refs() {
        let tmp = tempfile::tempdir().unwrap();
        let meta = Metadata {
            name: "my-ws".into(),
            branch: "my-ws".into(),
            repos: BTreeMap::from([
                ("github.com/acme/api-gateway".into(), None),
                (
                    "github.com/acme/user-service".into(),
                    Some(WorkspaceRepoRef {
                        r#ref: "main".into(),
                    }),
                ),
                (
                    "github.com/acme/proto".into(),
                    Some(WorkspaceRepoRef {
                        r#ref: "v1.0".into(),
                    }),
                ),
            ]),
            created: Utc::now(),
        };

        save_metadata(tmp.path(), &meta).unwrap();
        let loaded = load_metadata(tmp.path()).unwrap();

        assert_eq!(loaded.name, meta.name);
        assert_eq!(loaded.repos.len(), 3);

        // Active repo: nil entry
        assert!(loaded.repos["github.com/acme/api-gateway"].is_none());

        // Context repo with branch ref
        assert_eq!(
            loaded.repos["github.com/acme/user-service"]
                .as_ref()
                .unwrap()
                .r#ref,
            "main"
        );

        // Context repo with tag ref
        assert_eq!(
            loaded.repos["github.com/acme/proto"]
                .as_ref()
                .unwrap()
                .r#ref,
            "v1.0"
        );
    }

    #[test]
    fn test_validate_name() {
        let cases = vec![
            ("valid", "my-feature", false),
            ("valid with dots", "fix.bug", false),
            ("empty", "", true),
            ("forward slash", "a/b", true),
            ("backslash", "a\\b", true),
            ("dot", ".", true),
            ("dotdot", "..", true),
        ];
        for (name, input, want_err) in cases {
            let result = validate_name(input);
            if want_err {
                assert!(result.is_err(), "{}: expected error", name);
            } else {
                assert!(result.is_ok(), "{}: unexpected error: {:?}", name, result);
            }
        }
    }

    #[test]
    fn test_create_cleans_up_on_failure() {
        let tmp_data = tempfile::tempdir().unwrap();
        let tmp_home = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("XDG_DATA_HOME", tmp_data.path().to_str().unwrap());
            std::env::set_var("HOME", tmp_home.path().to_str().unwrap());
        }

        let ws_root = tmp_home.path().join("dev").join("workspaces");
        fs::create_dir_all(&ws_root).unwrap();

        // Try to create with a nonexistent repo identity — will fail
        let refs = BTreeMap::from([("nonexistent.local/user/nope".into(), String::new())]);
        let result = create("fail-ws", &refs);
        assert!(result.is_err());

        // Workspace dir should have been cleaned up
        let ws_dir = ws_root.join("fail-ws");
        assert!(
            !ws_dir.exists(),
            "workspace dir should be cleaned up on failure"
        );

        cleanup_env();
    }

    #[test]
    fn test_create_with_context_repo() {
        let (_d, _h, _r, identity) = setup_test_env();

        // Create workspace with the repo as context (ref = "main")
        let refs = BTreeMap::from([(identity.clone(), "main".into())]);
        create("ctx-ws", &refs).unwrap();

        let ws_dir = dir("ctx-ws").unwrap();
        let meta = load_metadata(&ws_dir).unwrap();

        assert!(meta.repos[&identity].is_some());
        assert_eq!(meta.repos[&identity].as_ref().unwrap().r#ref, "main");

        // Worktree directory should exist
        assert!(ws_dir.join("test-repo").exists());

        cleanup_env();
    }

    #[test]
    fn test_add_repos_to_existing_workspace() {
        let (_d, _h, _r, identity) = setup_test_env();

        // Create workspace with active repo
        let refs = BTreeMap::from([(identity.clone(), String::new())]);
        create("add-ws", &refs).unwrap();

        let ws_dir = dir("add-ws").unwrap();

        // Try adding the same repo again — should skip
        add_repos(&ws_dir, &refs).unwrap();

        let meta = load_metadata(&ws_dir).unwrap();
        assert_eq!(meta.repos.len(), 1);

        cleanup_env();
    }

    #[test]
    fn test_has_pending_changes_clean() {
        let (_d, _h, _r, identity) = setup_test_env();

        let refs = BTreeMap::from([(identity, String::new())]);
        create("pending-clean", &refs).unwrap();

        let ws_dir = dir("pending-clean").unwrap();
        let dirty = has_pending_changes(&ws_dir).unwrap();
        assert!(dirty.is_empty());

        cleanup_env();
    }

    #[test]
    fn test_has_pending_changes_uncommitted() {
        let (_d, _h, _r, identity) = setup_test_env();

        let refs = BTreeMap::from([(identity, String::new())]);
        create("pending-dirty", &refs).unwrap();

        let ws_dir = dir("pending-dirty").unwrap();
        let repo_dir = ws_dir.join("test-repo");
        fs::write(repo_dir.join("dirty.txt"), "x").unwrap();

        let dirty = has_pending_changes(&ws_dir).unwrap();
        assert!(dirty.contains(&"test-repo".to_string()));

        cleanup_env();
    }

    #[test]
    fn test_remove_skips_branch_delete_for_context_repos() {
        let (_d, _h, _r, identity) = setup_test_env();

        // Create workspace with context repo (pinned to "main")
        let refs = BTreeMap::from([(identity, "main".into())]);
        create("rm-ws-ctx", &refs).unwrap();

        // Remove should succeed without touching context repo branches
        remove("rm-ws-ctx", false).unwrap();

        cleanup_env();
    }
}

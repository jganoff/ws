use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::Paths;
use crate::filelock;
use crate::git;
use crate::giturl;
use crate::mirror;

const CURRENT_METADATA_VERSION: u32 = 0;

fn default_version() -> u32 {
    CURRENT_METADATA_VERSION
}

fn is_current_version(v: &u32) -> bool {
    *v == CURRENT_METADATA_VERSION
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceRepoRef {
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub r#ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metadata {
    #[serde(
        default = "default_version",
        skip_serializing_if = "is_current_version"
    )]
    pub version: u32,
    pub name: String,
    pub branch: String,
    pub repos: BTreeMap<String, Option<WorkspaceRepoRef>>,
    pub created: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dirs: BTreeMap<String, String>,
}

impl Metadata {
    /// Returns the clone directory name for an identity.
    /// Uses the dirs map if an override exists, otherwise falls back to parsed.repo.
    pub fn dir_name(&self, identity: &str) -> Result<String> {
        if let Some(dir) = self.dirs.get(identity) {
            return Ok(dir.clone());
        }
        let parsed = parse_identity(identity)?;
        Ok(parsed.repo)
    }
}

/// Detects repo-name collisions and returns a dirs map with `owner-repo` entries
/// for all identities that share the same repo short name.
/// Only colliding identities appear in the returned map.
fn compute_dir_names(identities: &[&str]) -> Result<BTreeMap<String, String>> {
    let mut by_repo: BTreeMap<String, Vec<(&str, String)>> = BTreeMap::new();
    for &id in identities {
        let parsed = parse_identity(id)?;
        by_repo
            .entry(parsed.repo.clone())
            .or_default()
            .push((id, parsed.owner.replace('/', "-")));
    }

    let mut dirs = BTreeMap::new();
    for entries in by_repo.values() {
        if entries.len() > 1 {
            for (id, owner) in entries {
                let parsed = parse_identity(id)?;
                dirs.insert(id.to_string(), format!("{}-{}", owner, parsed.repo));
            }
        }
    }
    Ok(dirs)
}

pub const METADATA_FILE: &str = ".wsp.yaml";

pub fn dir(workspaces_dir: &Path, name: &str) -> PathBuf {
    workspaces_dir.join(name)
}

pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("workspace name cannot be empty");
    }
    if name.contains('\0') {
        bail!("workspace name cannot contain null bytes");
    }
    if name.contains('/') || name.contains('\\') {
        bail!("workspace name {:?} cannot contain path separators", name);
    }
    if name.starts_with('-') {
        bail!("workspace name {:?} cannot start with a dash", name);
    }
    if name.starts_with('.') {
        bail!("workspace name {:?} cannot start with a dot", name);
    }
    Ok(())
}

pub fn load_metadata(ws_dir: &Path) -> Result<Metadata> {
    let data = fs::read_to_string(ws_dir.join(METADATA_FILE))?;
    let m: Metadata = serde_yaml_ng::from_str(&data)?;
    if m.version > CURRENT_METADATA_VERSION {
        eprintln!(
            "warning: .wsp.yaml has version {}, but this wsp only supports version {}. Some fields may be ignored.",
            m.version, CURRENT_METADATA_VERSION
        );
    }
    for (identity, dir_name) in &m.dirs {
        validate_dir_name(dir_name)
            .map_err(|e| anyhow::anyhow!("invalid dir override for {}: {}", identity, e))?;
    }
    Ok(m)
}

fn validate_dir_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("directory name cannot be empty");
    }
    if name.contains('\0') || name.contains('/') || name.contains('\\') {
        bail!(
            "directory name {:?} contains path separators or null bytes",
            name
        );
    }
    if name == "." || name == ".." || name.contains("..") {
        bail!("directory name {:?} contains path traversal", name);
    }
    Ok(())
}

pub fn save_metadata(ws_dir: &Path, m: &Metadata) -> Result<()> {
    let data = serde_yaml_ng::to_string(m)?;
    let mut tmp =
        tempfile::NamedTempFile::new_in(ws_dir).context("creating temp file for atomic save")?;
    tmp.write_all(data.as_bytes())
        .context("writing metadata to temp file")?;
    tmp.persist(ws_dir.join(METADATA_FILE))
        .context("renaming temp file to metadata")?;
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

pub fn create(
    paths: &Paths,
    name: &str,
    repo_refs: &BTreeMap<String, String>,
    branch_prefix: Option<&str>,
    upstream_urls: &BTreeMap<String, String>,
) -> Result<()> {
    validate_name(name)?;

    let ws_dir = dir(&paths.workspaces_dir, name);
    if ws_dir.exists() {
        bail!("workspace {:?} already exists", name);
    }

    fs::create_dir_all(&ws_dir)?;

    let branch = match branch_prefix.filter(|p| !p.is_empty()) {
        Some(prefix) => format!("{}/{}", prefix, name),
        None => name.to_string(),
    };

    match create_inner(
        &paths.mirrors_dir,
        &branch,
        &ws_dir,
        name,
        repo_refs,
        upstream_urls,
    ) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Clean up workspace dir on failure (best-effort)
            let _ = fs::remove_dir_all(&ws_dir);
            Err(e)
        }
    }
}

fn create_inner(
    mirrors_dir: &Path,
    branch: &str,
    ws_dir: &Path,
    name: &str,
    repo_refs: &BTreeMap<String, String>,
    upstream_urls: &BTreeMap<String, String>,
) -> Result<()> {
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

    let identities: Vec<&str> = repo_refs.keys().map(|s| s.as_str()).collect();
    let dirs = compute_dir_names(&identities)?;

    let meta = Metadata {
        version: CURRENT_METADATA_VERSION,
        name: name.to_string(),
        branch: branch.to_string(),
        repos,
        created: Utc::now(),
        dirs: dirs.clone(),
    };

    for (identity, r) in repo_refs {
        let dn = meta.dir_name(identity)?;
        let upstream = upstream_urls
            .get(identity)
            .map(|s| s.as_str())
            .unwrap_or("");
        clone_from_mirror(mirrors_dir, ws_dir, identity, &dn, branch, r, upstream)
            .map_err(|e| anyhow::anyhow!("cloning repo {}: {}", identity, e))?;
    }

    save_metadata(ws_dir, &meta)?;
    Ok(())
}

/// Validate that an existing directory can be adopted as a managed repo.
/// Checks that it is a git repo, has an origin remote, and its URL matches the expected identity.
fn validate_existing_dir(dir: &Path, expected_identity: &str) -> Result<()> {
    if !dir.join(".git").exists() {
        bail!(
            "directory {:?} exists but is not a git repository",
            dir.file_name().unwrap_or_default()
        );
    }
    let origin_url = git::remote_get_url(dir, "origin").map_err(|_| {
        anyhow::anyhow!(
            "directory {:?} exists but has no origin remote",
            dir.file_name().unwrap_or_default()
        )
    })?;
    let parsed = giturl::parse(&origin_url).map_err(|e| {
        anyhow::anyhow!(
            "directory {:?} has unparseable origin URL {:?}: {}",
            dir.file_name().unwrap_or_default(),
            origin_url,
            e
        )
    })?;
    let actual_identity = parsed.identity();
    if actual_identity != expected_identity {
        bail!(
            "directory {:?} origin remote ({}) doesn't match expected repo ({})",
            dir.file_name().unwrap_or_default(),
            actual_identity,
            expected_identity
        );
    }
    Ok(())
}

/// Prompt the user about origin URL when adopting an existing directory.
/// If the clone's origin URL differs from the registered URL, offer to repoint.
/// In non-interactive contexts, keeps as-is with a warning.
fn prompt_origin_url_for_adopt(dir: &Path, registered_url: &str) -> Result<()> {
    let clone_url = match git::remote_get_url(dir, "origin") {
        Ok(url) => url,
        Err(_) => return Ok(()), // no origin — already caught by validate_existing_dir
    };

    if clone_url == registered_url {
        return Ok(());
    }

    // Check if they resolve to the same identity (e.g., SSH vs HTTPS for same repo).
    // If identities match, the URLs are functionally equivalent but syntactically different.
    let clone_identity = giturl::parse(&clone_url).ok().map(|p| p.identity());
    let registered_identity = giturl::parse(registered_url).ok().map(|p| p.identity());
    if clone_identity.is_none()
        || registered_identity.is_none()
        || clone_identity != registered_identity
    {
        // Identity mismatch or unparseable — validate_existing_dir should have caught this
        return Ok(());
    }

    let dir_name = dir.file_name().unwrap_or_default().to_string_lossy();

    if !std::io::stdin().is_terminal() {
        eprintln!(
            "  warning: {}/ origin URL differs from registered URL (non-interactive, leaving as-is)",
            dir_name
        );
        eprintln!("    clone:      {}", clone_url);
        eprintln!("    registered: {}", registered_url);
        return Ok(());
    }

    eprintln!(
        "  warning: {}/ origin URL differs from registered URL",
        dir_name
    );
    eprintln!("    clone:      {}", clone_url);
    eprintln!("    registered: {}", registered_url);
    eprintln!("    [1] Keep current origin URL (default)");
    eprintln!("    [2] Repoint origin to registered URL");
    eprint!("  choice [1]: ");

    let choice = read_stdin_line();
    if choice.trim() == "2" {
        git::remote_set_url(dir, "origin", registered_url)?;
        eprintln!("  repointed origin to {}", registered_url);
    }

    Ok(())
}

/// Prompt the user about branch state when adopting an existing directory.
/// Returns Ok(()) after handling the branch (or leaving as-is).
/// In non-interactive contexts (stdin is not a terminal), defaults to leaving as-is.
fn prompt_branch_for_adopt(dir: &Path, ws_branch: &str) -> Result<()> {
    let current = git::branch_current(dir).unwrap_or_default();

    if current == ws_branch {
        // Already on workspace branch — nothing to do
        return Ok(());
    }

    let branch_exists = git::branch_exists(dir, ws_branch);
    let dir_name = dir.file_name().unwrap_or_default().to_string_lossy();

    if !std::io::stdin().is_terminal() {
        eprintln!(
            "  warning: {} is on branch '{}', not workspace branch '{}' (non-interactive, leaving as-is)",
            dir_name, current, ws_branch
        );
        return Ok(());
    }

    if branch_exists {
        eprintln!(
            "  warning: {} is on branch '{}', workspace branch is '{}'",
            dir_name, current, ws_branch
        );
        eprintln!("    [1] Leave as-is (default)");
        eprintln!("    [2] Switch to workspace branch '{}'", ws_branch);
    } else {
        eprintln!(
            "  warning: {} is on branch '{}', workspace branch '{}' does not exist",
            dir_name, current, ws_branch
        );
        eprintln!("    [1] Leave as-is (default)");
        eprintln!(
            "    [2] Create and checkout workspace branch '{}' from current HEAD",
            ws_branch
        );
    }

    eprint!("  choice [1]: ");
    let choice = read_stdin_line();

    if choice.trim() == "2" {
        if branch_exists {
            git::checkout(dir, ws_branch)?;
            eprintln!("  switched to branch '{}'", ws_branch);
        } else {
            git::checkout_new_branch(dir, ws_branch, "HEAD")?;
            eprintln!("  created and switched to branch '{}'", ws_branch);
        }
    }

    Ok(())
}

fn read_stdin_line() -> String {
    let stdin = std::io::stdin();
    let mut line = String::new();
    let _ = stdin.lock().read_line(&mut line);
    line
}

/// Propagate mirror refs into an existing clone directory.
/// Runs steps 4-6 of the clone_from_mirror process:
/// populate origin/* refs, set origin/HEAD, fix default branch tracking.
fn propagate_mirror_refs(mirrors_dir: &Path, dest: &Path, identity: &str) -> Result<()> {
    let parsed = parse_identity(identity)?;
    let mirror_dir = mirror::dir(mirrors_dir, &parsed);
    if !mirror_dir.exists() {
        return Ok(());
    }

    let mirror_default_br = git::default_branch_from_mirror(&mirror_dir).ok();

    // Populate origin/* refs from mirror (local fetch, no network)
    let _ = git::fetch_from_path(dest, &mirror_dir, MIRROR_PROPAGATE_REFSPEC, false);

    // Set origin/HEAD
    if let Some(ref default_br) = mirror_default_br {
        let _ = git::remote_set_head(dest, "origin", default_br);
    }

    // Fix default branch tracking and fast-forward local default branch
    if let Some(ref default_br) = mirror_default_br {
        let local_ref = format!("refs/heads/{}", default_br);
        let origin_ref = format!("origin/{}", default_br);
        if git::ref_exists(dest, &format!("refs/remotes/{}", origin_ref)) {
            let _ = git::set_upstream(dest, default_br, &origin_ref);
            // Only fast-forward; don't reset a branch that has local-only commits
            if git::is_ancestor(dest, &local_ref, &origin_ref) {
                let _ = git::update_ref(dest, &local_ref, &origin_ref);
            }
        }
    }

    Ok(())
}

pub fn add_repos(
    mirrors_dir: &Path,
    ws_dir: &Path,
    repo_refs: &BTreeMap<String, String>,
    upstream_urls: &BTreeMap<String, String>,
) -> Result<()> {
    // Phase 1: snapshot metadata to determine branch and dir layout (fast lock)
    let snapshot = filelock::read_metadata(ws_dir)?;
    let branch = snapshot.branch.clone();

    // Phase 2: clone repos from mirrors outside the lock (slow I/O).
    // Pre-compute directory names for the union of existing + new repos using
    // compute_dir_names, which detects collisions both against existing repos
    // and among the new repos themselves (e.g. alice/utils + bob/utils).
    // Directory renames for existing repos are deferred to phase 3 (under lock).

    // Filter out repos already in the workspace
    let new_identities: Vec<&String> = repo_refs
        .keys()
        .filter(|id| {
            if snapshot.repos.contains_key(id.as_str()) {
                eprintln!("  {} already in workspace, skipping", id);
                false
            } else {
                true
            }
        })
        .collect();

    // Compute dir names for existing + new repos together to detect all collisions
    let all_identities: Vec<&str> = snapshot
        .repos
        .keys()
        .map(|s| s.as_str())
        .chain(new_identities.iter().map(|s| s.as_str()))
        .collect();
    let all_dirs = compute_dir_names(&all_identities)?;

    // Determine which existing repos need renaming (they now appear in all_dirs
    // but weren't in snapshot.dirs, or their dir name changed)
    struct RenameInfo {
        existing_id: String,
        old_dir: String,
        new_dir: String,
    }
    let mut renames: Vec<RenameInfo> = Vec::new();
    for existing_id in snapshot.repos.keys() {
        if let Some(new_dir) = all_dirs.get(existing_id) {
            let old_dir = snapshot.dir_name(existing_id)?;
            if *new_dir != old_dir {
                renames.push(RenameInfo {
                    existing_id: existing_id.clone(),
                    old_dir,
                    new_dir: new_dir.clone(),
                });
            }
        }
    }

    struct CloneInfo {
        identity: String,
        git_ref: String,
        dir_name: String,
    }
    let mut clones: Vec<CloneInfo> = Vec::new();

    for identity in &new_identities {
        let r = &repo_refs[identity.as_str()];
        let upstream = upstream_urls
            .get(identity.as_str())
            .map(|s| s.as_str())
            .unwrap_or("");

        // Use disambiguated name from all_dirs if present, otherwise default
        let dn = match all_dirs.get(identity.as_str()) {
            Some(d) => d.clone(),
            None => {
                let parsed = parse_identity(identity)?;
                parsed.repo
            }
        };

        let dest = ws_dir.join(&dn);
        if dest.exists() {
            // Adopt existing directory instead of cloning
            validate_existing_dir(&dest, identity)?;
            propagate_mirror_refs(mirrors_dir, &dest, identity)?;
            if !upstream.is_empty() {
                prompt_origin_url_for_adopt(&dest, upstream)?;
            }
            if r.is_empty() {
                prompt_branch_for_adopt(&dest, &branch)?;
            }
            eprintln!("  adopted existing directory {}/", dn);
        } else {
            clone_from_mirror(mirrors_dir, ws_dir, identity, &dn, &branch, r, upstream)
                .map_err(|e| anyhow::anyhow!("cloning repo {}: {}", identity, e))?;
        }

        clones.push(CloneInfo {
            identity: identity.to_string(),
            git_ref: r.clone(),
            dir_name: dn,
        });
    }

    if clones.is_empty() {
        return Ok(());
    }

    // Phase 3: rename colliding directories and update metadata under lock (fast)
    filelock::with_metadata(ws_dir, |meta| {
        // Rename existing repos that now collide with new additions
        for ri in &renames {
            if meta.repos.contains_key(&ri.existing_id) {
                fs::rename(ws_dir.join(&ri.old_dir), ws_dir.join(&ri.new_dir)).map_err(|e| {
                    anyhow::anyhow!("renaming directory for {}: {}", ri.existing_id, e)
                })?;
                meta.dirs.insert(ri.existing_id.clone(), ri.new_dir.clone());
            }
        }

        // Register new repos
        for ci in &clones {
            if all_dirs.contains_key(&ci.identity) {
                meta.dirs.insert(ci.identity.clone(), ci.dir_name.clone());
            }

            if ci.git_ref.is_empty() {
                meta.repos.insert(ci.identity.clone(), None);
            } else {
                meta.repos.insert(
                    ci.identity.clone(),
                    Some(WorkspaceRepoRef {
                        r#ref: ci.git_ref.clone(),
                    }),
                );
            }
        }
        Ok(())
    })?;
    Ok(())
}

/// LEGACY(v0.5): remove the `wsp-mirror` remote from a clone if it exists.
/// Old versions of wsp added this remote; we no longer use it.
fn remove_legacy_wsp_mirror(clone_dir: &Path) {
    if git::has_remote(clone_dir, "wsp-mirror") {
        let _ = git::remove_remote(clone_dir, "wsp-mirror");
    }
}

/// Fetch a mirror from upstream and propagate refs to a clone (best-effort).
fn fetch_and_propagate(mirrors_dir: &Path, clone_dir: &Path, identity: &str) -> Result<()> {
    let parsed = parse_identity(identity)?;
    let mirror_path = mirror::dir(mirrors_dir, &parsed);
    remove_legacy_wsp_mirror(clone_dir);
    git::fetch(&mirror_path, true)?;
    git::fetch_from_path(clone_dir, &mirror_path, MIRROR_PROPAGATE_REFSPEC, true)?;
    Ok(())
}

pub fn remove_repos(
    mirrors_dir: &Path,
    ws_dir: &Path,
    identities_to_remove: &[String],
    force: bool,
) -> Result<()> {
    // Phase 1: snapshot metadata for safety checks (fast lock)
    let snapshot = filelock::read_metadata(ws_dir)?;

    // Validate all identities exist in the workspace
    for identity in identities_to_remove {
        if !snapshot.repos.contains_key(identity) {
            bail!("repo {} is not in this workspace", identity);
        }
    }

    // Phase 2: safety checks including network fetch (slow, no lock held)
    if !force {
        let mut problems: Vec<String> = Vec::new();
        for identity in identities_to_remove {
            let entry = &snapshot.repos[identity];
            let is_active = match entry {
                None => true,
                Some(re) => re.r#ref.is_empty(),
            };
            if !is_active {
                continue;
            }

            let dn = snapshot.dir_name(identity)?;
            let clone_dir = ws_dir.join(&dn);

            let changed = git::changed_file_count(&clone_dir).unwrap_or(0);
            let ahead = git::ahead_count(&clone_dir).unwrap_or(0);
            if changed > 0 || ahead > 0 {
                problems.push(format!("{} (pending changes)", identity));
                continue;
            }

            let fetch_failed = fetch_and_propagate(mirrors_dir, &clone_dir, identity).is_err();
            if fetch_failed {
                eprintln!("  warning: fetch failed for {}, using local data", identity);
            }

            if git::branch_exists(&clone_dir, &snapshot.branch) {
                let default_branch = git::default_branch_for_remote(&clone_dir, "origin")
                    .or_else(|_| git::default_branch(&clone_dir))
                    .unwrap_or_default();
                if !default_branch.is_empty() {
                    let merge_target = format!("origin/{}", default_branch);
                    let target = if git::ref_exists(&clone_dir, &merge_target) {
                        merge_target
                    } else {
                        default_branch
                    };
                    match git::branch_safety(&clone_dir, &snapshot.branch, &target) {
                        git::BranchSafety::Merged | git::BranchSafety::SquashMerged => {}
                        git::BranchSafety::PushedToRemote => {
                            let mut msg =
                                format!("{} (unmerged branch, but pushed to remote)", identity);
                            if fetch_failed {
                                msg.push_str(" (fetch failed, local data may be stale)");
                            }
                            problems.push(msg);
                        }
                        git::BranchSafety::Unmerged => {
                            let mut msg = format!("{} (unmerged branch)", identity);
                            if fetch_failed {
                                msg.push_str(" (fetch failed, local data may be stale)");
                            }
                            problems.push(msg);
                        }
                    }
                }
            }
        }

        if !problems.is_empty() {
            let mut list = String::new();
            for p in &problems {
                list.push_str(&format!("\n  - {}", p));
            }
            bail!(
                "cannot remove repos:{}\n\nUse --force to remove anyway",
                list
            );
        }
    }

    // Phase 3: remove directories and update metadata under lock (fast)
    filelock::with_metadata(ws_dir, |meta| {
        for identity in identities_to_remove {
            let dn = meta.dir_name(identity)?;
            let clone_path = ws_dir.join(&dn);

            if let Err(e) = fs::remove_dir_all(&clone_path) {
                eprintln!("  warning: removing clone for {}: {}", identity, e);
            }

            meta.repos.remove(identity);
            meta.dirs.remove(identity);
        }

        // Recalculate dir names for remaining repos
        let remaining_ids: Vec<&str> = meta.repos.keys().map(|s| s.as_str()).collect();
        let new_dirs = compute_dir_names(&remaining_ids)?;

        // Check if any collision disambiguations can be undone
        for (identity, new_dir) in &new_dirs {
            if let Some(old_dir) = meta.dirs.get(identity)
                && old_dir != new_dir
                && let Err(e) = fs::rename(ws_dir.join(old_dir), ws_dir.join(new_dir))
            {
                eprintln!("  warning: renaming directory for {}: {}", identity, e);
            }
        }

        // Check if repos that were disambiguated can now use their short name
        for identity in meta.repos.keys() {
            if let Some(old_dir) = meta.dirs.get(identity).cloned()
                && !new_dirs.contains_key(identity)
            {
                let parsed = parse_identity(identity)?;
                let short_name = parsed.repo.clone();
                if let Err(e) = fs::rename(ws_dir.join(&old_dir), ws_dir.join(&short_name)) {
                    eprintln!("  warning: renaming directory for {}: {}", identity, e);
                }
            }
        }

        // Update dirs map
        meta.dirs = new_dirs;
        Ok(())
    })?;
    Ok(())
}

/// Resolved per-repo info for workspace-scoped commands.
pub struct RepoInfo {
    pub identity: String,
    pub dir_name: String,
    pub clone_dir: PathBuf,
    pub is_context: bool,
    pub pinned_ref: Option<String>,
    pub error: Option<String>,
}

impl Metadata {
    /// Build a RepoInfo for each repo in the workspace.
    pub fn repo_infos(&self, ws_dir: &Path) -> Vec<RepoInfo> {
        let mut infos = Vec::new();
        for (identity, entry) in &self.repos {
            let is_context = match entry {
                Some(re) => !re.r#ref.is_empty(),
                None => false,
            };
            let pinned_ref = match entry {
                Some(re) if !re.r#ref.is_empty() => Some(re.r#ref.clone()),
                _ => None,
            };
            let dir_name = match self.dir_name(identity) {
                Ok(d) => d,
                Err(e) => {
                    infos.push(RepoInfo {
                        identity: identity.clone(),
                        dir_name: identity.clone(),
                        clone_dir: PathBuf::new(),
                        is_context,
                        pinned_ref,
                        error: Some(e.to_string()),
                    });
                    continue;
                }
            };
            let clone_dir = ws_dir.join(&dir_name);
            infos.push(RepoInfo {
                identity: identity.clone(),
                dir_name,
                clone_dir,
                is_context,
                pinned_ref,
                error: None,
            });
        }
        infos
    }
}

const MIRROR_PROPAGATE_REFSPEC: &str = "+refs/remotes/origin/*:refs/remotes/origin/*";

/// Propagate mirror refs into workspace clones (parallel, best-effort).
/// Fetches `refs/remotes/origin/*` from the mirror into each clone's `origin/*`.
/// Also removes the legacy `wsp-mirror` remote if present.
/// Callers wanting deleted-branch cleanup should pass `prune: true`.
pub fn propagate_mirror_to_clones(mirrors_dir: &Path, ws_dir: &Path, meta: &Metadata, prune: bool) {
    let clones: Vec<(String, PathBuf, PathBuf)> = meta
        .repos
        .keys()
        .filter_map(|id| {
            let dn = meta.dir_name(id).ok()?;
            let parsed = parse_identity(id).ok()?;
            let mirror_path = mirror::dir(mirrors_dir, &parsed);
            Some((id.clone(), ws_dir.join(dn), mirror_path))
        })
        .collect();

    if clones.is_empty() {
        return;
    }

    std::thread::scope(|s| {
        let handles: Vec<_> = clones
            .iter()
            .map(|(id, clone_dir, mirror_path)| {
                s.spawn(move || {
                    remove_legacy_wsp_mirror(clone_dir);
                    if let Err(e) = git::fetch_from_path(
                        clone_dir,
                        mirror_path,
                        MIRROR_PROPAGATE_REFSPEC,
                        prune,
                    ) {
                        eprintln!("  warning: propagate mirror for {}: {}", id, e);
                    }
                })
            })
            .collect();
        for h in handles {
            let _ = h.join();
        }
    });
}

/// Noise files that are safe to delete without warning.
const NOISE_FILES: &[&str] = &[".DS_Store", "Thumbs.db", "desktop.ini"];

/// Check workspace root for user content not managed by wsp.
/// Returns a list of human-readable problem descriptions.
// TODO: support .wspignore (global + per-workspace) to let users suppress
// specific paths from this check. See docs/roadmap.md.
pub(crate) fn check_root_content(ws_dir: &Path, metadata: &Metadata) -> Result<Vec<String>> {
    let mut problems = Vec::new();

    // Build set of known repo dir names
    let mut repo_dirs: std::collections::HashSet<String> = std::collections::HashSet::new();
    for identity in metadata.repos.keys() {
        if let Ok(dn) = metadata.dir_name(identity) {
            repo_dirs.insert(dn);
        }
    }

    let go_work_is_wsp = ws_dir.join("go.work").exists() && check_go_work(ws_dir).is_none();

    for entry in fs::read_dir(ws_dir).context("reading workspace root directory")? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Skip .wsp.yaml
        if name_str == METADATA_FILE {
            continue;
        }

        // Skip repo clone dirs (checked by repo safety)
        if repo_dirs.contains(name_str.as_ref()) {
            continue;
        }

        // Skip noise files
        if NOISE_FILES.contains(&name_str.as_ref()) {
            continue;
        }

        // AGENTS.md
        if name_str == "AGENTS.md" {
            if let Some(problem) = check_agents_md(ws_dir) {
                problems.push(problem);
            }
            continue;
        }

        // CLAUDE.md
        if name_str == "CLAUDE.md" {
            if let Some(problem) = check_claude_md(ws_dir) {
                problems.push(problem);
            }
            continue;
        }

        // .claude/ directory
        if name_str == ".claude" {
            problems.extend(check_claude_dir(ws_dir));
            continue;
        }

        // go.work
        if name_str == "go.work" {
            if let Some(problem) = check_go_work(ws_dir) {
                problems.push(problem);
            }
            continue;
        }

        // go.work.sum — safe when go.work is wsp-generated
        if name_str == "go.work.sum" && go_work_is_wsp {
            continue;
        }

        // Everything else is flagged
        let ft = entry.file_type()?;
        if ft.is_dir() {
            problems.push(format!("?? {}/", name_str));
        } else {
            problems.push(format!("?? {}", name_str));
        }
    }

    Ok(problems)
}

/// Check AGENTS.md for user content outside wsp markers.
fn check_agents_md(ws_dir: &Path) -> Option<String> {
    let path = ws_dir.join("AGENTS.md");
    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Some(" M AGENTS.md (unreadable)".into()),
    };

    // Find the begin marker
    let begin_idx = match content.find(crate::agentmd::MARKER_BEGIN) {
        Some(idx) => idx,
        None => return Some(" M AGENTS.md (wsp markers missing)".into()),
    };

    // Check content before the begin marker for user additions
    let preamble = &content[..begin_idx];
    for line in preamble.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Scaffold lines emitted by agentmd::build_initial_file()
        if trimmed.starts_with("# Workspace: ") {
            continue;
        }
        if trimmed == "<!-- Add your project-specific notes for AI agents here -->" {
            continue;
        }
        // Any other non-blank line is user content
        return Some(" M AGENTS.md (user-added content)".into());
    }

    // Check content after the end marker for user additions.
    // agentmd::replace_marked_section preserves post-marker content,
    // so users reasonably expect it persists across wsp operations.
    if let Some(end_idx) = content.find(crate::agentmd::MARKER_END) {
        let after_end = &content[end_idx + crate::agentmd::MARKER_END.len()..];
        for line in after_end.lines() {
            if !line.trim().is_empty() {
                return Some(" M AGENTS.md (user-added content after markers)".into());
            }
        }
    }

    None
}

/// Check CLAUDE.md — symlink to AGENTS.md is fine, anything else is flagged.
fn check_claude_md(ws_dir: &Path) -> Option<String> {
    let path = ws_dir.join("CLAUDE.md");
    match fs::symlink_metadata(&path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                match fs::read_link(&path) {
                    Ok(target) if target == Path::new("AGENTS.md") => None,
                    _ => Some(" M CLAUDE.md (symlink to unexpected target)".into()),
                }
            } else {
                Some("?? CLAUDE.md".into())
            }
        }
        Err(_) => None, // doesn't exist, fine
    }
}

/// Check .claude/ directory for non-wsp content.
fn check_claude_dir(ws_dir: &Path) -> Vec<String> {
    let claude_dir = ws_dir.join(".claude");
    let mut problems = Vec::new();

    // Known wsp-managed paths (relative to .claude/)
    let managed: std::collections::HashSet<&str> =
        ["skills/wsp-manage/SKILL.md", "skills/wsp-report/SKILL.md"]
            .iter()
            .copied()
            .collect();

    // Intermediate directories that only contain managed content
    let managed_dirs: std::collections::HashSet<&str> =
        ["skills", "skills/wsp-manage", "skills/wsp-report"]
            .iter()
            .copied()
            .collect();

    fn walk(
        base: &Path,
        rel: &str,
        managed: &std::collections::HashSet<&str>,
        managed_dirs: &std::collections::HashSet<&str>,
        problems: &mut Vec<String>,
    ) {
        let dir = if rel.is_empty() {
            base.to_path_buf()
        } else {
            base.join(rel)
        };
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            let child_rel = if rel.is_empty() {
                name_str.to_string()
            } else {
                format!("{}/{}", rel, name_str)
            };

            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };

            if ft.is_dir() {
                if managed_dirs.contains(child_rel.as_str()) {
                    walk(base, &child_rel, managed, managed_dirs, problems);
                } else {
                    problems.push(format!("?? .claude/{}", child_rel));
                }
            } else if !managed.contains(child_rel.as_str()) {
                problems.push(format!("?? .claude/{}", child_rel));
            }
        }
    }

    walk(&claude_dir, "", &managed, &managed_dirs, &mut problems);
    problems
}

/// Check go.work — wsp-generated header means it's managed.
fn check_go_work(ws_dir: &Path) -> Option<String> {
    let path = ws_dir.join("go.work");
    if !path.exists() {
        return None;
    }
    match fs::read_to_string(&path) {
        Ok(content) if content.starts_with(crate::lang::GO_WORK_HEADER) => None,
        Ok(_) => Some("?? go.work".into()),
        Err(_) => Some("?? go.work (unreadable)".into()),
    }
}

pub fn remove(paths: &Paths, name: &str, force: bool, permanent: bool) -> Result<()> {
    let ws_dir = dir(&paths.workspaces_dir, name);
    let meta =
        load_metadata(&ws_dir).map_err(|e| anyhow::anyhow!("reading workspace metadata: {}", e))?;

    if !force {
        let mut problems: Vec<String> = Vec::new();

        for (identity, entry) in &meta.repos {
            let is_active = match entry {
                None => true,
                Some(re) => re.r#ref.is_empty(),
            };
            if !is_active {
                continue;
            }

            let dn = meta.dir_name(identity)?;
            let clone_dir = ws_dir.join(&dn);

            // Check for pending local changes on HEAD
            let changed = git::changed_file_count(&clone_dir).unwrap_or(0);
            let ahead = git::ahead_count(&clone_dir).unwrap_or(0);
            if changed > 0 || ahead > 0 {
                problems.push(format!("{} (pending changes)", identity));
                continue;
            }

            // Check if HEAD is on the wrong branch — the workspace branch may
            // have unpushed commits that the HEAD-relative checks above missed.
            let current = git::branch_current(&clone_dir).unwrap_or_default();
            if current != meta.branch && git::branch_exists(&clone_dir, &meta.branch) {
                let ws_ahead =
                    git::commit_count(&clone_dir, &format!("origin/{}", meta.branch), &meta.branch)
                        .or_else(|_| {
                            // No remote tracking branch — count all commits vs default branch
                            let default = git::default_branch(&clone_dir).unwrap_or("main".into());
                            git::commit_count(
                                &clone_dir,
                                &format!("origin/{}", default),
                                &meta.branch,
                            )
                        })
                        .unwrap_or(0);
                if ws_ahead > 0 {
                    problems.push(format!(
                        "{} (not on workspace branch; {} has {} unpushed commit{})",
                        identity,
                        meta.branch,
                        ws_ahead,
                        if ws_ahead == 1 { "" } else { "s" }
                    ));
                    continue;
                }
            }

            let fetch_failed =
                fetch_and_propagate(&paths.mirrors_dir, &clone_dir, identity).is_err();
            if fetch_failed {
                eprintln!("  warning: fetch failed for {}, using local data", identity);
            }

            if !git::branch_exists(&clone_dir, &meta.branch) {
                continue;
            }
            let default_branch = match git::default_branch_for_remote(&clone_dir, "origin") {
                Ok(b) => b,
                Err(_) => match git::default_branch(&clone_dir) {
                    Ok(b) => b,
                    Err(e) => {
                        eprintln!(
                            "  warning: cannot detect default branch for {}: {}",
                            identity, e
                        );
                        continue;
                    }
                },
            };
            let merge_target = format!("origin/{}", default_branch);
            let target = if git::ref_exists(&clone_dir, &merge_target) {
                merge_target
            } else {
                default_branch
            };
            match git::branch_safety(&clone_dir, &meta.branch, &target) {
                git::BranchSafety::Merged | git::BranchSafety::SquashMerged => {}
                git::BranchSafety::PushedToRemote => {
                    let mut msg = format!("{} (unmerged branch, but pushed to remote)", identity);
                    if fetch_failed {
                        msg.push_str(" (fetch failed, local data may be stale)");
                    }
                    problems.push(msg);
                }
                git::BranchSafety::Unmerged => {
                    let mut msg = format!("{} (unmerged branch)", identity);
                    if fetch_failed {
                        msg.push_str(" (fetch failed, local data may be stale)");
                    }
                    problems.push(msg);
                }
            }
        }

        // Check workspace root for user content
        match check_root_content(&ws_dir, &meta) {
            Ok(root_problems) => {
                if !root_problems.is_empty() {
                    let mut msg = String::from("workspace root has user content:");
                    for p in &root_problems {
                        msg.push_str(&format!("\n      {}", p));
                    }
                    problems.push(msg);
                }
            }
            Err(e) => {
                eprintln!("  warning: root content check failed: {}", e);
            }
        }

        if !problems.is_empty() {
            let mut sorted = problems;
            sorted.sort();
            let mut list = String::new();
            for p in &sorted {
                list.push_str(&format!("\n  - {}", p));
            }
            bail!(
                "workspace {:?} has unsaved work ({}):{}\n\nUse --force to remove anyway",
                name,
                meta.branch,
                list
            );
        }
    }

    if permanent {
        fs::remove_dir_all(&ws_dir)?;
    } else {
        crate::gc::move_to_gc(paths, name, &meta.branch)?;
    }
    Ok(())
}

pub fn list_all(workspaces_dir: &Path) -> Result<Vec<String>> {
    if !workspaces_dir.exists() {
        return Ok(Vec::new());
    }

    let mut names = Vec::new();
    for entry in fs::read_dir(workspaces_dir)? {
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

/// Clone a repo into the workspace from its bare mirror.
///
/// Steps:
///   1. `git clone --local <mirror> <dest>` — hardlinks, origin → mirror path
///   2. `git remote set-url origin <upstream_url>` — repoint to upstream
///   3. Read default branch from mirror
///   4. `git fetch <mirror_path> +refs/remotes/origin/*:refs/remotes/origin/*`
///      — populate origin refs from mirror (local-only, no network, no trace)
///   5. `git remote set-head origin <default_branch>`
///   6. Fix tracking: set-upstream-to origin/<default> or unset
///   7. Checkout: context repos get pinned ref; active repos get workspace
///      branch via `--no-track` (intentional: tracking `origin/main`
///      would cause bare `git push` to target the wrong branch)
fn clone_from_mirror(
    mirrors_dir: &Path,
    ws_dir: &Path,
    identity: &str,
    dir_name: &str,
    branch: &str,
    git_ref: &str,
    upstream_url: &str,
) -> Result<()> {
    let parsed = parse_identity(identity)?;
    let mirror_dir = mirror::dir(mirrors_dir, &parsed);
    let dest = ws_dir.join(dir_name);

    // 1. Clone from mirror (hardlinks, origin → mirror path)
    git::clone_local(&mirror_dir, &dest)?;

    // 2. Repoint origin to the real upstream URL
    if !upstream_url.is_empty() {
        git::remote_set_url(&dest, "origin", upstream_url)?;
    }

    // 3. Read default branch from mirror
    let mirror_default_br = git::default_branch_from_mirror(&mirror_dir).ok();

    // 4. Populate origin/* refs from mirror (local fetch, no network).
    // Note: bare mirrors have refs/remotes/origin/* only after their first
    // upstream fetch (`git fetch` in the mirror). Before that, only
    // refs/heads/* exists (from clone_bare). Step 1's `git clone --local`
    // already creates origin/* from the mirror's refs/heads/*, so this
    // fetch is a no-op on fresh mirrors but essential for mirrors that
    // have been fetched (the normal production path).
    git::fetch_from_path(&dest, &mirror_dir, MIRROR_PROPAGATE_REFSPEC, false)?;

    // 5. Set origin/HEAD
    if let Some(ref default_br) = mirror_default_br {
        let _ = git::remote_set_head(&dest, "origin", default_br);
    }

    // 6. Fix default branch tracking and fast-forward local default branch.
    // Clone from mirror creates main tracking origin/main. Re-set explicitly,
    // then fast-forward local main to match origin/main (step 1's clone may
    // have created it from the mirror's stale HEAD).
    if let Some(ref default_br) = mirror_default_br {
        let local_ref = format!("refs/heads/{}", default_br);
        let origin_ref = format!("origin/{}", default_br);
        if git::ref_exists(&dest, &format!("refs/remotes/{}", origin_ref)) {
            let _ = git::set_upstream(&dest, default_br, &origin_ref);
            if git::is_ancestor(&dest, &local_ref, &origin_ref) {
                let _ = git::update_ref(&dest, &local_ref, &origin_ref);
            }
        } else {
            let _ = git::unset_upstream(&dest, default_br);
        }
    }

    // 7. Checkout the right ref/branch
    // Context repo: check out at the specified ref
    if !git_ref.is_empty() {
        let origin_ref = format!("origin/{}", git_ref);
        if git::branch_exists(&dest, git_ref) {
            // Local branch already exists
            git::checkout(&dest, git_ref)?;
        } else if git::ref_exists(&dest, &format!("refs/remotes/origin/{}", git_ref)) {
            // Create branch from origin/<ref>, no tracking — devs must
            // explicitly `git push -u` to avoid accidentally pushing to the
            // wrong branch.
            git::checkout_new_branch(&dest, git_ref, &origin_ref)?;
        } else {
            // Tag or SHA: detached HEAD
            git::checkout_detached(&dest, git_ref)?;
        }
        return Ok(());
    }

    // Active repo: create/checkout workspace branch
    if git::branch_exists(&dest, branch) {
        git::checkout(&dest, branch)?;
        return Ok(());
    }

    // No upstream tracking — the workspace branch differs from the default
    // branch, so tracking origin/<default> would cause a bare `git push` to
    // target the wrong branch. Devs set tracking explicitly via `git push -u`.
    let default_branch = mirror_default_br
        .ok_or_else(|| anyhow::anyhow!("cannot detect default branch from mirror"))?;
    let start_point = format!("origin/{}", default_branch);
    git::checkout_new_branch(&dest, branch, &start_point)?;

    Ok(())
}

fn parse_identity(identity: &str) -> Result<giturl::Parsed> {
    giturl::Parsed::from_identity(identity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// Sets up a test environment using tempdirs.
    /// Returns Paths, TempDirs (keep alive!), identity, and upstream URL map.
    fn setup_test_env() -> (
        Paths,
        tempfile::TempDir,
        tempfile::TempDir,
        String,
        BTreeMap<String, String>,
    ) {
        let tmp_data = tempfile::tempdir().unwrap();
        let tmp_home = tempfile::tempdir().unwrap();

        let data_dir = tmp_data.path().join("wsp");
        let workspaces_dir = tmp_home.path().join("dev").join("workspaces");
        fs::create_dir_all(&workspaces_dir).unwrap();

        let paths = Paths::from_dirs(&data_dir, &workspaces_dir);

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
        mirror::clone(
            &paths.mirrors_dir,
            &parsed,
            repo_dir.path().to_str().unwrap(),
        )
        .unwrap();

        // Set up HEAD ref so DefaultBranch works
        let mirror_dir = mirror::dir(&paths.mirrors_dir, &parsed);
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

        let identity = parsed.identity();
        let upstream_urls = BTreeMap::from([(
            identity.clone(),
            repo_dir.path().to_str().unwrap().to_string(),
        )]);

        (paths, tmp_data, repo_dir, identity, upstream_urls)
    }

    #[test]
    fn test_create_and_load_metadata() {
        let (paths, _d, _r, identity, upstream_urls) = setup_test_env();

        let refs = BTreeMap::from([(identity.clone(), String::new())]);
        create(&paths, "test-ws", &refs, None, &upstream_urls).unwrap();

        let ws_dir = dir(&paths.workspaces_dir, "test-ws");
        let meta = load_metadata(&ws_dir).unwrap();

        assert_eq!(meta.name, "test-ws");
        assert_eq!(meta.branch, "test-ws");
        assert!(meta.repos.contains_key(&identity));

        // Clone directory should exist and be a regular git repo
        let clone_dir = ws_dir.join("test-repo");
        assert!(clone_dir.exists());
        assert!(
            clone_dir.join(".git").is_dir(),
            ".git should be a directory, not a worktree file"
        );
    }

    #[test]
    fn test_active_repo_has_no_upstream_tracking() {
        let (paths, _d, _r, identity, upstream_urls) = setup_test_env();

        let refs = BTreeMap::from([(identity, String::new())]);
        create(&paths, "no-track", &refs, None, &upstream_urls).unwrap();

        let ws_dir = dir(&paths.workspaces_dir, "no-track");
        let clone_dir = ws_dir.join("test-repo");

        // Branch must have no upstream — a bare `git push` should not target origin/main
        let result = git::run(Some(&clone_dir), &["rev-parse", "--verify", "@{upstream}"]);
        assert!(
            result.is_err(),
            "workspace branch should have no upstream tracking"
        );
    }

    #[test]
    fn test_context_repo_default_branch_tracks_origin() {
        let (paths, _d, _r, identity, upstream_urls) = setup_test_env();

        // Context repo pinned to "main" — same as default branch
        let refs = BTreeMap::from([(identity, "main".into())]);
        create(&paths, "ctx-track", &refs, None, &upstream_urls).unwrap();

        let ws_dir = dir(&paths.workspaces_dir, "ctx-track");
        let clone_dir = ws_dir.join("test-repo");

        // main should track origin/main (the real upstream), not wsp-mirror/main
        let upstream = git::run(
            Some(&clone_dir),
            &[
                "for-each-ref",
                "--format=%(upstream:short)",
                "refs/heads/main",
            ],
        )
        .unwrap();
        assert_eq!(
            upstream, "origin/main",
            "context repo main should track origin/main, got {:?}",
            upstream
        );
    }

    #[test]
    fn test_default_branch_tracks_origin_not_mirror() {
        let (paths, _d, _r, identity, upstream_urls) = setup_test_env();

        let refs = BTreeMap::from([(identity, String::new())]);
        create(&paths, "track-origin", &refs, None, &upstream_urls).unwrap();

        let ws_dir = dir(&paths.workspaces_dir, "track-origin");
        let clone_dir = ws_dir.join("test-repo");

        // main should track origin/main, not wsp-mirror/main
        let upstream = git::run(
            Some(&clone_dir),
            &[
                "for-each-ref",
                "--format=%(upstream:short)",
                "refs/heads/main",
            ],
        )
        .unwrap();
        assert_eq!(
            upstream, "origin/main",
            "main should track origin/main, got {:?}",
            upstream
        );
    }

    #[test]
    fn test_create_with_branch_prefix() {
        let (paths, _d, _r, identity, upstream_urls) = setup_test_env();

        let refs = BTreeMap::from([(identity.clone(), String::new())]);
        create(&paths, "my-feature", &refs, Some("jganoff"), &upstream_urls).unwrap();

        let ws_dir = dir(&paths.workspaces_dir, "my-feature");
        let meta = load_metadata(&ws_dir).unwrap();

        assert_eq!(meta.name, "my-feature");
        assert_eq!(meta.branch, "jganoff/my-feature");
        assert!(meta.repos.contains_key(&identity));
        assert!(ws_dir.join("test-repo").exists());
    }

    #[test]
    fn test_create_with_empty_branch_prefix() {
        let (paths, _d, _r, identity, upstream_urls) = setup_test_env();

        let refs = BTreeMap::from([(identity.clone(), String::new())]);
        create(&paths, "empty-prefix", &refs, Some(""), &upstream_urls).unwrap();

        let ws_dir = dir(&paths.workspaces_dir, "empty-prefix");
        let meta = load_metadata(&ws_dir).unwrap();

        assert_eq!(meta.branch, "empty-prefix");
    }

    #[test]
    fn test_create_duplicate() {
        let (paths, _d, _r, identity, upstream_urls) = setup_test_env();

        let refs = BTreeMap::from([(identity.clone(), String::new())]);
        create(&paths, "test-ws-dup", &refs, None, &upstream_urls).unwrap();
        assert!(create(&paths, "test-ws-dup", &refs, None, &upstream_urls).is_err());
    }

    #[test]
    fn test_local_default_branch_matches_origin_after_create() {
        let (paths, _d, source_repo, identity, upstream_urls) = setup_test_env();

        // Add a commit to upstream after mirror was cloned, then fetch into mirror
        // so the mirror is ahead of what the initial bare clone had.
        let output = Command::new("git")
            .args(["commit", "--allow-empty", "-m", "upstream advance"])
            .current_dir(source_repo.path())
            .output()
            .unwrap();
        assert!(output.status.success());

        let parsed = giturl::Parsed::from_identity(&identity).unwrap();
        mirror::fetch(&paths.mirrors_dir, &parsed).unwrap();

        // Create workspace — local main should be fast-forwarded to origin/main
        let refs = BTreeMap::from([(identity, String::new())]);
        create(&paths, "ff-test", &refs, None, &upstream_urls).unwrap();

        let clone_dir = dir(&paths.workspaces_dir, "ff-test").join("test-repo");

        let local_main = git::run(Some(&clone_dir), &["rev-parse", "refs/heads/main"]).unwrap();
        let origin_main =
            git::run(Some(&clone_dir), &["rev-parse", "refs/remotes/origin/main"]).unwrap();

        assert_eq!(
            local_main, origin_main,
            "local main should match origin/main after create"
        );
    }

    #[test]
    fn test_detect() {
        let (paths, _d, _r, identity, upstream_urls) = setup_test_env();

        let refs = BTreeMap::from([(identity, String::new())]);
        create(&paths, "test-ws-detect", &refs, None, &upstream_urls).unwrap();

        let ws_dir = dir(&paths.workspaces_dir, "test-ws-detect");

        // From workspace root
        let found = detect(&ws_dir).unwrap();
        assert_eq!(found, ws_dir);

        // From a repo subdirectory
        let repo_dir = ws_dir.join("test-repo");
        let found = detect(&repo_dir).unwrap();
        assert_eq!(found, ws_dir);
    }

    #[test]
    fn test_detect_not_in_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(detect(tmp.path()).is_err());
    }

    #[test]
    fn test_remove_merged_workspace() {
        let (paths, _d, _r, identity, upstream_urls) = setup_test_env();

        let refs = BTreeMap::from([(identity.clone(), String::new())]);
        create(&paths, "rm-merged", &refs, None, &upstream_urls).unwrap();

        let ws_dir = dir(&paths.workspaces_dir, "rm-merged");
        assert!(ws_dir.exists());

        // Branch was created from main with no extra commits, so it's merged
        remove(&paths, "rm-merged", false, true).unwrap();
        assert!(!ws_dir.exists());
    }

    #[test]
    fn test_remove_merged_when_origin_ahead_of_local_main() {
        let (paths, _d, source_repo, identity, upstream_urls) = setup_test_env();

        let parsed = parse_identity(&identity).unwrap();
        let mirror_dir = mirror::dir(&paths.mirrors_dir, &parsed);

        // Advance the source repo so origin/main moves ahead
        let cmds: Vec<Vec<&str>> = vec![vec![
            "git",
            "commit",
            "--allow-empty",
            "-m",
            "upstream advance",
        ]];
        for args in &cmds {
            let output = Command::new(args[0])
                .args(&args[1..])
                .current_dir(source_repo.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "command {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        }

        // Fetch to update mirror
        git::fetch(&mirror_dir, true).unwrap();

        // Create workspace
        let refs = BTreeMap::from([(identity.clone(), String::new())]);
        create(&paths, "rm-origin-ahead", &refs, None, &upstream_urls).unwrap();

        let ws_dir = dir(&paths.workspaces_dir, "rm-origin-ahead");
        assert!(ws_dir.exists());

        // Remove should succeed — the workspace branch has no extra commits
        remove(&paths, "rm-origin-ahead", false, true).unwrap();
        assert!(!ws_dir.exists());
    }

    #[test]
    fn test_remove_blocks_unmerged_branch() {
        let (paths, _d, _r, identity, upstream_urls) = setup_test_env();

        let refs = BTreeMap::from([(identity.clone(), String::new())]);
        create(&paths, "rm-unmerged", &refs, None, &upstream_urls).unwrap();

        let ws_dir = dir(&paths.workspaces_dir, "rm-unmerged");
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

        let result = remove(&paths, "rm-unmerged", false, true);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unsaved work"),
            "expected 'unsaved work' in error: {}",
            err
        );

        // Workspace should still exist
        assert!(ws_dir.exists());
    }

    #[test]
    fn test_remove_force_deletes_unmerged() {
        let (paths, _d, _r, identity, upstream_urls) = setup_test_env();

        let refs = BTreeMap::from([(identity.clone(), String::new())]);
        create(&paths, "rm-force", &refs, None, &upstream_urls).unwrap();

        let ws_dir = dir(&paths.workspaces_dir, "rm-force");
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
        remove(&paths, "rm-force", true, true).unwrap();
        assert!(!ws_dir.exists());
    }

    #[test]
    fn test_remove_blocks_pending_changes() {
        let (paths, _d, _r, identity, upstream_urls) = setup_test_env();

        let refs = BTreeMap::from([(identity, String::new())]);
        create(&paths, "rm-dirty", &refs, None, &upstream_urls).unwrap();

        let ws_dir = dir(&paths.workspaces_dir, "rm-dirty");
        let repo_dir = ws_dir.join("test-repo");
        fs::write(repo_dir.join("dirty.txt"), "x").unwrap();

        let result = remove(&paths, "rm-dirty", false, true);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("pending changes"),
            "expected 'pending changes' in error: {}",
            err
        );
        assert!(ws_dir.exists());
    }

    #[test]
    fn test_list_all() {
        let (paths, _d, _r, identity, upstream_urls) = setup_test_env();

        // Initially empty
        let names = list_all(&paths.workspaces_dir).unwrap();
        assert!(names.is_empty());

        // Create a workspace
        let refs = BTreeMap::from([(identity, String::new())]);
        create(&paths, "ws-1-list", &refs, None, &upstream_urls).unwrap();

        let names = list_all(&paths.workspaces_dir).unwrap();
        assert_eq!(names, vec!["ws-1-list"]);
    }

    #[test]
    fn test_save_and_load_metadata_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let meta = Metadata {
            version: CURRENT_METADATA_VERSION,
            name: "my-ws".into(),
            branch: "my-ws".into(),
            repos: BTreeMap::from([
                ("github.com/user/repo-a".into(), None),
                ("github.com/user/repo-b".into(), None),
            ]),
            created: Utc::now(),
            dirs: BTreeMap::new(),
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
            version: CURRENT_METADATA_VERSION,
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
            dirs: BTreeMap::new(),
        };

        save_metadata(tmp.path(), &meta).unwrap();
        let loaded = load_metadata(tmp.path()).unwrap();

        assert_eq!(loaded.name, meta.name);
        assert_eq!(loaded.repos.len(), 3);
        assert!(loaded.repos["github.com/acme/api-gateway"].is_none());
        assert_eq!(
            loaded.repos["github.com/acme/user-service"]
                .as_ref()
                .unwrap()
                .r#ref,
            "main"
        );
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
            ("dash prefix", "-bad", true),
            ("double dash prefix", "--also-bad", true),
            ("dot", ".", true),
            ("dotdot", "..", true),
            ("dot prefix", ".hidden", true),
            ("dot prefix config", ".config", true),
            ("null byte", "bad\0name", true),
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
    fn test_validate_dir_name() {
        let cases = vec![
            ("valid simple", "repo-a", false),
            ("valid with owner prefix", "acme-utils", false),
            ("empty", "", true),
            ("forward slash", "a/b", true),
            ("backslash", "a\\b", true),
            ("null byte", "bad\0name", true),
            ("dotdot", "..", true),
            ("dot", ".", true),
            ("contains dotdot", "foo..bar", true),
            ("path traversal prefix", "../etc", true),
            ("absolute path", "/etc/passwd", true),
        ];
        for (name, input, want_err) in cases {
            let result = validate_dir_name(input);
            if want_err {
                assert!(result.is_err(), "{}: expected error", name);
            } else {
                assert!(result.is_ok(), "{}: unexpected error: {:?}", name, result);
            }
        }
    }

    #[test]
    fn test_load_metadata_rejects_traversal_in_dirs() {
        let cases = vec![
            ("path separator", "../../.ssh", "path separators"),
            ("dotdot", "..", "path traversal"),
        ];
        for (name, dir_val, expected_msg) in cases {
            let tmp = tempfile::tempdir().unwrap();
            let yaml = format!(
                "name: evil-ws\nbranch: evil-ws\nrepos:\n  github.com/acme/api:\ncreated: '2024-01-01T00:00:00Z'\ndirs:\n  github.com/acme/api: '{}'\n",
                dir_val
            );
            fs::write(tmp.path().join(METADATA_FILE), &yaml).unwrap();

            let result = load_metadata(tmp.path());
            assert!(result.is_err(), "{}: expected error", name);
            let err = result.unwrap_err().to_string();
            assert!(
                err.contains(expected_msg),
                "{}: expected {:?} in error: {}",
                name,
                expected_msg,
                err
            );
        }
    }

    #[test]
    fn test_create_cleans_up_on_failure() {
        let tmp_data = tempfile::tempdir().unwrap();
        let tmp_home = tempfile::tempdir().unwrap();

        let data_dir = tmp_data.path().join("wsp");
        let workspaces_dir = tmp_home.path().join("dev").join("workspaces");
        fs::create_dir_all(&workspaces_dir).unwrap();

        let paths = Paths::from_dirs(&data_dir, &workspaces_dir);

        // Try to create with a nonexistent repo identity — will fail
        let refs = BTreeMap::from([("nonexistent.local/user/nope".into(), String::new())]);
        let upstream_urls = BTreeMap::new();
        let result = create(&paths, "fail-ws", &refs, None, &upstream_urls);
        assert!(result.is_err());

        // Workspace dir should have been cleaned up
        let ws_dir = workspaces_dir.join("fail-ws");
        assert!(
            !ws_dir.exists(),
            "workspace dir should be cleaned up on failure"
        );
    }

    #[test]
    fn test_create_with_context_repo() {
        let (paths, _d, _r, identity, upstream_urls) = setup_test_env();

        // Create workspace with the repo as context (ref = "main")
        let refs = BTreeMap::from([(identity.clone(), "main".into())]);
        create(&paths, "ctx-ws", &refs, None, &upstream_urls).unwrap();

        let ws_dir = dir(&paths.workspaces_dir, "ctx-ws");
        let meta = load_metadata(&ws_dir).unwrap();

        assert!(meta.repos[&identity].is_some());
        assert_eq!(meta.repos[&identity].as_ref().unwrap().r#ref, "main");
        assert!(ws_dir.join("test-repo").exists());
    }

    #[test]
    fn test_add_repos_to_existing_workspace() {
        let (paths, _d, _r, identity, upstream_urls) = setup_test_env();

        // Create workspace with active repo
        let refs = BTreeMap::from([(identity.clone(), String::new())]);
        create(&paths, "add-ws", &refs, None, &upstream_urls).unwrap();

        let ws_dir = dir(&paths.workspaces_dir, "add-ws");

        // Try adding the same repo again — should skip
        add_repos(&paths.mirrors_dir, &ws_dir, &refs, &upstream_urls).unwrap();

        let meta = load_metadata(&ws_dir).unwrap();
        assert_eq!(meta.repos.len(), 1);
    }

    #[test]
    fn test_add_repo_has_no_upstream_tracking() {
        let (paths, _d, source_repo, identity1, mut upstream_urls) = setup_test_env();

        let refs = BTreeMap::from([(identity1, String::new())]);
        create(&paths, "add-no-track", &refs, None, &upstream_urls).unwrap();

        let ws_dir = dir(&paths.workspaces_dir, "add-no-track");

        // Add a second repo via add_repos
        let (identity2, urls2) = add_mirror_with_owner(
            &paths,
            source_repo.path(),
            "test.local",
            "other",
            "added-repo",
        );
        upstream_urls.extend(urls2);

        let add_refs = BTreeMap::from([(identity2, String::new())]);
        add_repos(&paths.mirrors_dir, &ws_dir, &add_refs, &upstream_urls).unwrap();

        let clone_dir = ws_dir.join("added-repo");
        let result = git::run(Some(&clone_dir), &["rev-parse", "--verify", "@{upstream}"]);
        assert!(
            result.is_err(),
            "repo added via add_repos should have no upstream tracking"
        );
    }

    #[test]
    fn test_remove_context_repo() {
        let (paths, _d, _r, identity, upstream_urls) = setup_test_env();

        // Create workspace with context repo (pinned to "main")
        let refs = BTreeMap::from([(identity, "main".into())]);
        create(&paths, "rm-ws-ctx", &refs, None, &upstream_urls).unwrap();

        // Remove should succeed without touching context repo branches
        remove(&paths, "rm-ws-ctx", false, true).unwrap();
    }

    /// Creates a second mirror with a different owner but same repo name.
    /// Returns (identity, upstream_urls entry).
    fn add_mirror_with_owner(
        paths: &Paths,
        source_repo: &Path,
        host: &str,
        owner: &str,
        repo: &str,
    ) -> (String, BTreeMap<String, String>) {
        let parsed = giturl::Parsed {
            host: host.into(),
            owner: owner.into(),
            repo: repo.into(),
        };
        mirror::clone(&paths.mirrors_dir, &parsed, source_repo.to_str().unwrap()).unwrap();

        let mirror_dir = mirror::dir(&paths.mirrors_dir, &parsed);
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

        let id = parsed.identity();
        let urls = BTreeMap::from([(id.clone(), source_repo.to_str().unwrap().to_string())]);
        (id, urls)
    }

    #[test]
    fn test_compute_dir_names_no_collision() {
        let ids = vec!["github.com/acme/api", "github.com/acme/web"];
        let dirs = compute_dir_names(&ids).unwrap();
        assert!(dirs.is_empty(), "no collision means empty map");
    }

    #[test]
    fn test_compute_dir_names_with_collision() {
        let ids = vec!["github.com/acme/utils", "github.com/other/utils"];
        let dirs = compute_dir_names(&ids).unwrap();
        assert_eq!(dirs.len(), 2);
        assert_eq!(dirs["github.com/acme/utils"], "acme-utils");
        assert_eq!(dirs["github.com/other/utils"], "other-utils");
    }

    #[test]
    fn test_compute_dir_names_nested_owner() {
        let ids = vec!["gitlab.com/org/sub/utils", "gitlab.com/other/utils"];
        let dirs = compute_dir_names(&ids).unwrap();
        assert_eq!(dirs.len(), 2);
        assert_eq!(dirs["gitlab.com/org/sub/utils"], "org-sub-utils");
        assert_eq!(dirs["gitlab.com/other/utils"], "other-utils");
    }

    #[test]
    fn test_dir_name_with_override() {
        let meta = Metadata {
            version: CURRENT_METADATA_VERSION,
            name: "test".into(),
            branch: "test".into(),
            repos: BTreeMap::from([("github.com/acme/utils".into(), None)]),
            created: Utc::now(),
            dirs: BTreeMap::from([("github.com/acme/utils".into(), "acme-utils".into())]),
        };
        assert_eq!(
            meta.dir_name("github.com/acme/utils").unwrap(),
            "acme-utils"
        );
    }

    #[test]
    fn test_dir_name_without_override() {
        let meta = Metadata {
            version: CURRENT_METADATA_VERSION,
            name: "test".into(),
            branch: "test".into(),
            repos: BTreeMap::from([("github.com/acme/utils".into(), None)]),
            created: Utc::now(),
            dirs: BTreeMap::new(),
        };
        assert_eq!(meta.dir_name("github.com/acme/utils").unwrap(), "utils");
    }

    #[test]
    fn test_backward_compat_no_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let yaml = "name: old-ws\nbranch: old-ws\nrepos:\n  github.com/acme/api:\ncreated: '2024-01-01T00:00:00Z'\n";
        fs::write(tmp.path().join(METADATA_FILE), yaml).unwrap();

        let meta = load_metadata(tmp.path()).unwrap();
        assert_eq!(meta.name, "old-ws");
        assert!(meta.dirs.is_empty());
        assert_eq!(meta.dir_name("github.com/acme/api").unwrap(), "api");
    }

    #[test]
    fn test_create_with_colliding_repo_names() {
        let (paths, _d, source_repo, identity1, mut upstream_urls) = setup_test_env();

        let (identity2, urls2) = add_mirror_with_owner(
            &paths,
            source_repo.path(),
            "test.local",
            "other",
            "test-repo",
        );
        upstream_urls.extend(urls2);

        let refs = BTreeMap::from([
            (identity1.clone(), String::new()),
            (identity2.clone(), String::new()),
        ]);
        create(&paths, "collide-ws", &refs, None, &upstream_urls).unwrap();

        let ws_dir = dir(&paths.workspaces_dir, "collide-ws");
        let meta = load_metadata(&ws_dir).unwrap();

        assert_eq!(meta.dir_name(&identity1).unwrap(), "user-test-repo");
        assert_eq!(meta.dir_name(&identity2).unwrap(), "other-test-repo");
        assert!(ws_dir.join("user-test-repo").exists());
        assert!(ws_dir.join("other-test-repo").exists());
    }

    #[test]
    fn test_add_repo_causing_collision() {
        let (paths, _d, source_repo, identity1, upstream_urls) = setup_test_env();

        let refs = BTreeMap::from([(identity1.clone(), String::new())]);
        create(&paths, "add-collide", &refs, None, &upstream_urls).unwrap();

        let ws_dir = dir(&paths.workspaces_dir, "add-collide");
        assert!(ws_dir.join("test-repo").exists());

        let (identity2, urls2) = add_mirror_with_owner(
            &paths,
            source_repo.path(),
            "test.local",
            "other",
            "test-repo",
        );
        let new_refs = BTreeMap::from([(identity2.clone(), String::new())]);
        add_repos(&paths.mirrors_dir, &ws_dir, &new_refs, &urls2).unwrap();

        let meta = load_metadata(&ws_dir).unwrap();
        assert_eq!(meta.dir_name(&identity1).unwrap(), "user-test-repo");
        assert_eq!(meta.dir_name(&identity2).unwrap(), "other-test-repo");
        assert!(!ws_dir.join("test-repo").exists());
        assert!(ws_dir.join("user-test-repo").exists());
        assert!(ws_dir.join("other-test-repo").exists());
    }

    #[test]
    fn test_add_repos_intra_batch_collision() {
        let (paths, _d, source_repo, identity1, mut upstream_urls) = setup_test_env();

        // Create workspace with no repos
        let refs = BTreeMap::new();
        create(&paths, "batch-collide", &refs, None, &upstream_urls).unwrap();
        let ws_dir = dir(&paths.workspaces_dir, "batch-collide");

        // Add two repos with the same short name ("test-repo") in one batch
        let (identity2, urls2) = add_mirror_with_owner(
            &paths,
            source_repo.path(),
            "test.local",
            "other",
            "test-repo",
        );
        upstream_urls.extend(urls2.clone());

        let new_refs = BTreeMap::from([
            (identity1.clone(), String::new()),
            (identity2.clone(), String::new()),
        ]);
        let mut all_urls = upstream_urls.clone();
        all_urls.extend(urls2);
        add_repos(&paths.mirrors_dir, &ws_dir, &new_refs, &all_urls).unwrap();

        let meta = load_metadata(&ws_dir).unwrap();
        assert_eq!(meta.dir_name(&identity1).unwrap(), "user-test-repo");
        assert_eq!(meta.dir_name(&identity2).unwrap(), "other-test-repo");
        assert!(ws_dir.join("user-test-repo").exists());
        assert!(ws_dir.join("other-test-repo").exists());
        // Short name should not exist — both are disambiguated
        assert!(!ws_dir.join("test-repo").exists());
    }

    #[test]
    fn test_remove_repos_basic() {
        let (paths, _d, source_repo, identity1, mut upstream_urls) = setup_test_env();

        let (identity2, urls2) = add_mirror_with_owner(
            &paths,
            source_repo.path(),
            "test.local",
            "other",
            "other-repo",
        );
        upstream_urls.extend(urls2);

        let refs = BTreeMap::from([
            (identity1.clone(), String::new()),
            (identity2.clone(), String::new()),
        ]);
        create(&paths, "rm-repo-ws", &refs, None, &upstream_urls).unwrap();

        let ws_dir = dir(&paths.workspaces_dir, "rm-repo-ws");
        assert!(ws_dir.join("test-repo").exists());
        assert!(ws_dir.join("other-repo").exists());

        remove_repos(&paths.mirrors_dir, &ws_dir, &[identity2.clone()], false).unwrap();

        let meta = load_metadata(&ws_dir).unwrap();
        assert_eq!(meta.repos.len(), 1);
        assert!(meta.repos.contains_key(&identity1));
        assert!(!meta.repos.contains_key(&identity2));
        assert!(ws_dir.join("test-repo").exists());
        assert!(!ws_dir.join("other-repo").exists());
    }

    #[test]
    fn test_remove_repos_not_in_workspace() {
        let (paths, _d, _r, identity, upstream_urls) = setup_test_env();

        let refs = BTreeMap::from([(identity, String::new())]);
        create(&paths, "rm-repo-nf", &refs, None, &upstream_urls).unwrap();

        let ws_dir = dir(&paths.workspaces_dir, "rm-repo-nf");
        let result = remove_repos(
            &paths.mirrors_dir,
            &ws_dir,
            &["test.local/nobody/fake".to_string()],
            false,
        );
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("not in this workspace")
        );
    }

    #[test]
    fn test_remove_repos_blocks_pending_changes() {
        let (paths, _d, _r, identity, upstream_urls) = setup_test_env();

        let refs = BTreeMap::from([(identity.clone(), String::new())]);
        create(&paths, "rm-repo-dirty", &refs, None, &upstream_urls).unwrap();

        let ws_dir = dir(&paths.workspaces_dir, "rm-repo-dirty");
        let repo_dir = ws_dir.join("test-repo");
        fs::write(repo_dir.join("dirty.txt"), "x").unwrap();

        let result = remove_repos(&paths.mirrors_dir, &ws_dir, &[identity.clone()], false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("pending changes"));
    }

    #[test]
    fn test_remove_repos_force_with_pending_changes() {
        let (paths, _d, _r, identity, upstream_urls) = setup_test_env();

        let refs = BTreeMap::from([(identity.clone(), String::new())]);
        create(&paths, "rm-repo-force", &refs, None, &upstream_urls).unwrap();

        let ws_dir = dir(&paths.workspaces_dir, "rm-repo-force");
        let repo_dir = ws_dir.join("test-repo");
        fs::write(repo_dir.join("dirty.txt"), "x").unwrap();

        remove_repos(&paths.mirrors_dir, &ws_dir, &[identity.clone()], true).unwrap();

        let meta = load_metadata(&ws_dir).unwrap();
        assert!(meta.repos.is_empty());
        assert!(!ws_dir.join("test-repo").exists());
    }

    #[test]
    fn test_remove_repos_undoes_collision() {
        let (paths, _d, source_repo, identity1, mut upstream_urls) = setup_test_env();

        let (identity2, urls2) = add_mirror_with_owner(
            &paths,
            source_repo.path(),
            "test.local",
            "other",
            "test-repo",
        );
        upstream_urls.extend(urls2);

        let refs = BTreeMap::from([
            (identity1.clone(), String::new()),
            (identity2.clone(), String::new()),
        ]);
        create(&paths, "rm-repo-col", &refs, None, &upstream_urls).unwrap();

        let ws_dir = dir(&paths.workspaces_dir, "rm-repo-col");
        assert!(ws_dir.join("user-test-repo").exists());
        assert!(ws_dir.join("other-test-repo").exists());

        remove_repos(&paths.mirrors_dir, &ws_dir, &[identity2.clone()], false).unwrap();

        let meta = load_metadata(&ws_dir).unwrap();
        assert_eq!(meta.repos.len(), 1);
        assert!(meta.dirs.is_empty(), "no collisions, dirs should be empty");
        assert_eq!(meta.dir_name(&identity1).unwrap(), "test-repo");
        assert!(ws_dir.join("test-repo").exists());
        assert!(!ws_dir.join("user-test-repo").exists());
        assert!(!ws_dir.join("other-test-repo").exists());
    }

    #[test]
    fn test_remove_repos_context_repo() {
        let (paths, _d, _r, identity, upstream_urls) = setup_test_env();

        let refs = BTreeMap::from([(identity.clone(), "main".into())]);
        create(&paths, "rm-repo-ctx", &refs, None, &upstream_urls).unwrap();

        let ws_dir = dir(&paths.workspaces_dir, "rm-repo-ctx");
        remove_repos(&paths.mirrors_dir, &ws_dir, &[identity.clone()], false).unwrap();

        let meta = load_metadata(&ws_dir).unwrap();
        assert!(meta.repos.is_empty());
    }

    /// Helper: squash-merge a branch into target in the source repo.
    fn squash_merge_branch(dir: &Path, branch: &str, target: &str) {
        for args in &[
            vec!["git", "checkout", target],
            vec!["git", "merge", "--squash", branch],
            vec!["git", "commit", "-m", &format!("squash-merge {}", branch)],
        ] {
            let output = Command::new(args[0])
                .args(&args[1..])
                .current_dir(dir)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{:?}: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    /// Helper: commit a file, push to origin, fetch, and set up tracking in a clone.
    fn commit_push_and_track(repo_dir: &Path, branch: &str, file: &str, content: &str) {
        for args in &[
            vec!["git", "config", "user.email", "test@test.com"],
            vec!["git", "config", "user.name", "Test"],
            vec!["git", "config", "commit.gpgsign", "false"],
        ] {
            let output = Command::new(args[0])
                .args(&args[1..])
                .current_dir(repo_dir)
                .output()
                .unwrap();
            assert!(output.status.success());
        }
        fs::write(repo_dir.join(file), content).unwrap();
        let output = Command::new("git")
            .args(["add", file])
            .current_dir(repo_dir)
            .output()
            .unwrap();
        assert!(output.status.success());
        let output = Command::new("git")
            .args(["commit", "-m", &format!("add {}", file)])
            .current_dir(repo_dir)
            .output()
            .unwrap();
        assert!(output.status.success());

        // Push to origin (source repo)
        let output = Command::new("git")
            .args(["push", "origin", branch])
            .current_dir(repo_dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "push: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        // Fetch so origin/<branch> appears locally
        let output = Command::new("git")
            .args(["fetch", "origin"])
            .current_dir(repo_dir)
            .output()
            .unwrap();
        assert!(output.status.success());

        // Set tracking so ahead_count returns 0
        let upstream = format!("origin/{}", branch);
        let output = Command::new("git")
            .args(["branch", "--set-upstream-to", &upstream])
            .current_dir(repo_dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "set-upstream: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn test_remove_allows_squash_merged_branch() {
        let (paths, _d, source_repo, identity, upstream_urls) = setup_test_env();

        let refs = BTreeMap::from([(identity.clone(), String::new())]);
        create(&paths, "rm-squash", &refs, None, &upstream_urls).unwrap();

        let ws_dir = dir(&paths.workspaces_dir, "rm-squash");
        let repo_dir = ws_dir.join("test-repo");

        commit_push_and_track(&repo_dir, "rm-squash", "feat.txt", "feature");
        squash_merge_branch(source_repo.path(), "rm-squash", "main");

        // Remove should succeed without --force since branch is squash-merged
        remove(&paths, "rm-squash", false, true).unwrap();
        assert!(!ws_dir.exists());
    }

    #[test]
    fn test_remove_blocks_pushed_but_unmerged() {
        let (paths, _d, _source_repo, identity, upstream_urls) = setup_test_env();

        let refs = BTreeMap::from([(identity.clone(), String::new())]);
        create(&paths, "rm-pushed", &refs, None, &upstream_urls).unwrap();

        let ws_dir = dir(&paths.workspaces_dir, "rm-pushed");
        let repo_dir = ws_dir.join("test-repo");

        commit_push_and_track(&repo_dir, "rm-pushed", "wip.txt", "wip");

        let result = remove(&paths, "rm-pushed", false, true);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("pushed to remote"),
            "expected 'pushed to remote' in error: {}",
            err
        );
        assert!(ws_dir.exists());
    }

    #[test]
    fn test_remove_repos_allows_squash_merged() {
        let (paths, _d, source_repo, identity, upstream_urls) = setup_test_env();

        let refs = BTreeMap::from([(identity.clone(), String::new())]);
        create(&paths, "rmr-squash", &refs, None, &upstream_urls).unwrap();

        let ws_dir = dir(&paths.workspaces_dir, "rmr-squash");
        let repo_dir = ws_dir.join("test-repo");

        commit_push_and_track(&repo_dir, "rmr-squash", "feat.txt", "feature");
        squash_merge_branch(source_repo.path(), "rmr-squash", "main");

        remove_repos(&paths.mirrors_dir, &ws_dir, &[identity.clone()], false).unwrap();
        let meta = load_metadata(&ws_dir).unwrap();
        assert!(meta.repos.is_empty());
    }

    #[test]
    fn test_remove_repos_blocks_pushed_but_unmerged() {
        let (paths, _d, _r, identity, upstream_urls) = setup_test_env();

        let refs = BTreeMap::from([(identity.clone(), String::new())]);
        create(&paths, "rmr-pushed", &refs, None, &upstream_urls).unwrap();

        let ws_dir = dir(&paths.workspaces_dir, "rmr-pushed");
        let repo_dir = ws_dir.join("test-repo");

        commit_push_and_track(&repo_dir, "rmr-pushed", "wip.txt", "wip");

        let result = remove_repos(&paths.mirrors_dir, &ws_dir, &[identity.clone()], false);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("pushed to remote"),
            "expected 'pushed to remote' in error: {}",
            err
        );
    }

    #[test]
    fn test_clone_has_only_origin() {
        let (paths, _d, _r, identity, upstream_urls) = setup_test_env();

        let refs = BTreeMap::from([(identity.clone(), String::new())]);
        create(&paths, "only-origin", &refs, None, &upstream_urls).unwrap();

        let ws_dir = dir(&paths.workspaces_dir, "only-origin");
        let clone_dir = ws_dir.join("test-repo");

        // Verify only origin exists, no wsp-mirror
        let remotes = git::run(Some(&clone_dir), &["remote"]).unwrap();
        assert!(remotes.contains("origin"), "should have origin remote");
        assert!(
            !remotes.contains("wsp-mirror"),
            "should not have wsp-mirror remote"
        );

        // origin should point to source repo (upstream URL)
        let origin_url = git::run(Some(&clone_dir), &["remote", "get-url", "origin"]).unwrap();
        assert_eq!(origin_url, upstream_urls[&identity]);
    }

    #[test]
    fn test_remove_does_not_touch_mirror_branches() {
        let (paths, _d, _r, identity, upstream_urls) = setup_test_env();

        let refs = BTreeMap::from([(identity.clone(), String::new())]);
        create(&paths, "rm-no-mirror", &refs, None, &upstream_urls).unwrap();

        // The workspace branch should NOT exist in the mirror (clones are independent)
        let parsed = parse_identity(&identity).unwrap();
        let mirror_dir = mirror::dir(&paths.mirrors_dir, &parsed);

        remove(&paths, "rm-no-mirror", false, true).unwrap();

        // Mirror should still exist and be intact
        assert!(mirror_dir.exists());
    }

    #[test]
    fn test_propagate_mirror_to_clones() {
        let (paths, _d, source_repo, identity, upstream_urls) = setup_test_env();

        let refs = BTreeMap::from([(identity.clone(), String::new())]);
        create(&paths, "prop-ws", &refs, None, &upstream_urls).unwrap();

        let ws_dir = dir(&paths.workspaces_dir, "prop-ws");
        let clone_dir = ws_dir.join("test-repo");

        // Add a commit to source repo on main
        let cmds: Vec<Vec<&str>> = vec![
            vec!["git", "checkout", "main"],
            vec![
                "git",
                "commit",
                "--allow-empty",
                "-m",
                "new upstream commit",
            ],
        ];
        for args in &cmds {
            let output = Command::new(args[0])
                .args(&args[1..])
                .current_dir(source_repo.path())
                .output()
                .unwrap();
            assert!(output.status.success());
        }

        // Fetch mirror to pick up the new commit
        let parsed = parse_identity(&identity).unwrap();
        let mirror_dir = mirror::dir(&paths.mirrors_dir, &parsed);
        git::fetch(&mirror_dir, true).unwrap();

        // Get the new commit sha from mirror
        let mirror_sha = git::run(Some(&mirror_dir), &["rev-parse", "origin/main"]).unwrap();

        // Before propagation, clone doesn't have the new commit at origin/main
        let clone_sha_before = git::run(Some(&clone_dir), &["rev-parse", "origin/main"]).unwrap();
        assert_ne!(clone_sha_before, mirror_sha);

        // Propagate
        let meta = load_metadata(&ws_dir).unwrap();
        propagate_mirror_to_clones(&paths.mirrors_dir, &ws_dir, &meta, false);

        // After propagation, clone should have the new commit at origin/main
        let clone_sha_after = git::run(Some(&clone_dir), &["rev-parse", "origin/main"]).unwrap();
        assert_eq!(clone_sha_after, mirror_sha);
    }

    #[test]
    fn test_propagate_removes_legacy_wsp_mirror() {
        let (paths, _d, _r, identity, upstream_urls) = setup_test_env();

        let refs = BTreeMap::from([(identity.clone(), String::new())]);
        create(&paths, "prop-legacy", &refs, None, &upstream_urls).unwrap();

        let ws_dir = dir(&paths.workspaces_dir, "prop-legacy");
        let clone_dir = ws_dir.join("test-repo");

        // Manually add a wsp-mirror remote to simulate a legacy clone
        let parsed = parse_identity(&identity).unwrap();
        let mirror_dir = mirror::dir(&paths.mirrors_dir, &parsed);
        git::run(
            Some(&clone_dir),
            &["remote", "add", "wsp-mirror", mirror_dir.to_str().unwrap()],
        )
        .unwrap();
        assert!(
            git::has_remote(&clone_dir, "wsp-mirror"),
            "wsp-mirror should exist before propagate"
        );

        // Propagate
        let meta = load_metadata(&ws_dir).unwrap();
        propagate_mirror_to_clones(&paths.mirrors_dir, &ws_dir, &meta, false);

        // wsp-mirror should have been removed
        assert!(
            !git::has_remote(&clone_dir, "wsp-mirror"),
            "wsp-mirror should be removed after propagate"
        );
    }

    #[test]
    fn test_propagate_with_prune_removes_deleted_branches() {
        let (paths, _d, source_repo, identity, upstream_urls) = setup_test_env();

        let refs = BTreeMap::from([(identity.clone(), String::new())]);
        create(&paths, "prop-prune", &refs, None, &upstream_urls).unwrap();

        let ws_dir = dir(&paths.workspaces_dir, "prop-prune");
        let clone_dir = ws_dir.join("test-repo");
        let parsed = parse_identity(&identity).unwrap();
        let mirror_dir = mirror::dir(&paths.mirrors_dir, &parsed);

        // Create a branch in source, fetch into mirror, propagate to clone
        let output = Command::new("git")
            .args(["checkout", "-b", "feature-x"])
            .current_dir(source_repo.path())
            .output()
            .unwrap();
        assert!(output.status.success());
        let output = Command::new("git")
            .args(["commit", "--allow-empty", "-m", "feature commit"])
            .current_dir(source_repo.path())
            .output()
            .unwrap();
        assert!(output.status.success());

        git::fetch(&mirror_dir, true).unwrap();
        let meta = load_metadata(&ws_dir).unwrap();
        propagate_mirror_to_clones(&paths.mirrors_dir, &ws_dir, &meta, false);

        // Clone should now see origin/feature-x
        assert!(
            git::ref_exists(&clone_dir, "refs/remotes/origin/feature-x"),
            "origin/feature-x should exist after propagation"
        );

        // Delete the branch in source and re-fetch mirror (mirror auto-prunes)
        let output = Command::new("git")
            .args(["checkout", "main"])
            .current_dir(source_repo.path())
            .output()
            .unwrap();
        assert!(output.status.success());
        let output = Command::new("git")
            .args(["branch", "-D", "feature-x"])
            .current_dir(source_repo.path())
            .output()
            .unwrap();
        assert!(output.status.success());

        git::fetch(&mirror_dir, true).unwrap();

        // Propagate with prune=true — should remove stale origin/feature-x
        propagate_mirror_to_clones(&paths.mirrors_dir, &ws_dir, &meta, true);

        assert!(
            !git::ref_exists(&clone_dir, "refs/remotes/origin/feature-x"),
            "origin/feature-x should be pruned after propagation with prune=true"
        );
    }

    #[test]
    fn test_clone_has_origin_remote_refs() {
        let (paths, _d, _r, identity, upstream_urls) = setup_test_env();

        let refs = BTreeMap::from([(identity.clone(), String::new())]);
        create(&paths, "origin-refs", &refs, None, &upstream_urls).unwrap();

        let ws_dir = dir(&paths.workspaces_dir, "origin-refs");
        let clone_dir = ws_dir.join("test-repo");

        // origin/main should exist after clone setup
        assert!(
            git::ref_exists(&clone_dir, "refs/remotes/origin/main"),
            "origin/main should exist after ws new"
        );
    }

    #[test]
    fn test_remove_detects_diverged_squash_merge() {
        let (paths, _d, source_repo, identity, upstream_urls) = setup_test_env();

        let refs = BTreeMap::from([(identity.clone(), String::new())]);
        create(&paths, "rm-div-squash", &refs, None, &upstream_urls).unwrap();

        let ws_dir = dir(&paths.workspaces_dir, "rm-div-squash");
        let repo_dir = ws_dir.join("test-repo");

        // Commit and push on the workspace branch
        commit_push_and_track(&repo_dir, "rm-div-squash", "feat.txt", "feature content");

        // Add diverging commits to main on the source repo (different files)
        let out = Command::new("git")
            .args(["checkout", "main"])
            .current_dir(source_repo.path())
            .output()
            .unwrap();
        assert!(out.status.success());
        for args in &[
            vec!["git", "config", "user.email", "test@test.com"],
            vec!["git", "config", "user.name", "Test"],
            vec!["git", "config", "commit.gpgsign", "false"],
        ] {
            let out = Command::new(args[0])
                .args(&args[1..])
                .current_dir(source_repo.path())
                .output()
                .unwrap();
            assert!(out.status.success());
        }
        fs::write(source_repo.path().join("diverge.txt"), "diverge").unwrap();
        for args in &[
            vec!["git", "add", "diverge.txt"],
            vec!["git", "commit", "-m", "diverge main"],
        ] {
            let out = Command::new(args[0])
                .args(&args[1..])
                .current_dir(source_repo.path())
                .output()
                .unwrap();
            assert!(out.status.success());
        }

        // Squash-merge the branch into main on the source repo
        squash_merge_branch(source_repo.path(), "rm-div-squash", "main");

        // Delete the remote branch on the source repo
        let out = Command::new("git")
            .args(["branch", "-D", "rm-div-squash"])
            .current_dir(source_repo.path())
            .output()
            .unwrap();
        assert!(out.status.success());

        // Remove should succeed without --force
        remove(&paths, "rm-div-squash", false, true).unwrap();
        assert!(!ws_dir.exists());
    }

    #[test]
    fn test_metadata_version_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let meta = Metadata {
            version: CURRENT_METADATA_VERSION,
            name: "my-ws".into(),
            branch: "my-ws".into(),
            repos: BTreeMap::from([("github.com/user/repo-a".into(), None)]),
            created: Utc::now(),
            dirs: BTreeMap::new(),
        };

        save_metadata(tmp.path(), &meta).unwrap();

        // version 0 should be omitted from YAML via skip_serializing_if
        let yaml = fs::read_to_string(tmp.path().join(METADATA_FILE)).unwrap();
        assert!(
            !yaml.contains("version"),
            "version 0 should be omitted from YAML"
        );

        let loaded = load_metadata(tmp.path()).unwrap();
        assert_eq!(loaded.version, 0);
    }

    #[test]
    fn test_metadata_backward_compat_no_version() {
        let tmp = tempfile::tempdir().unwrap();
        let yaml = "name: old-ws\nbranch: old-ws\nrepos:\n  github.com/acme/api:\ncreated: '2024-01-01T00:00:00Z'\n";
        fs::write(tmp.path().join(METADATA_FILE), yaml).unwrap();

        let meta = load_metadata(tmp.path()).unwrap();
        assert_eq!(meta.version, 0);
    }

    #[test]
    fn test_metadata_unknown_version_loads() {
        let tmp = tempfile::tempdir().unwrap();
        let yaml = "version: 99\nname: future-ws\nbranch: future-ws\nrepos:\n  github.com/acme/api:\ncreated: '2024-01-01T00:00:00Z'\n";
        fs::write(tmp.path().join(METADATA_FILE), yaml).unwrap();

        let meta = load_metadata(tmp.path()).unwrap();
        assert_eq!(meta.version, 99);
        assert_eq!(meta.name, "future-ws");
    }

    // --- Root content detection tests ---

    fn make_simple_metadata(repos: &[&str]) -> Metadata {
        let mut map = BTreeMap::new();
        for id in repos {
            map.insert(id.to_string(), None);
        }
        Metadata {
            version: CURRENT_METADATA_VERSION,
            name: "test".into(),
            branch: "test".into(),
            repos: map,
            created: Utc::now(),
            dirs: BTreeMap::new(),
        }
    }

    #[test]
    fn test_check_root_content() {
        use std::os::unix::fs::symlink;

        struct Case {
            name: &'static str,
            setup: Box<dyn Fn(&Path)>,
            repos: Vec<&'static str>,
            want_clean: bool,
            want_contains: Vec<&'static str>,
        }

        let cases: Vec<Case> = vec![
            Case {
                name: "clean workspace — only repo dirs + .wsp.yaml",
                setup: Box::new(|ws| {
                    fs::create_dir_all(ws.join("api-gateway")).unwrap();
                    fs::write(ws.join(METADATA_FILE), "").unwrap();
                }),
                repos: vec!["github.com/acme/api-gateway"],
                want_clean: true,
                want_contains: vec![],
            },
            Case {
                name: "user file at root",
                setup: Box::new(|ws| {
                    fs::write(ws.join(METADATA_FILE), "").unwrap();
                    fs::write(ws.join("notes.md"), "my notes").unwrap();
                }),
                repos: vec![],
                want_clean: false,
                want_contains: vec!["?? notes.md"],
            },
            Case {
                name: "user directory at root",
                setup: Box::new(|ws| {
                    fs::write(ws.join(METADATA_FILE), "").unwrap();
                    fs::create_dir_all(ws.join("my-stuff")).unwrap();
                }),
                repos: vec![],
                want_clean: false,
                want_contains: vec!["?? my-stuff/"],
            },
            Case {
                name: "AGENTS.md with only scaffold + markers",
                setup: Box::new(|ws| {
                    fs::write(ws.join(METADATA_FILE), "").unwrap();
                    fs::write(
                        ws.join("AGENTS.md"),
                        "# Workspace: test\n\n<!-- Add your project-specific notes for AI agents here -->\n\n<!-- wsp:begin -->\nstuff\n<!-- wsp:end -->\n",
                    )
                    .unwrap();
                }),
                repos: vec![],
                want_clean: true,
                want_contains: vec![],
            },
            Case {
                name: "AGENTS.md with user notes before markers",
                setup: Box::new(|ws| {
                    fs::write(ws.join(METADATA_FILE), "").unwrap();
                    fs::write(
                        ws.join("AGENTS.md"),
                        "# Workspace: test\n\n## My Custom Notes\n\nImportant context here.\n\n<!-- wsp:begin -->\nstuff\n<!-- wsp:end -->\n",
                    )
                    .unwrap();
                }),
                repos: vec![],
                want_clean: false,
                want_contains: vec![" M AGENTS.md (user-added content)"],
            },
            Case {
                name: "AGENTS.md with missing markers",
                setup: Box::new(|ws| {
                    fs::write(ws.join(METADATA_FILE), "").unwrap();
                    fs::write(ws.join("AGENTS.md"), "# Some random content\n").unwrap();
                }),
                repos: vec![],
                want_clean: false,
                want_contains: vec![" M AGENTS.md (wsp markers missing)"],
            },
            Case {
                name: "CLAUDE.md as symlink to AGENTS.md",
                setup: Box::new(|ws| {
                    fs::write(ws.join(METADATA_FILE), "").unwrap();
                    fs::write(
                        ws.join("AGENTS.md"),
                        "# Workspace: test\n\n<!-- wsp:begin -->\n<!-- wsp:end -->\n",
                    )
                    .unwrap();
                    symlink("AGENTS.md", ws.join("CLAUDE.md")).unwrap();
                }),
                repos: vec![],
                want_clean: true,
                want_contains: vec![],
            },
            Case {
                name: "CLAUDE.md as regular file",
                setup: Box::new(|ws| {
                    fs::write(ws.join(METADATA_FILE), "").unwrap();
                    fs::write(
                        ws.join("AGENTS.md"),
                        "# Workspace: test\n\n<!-- wsp:begin -->\n<!-- wsp:end -->\n",
                    )
                    .unwrap();
                    fs::write(ws.join("CLAUDE.md"), "custom content").unwrap();
                }),
                repos: vec![],
                want_clean: false,
                want_contains: vec!["?? CLAUDE.md"],
            },
            Case {
                name: ".claude/ with only managed skills",
                setup: Box::new(|ws| {
                    fs::write(ws.join(METADATA_FILE), "").unwrap();
                    for name in &["wsp-manage", "wsp-report"] {
                        let skill_dir = ws.join(format!(".claude/skills/{}", name));
                        fs::create_dir_all(&skill_dir).unwrap();
                        fs::write(skill_dir.join("SKILL.md"), "skill content").unwrap();
                    }
                }),
                repos: vec![],
                want_clean: true,
                want_contains: vec![],
            },
            Case {
                name: ".claude/ with user files",
                setup: Box::new(|ws| {
                    fs::write(ws.join(METADATA_FILE), "").unwrap();
                    for name in &["wsp-manage", "wsp-report"] {
                        let skill_dir = ws.join(format!(".claude/skills/{}", name));
                        fs::create_dir_all(&skill_dir).unwrap();
                        fs::write(skill_dir.join("SKILL.md"), "skill content").unwrap();
                    }
                    fs::write(ws.join(".claude/settings.json"), "{}").unwrap();
                }),
                repos: vec![],
                want_clean: false,
                want_contains: vec!["?? .claude/settings.json"],
            },
            Case {
                name: "go.work with wsp header",
                setup: Box::new(|ws| {
                    fs::write(ws.join(METADATA_FILE), "").unwrap();
                    fs::write(
                        ws.join("go.work"),
                        "// Code generated by ws. DO NOT EDIT.\ngo 1.22\n\nuse (\n\t./api\n)\n",
                    )
                    .unwrap();
                }),
                repos: vec![],
                want_clean: true,
                want_contains: vec![],
            },
            Case {
                name: "go.work without wsp header",
                setup: Box::new(|ws| {
                    fs::write(ws.join(METADATA_FILE), "").unwrap();
                    fs::write(ws.join("go.work"), "go 1.22\n\nuse (\n\t./api\n)\n").unwrap();
                }),
                repos: vec![],
                want_clean: false,
                want_contains: vec!["?? go.work"],
            },
            Case {
                name: "go.work.sum alongside wsp go.work",
                setup: Box::new(|ws| {
                    fs::write(ws.join(METADATA_FILE), "").unwrap();
                    fs::write(
                        ws.join("go.work"),
                        "// Code generated by ws. DO NOT EDIT.\ngo 1.22\n\nuse (\n\t./api\n)\n",
                    )
                    .unwrap();
                    fs::write(ws.join("go.work.sum"), "sum data").unwrap();
                }),
                repos: vec![],
                want_clean: true,
                want_contains: vec![],
            },
            Case {
                name: "go.work.sum without go.work is flagged",
                setup: Box::new(|ws| {
                    fs::write(ws.join(METADATA_FILE), "").unwrap();
                    fs::write(ws.join("go.work.sum"), "sum data").unwrap();
                }),
                repos: vec![],
                want_clean: false,
                want_contains: vec!["?? go.work.sum"],
            },
            Case {
                name: "noise files (.DS_Store) ignored",
                setup: Box::new(|ws| {
                    fs::write(ws.join(METADATA_FILE), "").unwrap();
                    fs::write(ws.join(".DS_Store"), "").unwrap();
                    fs::write(ws.join("Thumbs.db"), "").unwrap();
                    fs::write(ws.join("desktop.ini"), "").unwrap();
                }),
                repos: vec![],
                want_clean: true,
                want_contains: vec![],
            },
            Case {
                name: "multiple issues combined",
                setup: Box::new(|ws| {
                    fs::write(ws.join(METADATA_FILE), "").unwrap();
                    fs::write(ws.join("notes.md"), "x").unwrap();
                    let claude_dir = ws.join(".claude");
                    fs::create_dir_all(&claude_dir).unwrap();
                    fs::write(claude_dir.join("settings.json"), "{}").unwrap();
                }),
                repos: vec![],
                want_clean: false,
                want_contains: vec!["?? notes.md", "?? .claude/settings.json"],
            },
        ];

        for tc in &cases {
            let tmp = tempfile::tempdir().unwrap();
            let ws_dir = tmp.path();
            (tc.setup)(ws_dir);

            let meta = make_simple_metadata(&tc.repos);
            let problems = check_root_content(ws_dir, &meta).unwrap();

            if tc.want_clean {
                assert!(
                    problems.is_empty(),
                    "case {:?}: expected clean, got {:?}",
                    tc.name,
                    problems
                );
            } else {
                assert!(
                    !problems.is_empty(),
                    "case {:?}: expected problems, got none",
                    tc.name
                );
            }

            for want in &tc.want_contains {
                assert!(
                    problems.iter().any(|p| p.contains(want)),
                    "case {:?}: expected problem containing {:?}, got {:?}",
                    tc.name,
                    want,
                    problems
                );
            }
        }
    }

    #[test]
    fn test_check_agents_md() {
        struct Case {
            name: &'static str,
            content: &'static str,
            want_clean: bool,
            want_contains: Option<&'static str>,
        }

        let cases = vec![
            Case {
                name: "scaffold only",
                content: "# Workspace: test\n\n<!-- Add your project-specific notes for AI agents here -->\n\n<!-- wsp:begin -->\nstuff\n<!-- wsp:end -->\n",
                want_clean: true,
                want_contains: None,
            },
            Case {
                name: "user heading before marker",
                content: "# Workspace: test\n\n## My Notes\n\n<!-- wsp:begin -->\nstuff\n<!-- wsp:end -->\n",
                want_clean: false,
                want_contains: Some("user-added content"),
            },
            Case {
                name: "user paragraph before marker",
                content: "# Workspace: test\n\nThis is important context for AI agents.\n\n<!-- wsp:begin -->\nstuff\n<!-- wsp:end -->\n",
                want_clean: false,
                want_contains: Some("user-added content"),
            },
            Case {
                name: "no markers",
                content: "# Some random file\n\nNo wsp markers here.\n",
                want_clean: false,
                want_contains: Some("wsp markers missing"),
            },
            Case {
                name: "empty preamble",
                content: "<!-- wsp:begin -->\nstuff\n<!-- wsp:end -->\n",
                want_clean: true,
                want_contains: None,
            },
            Case {
                name: "only blank lines before marker",
                content: "\n\n\n<!-- wsp:begin -->\nstuff\n<!-- wsp:end -->\n",
                want_clean: true,
                want_contains: None,
            },
            Case {
                name: "user content after end marker",
                content: "# Workspace: test\n\n<!-- wsp:begin -->\nstuff\n<!-- wsp:end -->\n\n## My post-marker notes\n",
                want_clean: false,
                want_contains: Some("user-added content after markers"),
            },
        ];

        for tc in &cases {
            let tmp = tempfile::tempdir().unwrap();
            let ws_dir = tmp.path();
            fs::write(ws_dir.join("AGENTS.md"), tc.content).unwrap();

            let result = check_agents_md(ws_dir);

            if tc.want_clean {
                assert!(
                    result.is_none(),
                    "case {:?}: expected clean, got {:?}",
                    tc.name,
                    result
                );
            } else {
                assert!(
                    result.is_some(),
                    "case {:?}: expected problem, got None",
                    tc.name
                );
                if let Some(want) = tc.want_contains {
                    assert!(
                        result.as_ref().unwrap().contains(want),
                        "case {:?}: expected {:?} in {:?}",
                        tc.name,
                        want,
                        result
                    );
                }
            }
        }
    }

    /// Create a git repo in the given directory with one commit and an origin remote.
    fn create_local_repo(dir: &Path, origin_url: &str) {
        fs::create_dir_all(dir).unwrap();
        let cmds: Vec<Vec<&str>> = vec![
            vec!["git", "init", "--initial-branch=main"],
            vec!["git", "config", "user.email", "test@test.com"],
            vec!["git", "config", "user.name", "Test"],
            vec!["git", "config", "commit.gpgsign", "false"],
            vec!["git", "commit", "--allow-empty", "-m", "initial"],
            vec!["git", "remote", "add", "origin", origin_url],
        ];
        for args in &cmds {
            let output = Command::new(args[0])
                .args(&args[1..])
                .current_dir(dir)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "command {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn test_validate_existing_dir_success() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_dir = tmp.path().join("test-repo");
        create_local_repo(&repo_dir, "git@github.com:user/test-repo.git");

        let result = validate_existing_dir(&repo_dir, "github.com/user/test-repo");
        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
    }

    #[test]
    fn test_validate_existing_dir_cases() {
        struct Case {
            name: &'static str,
            setup: Box<dyn Fn(&Path)>,
            identity: &'static str,
            expect_err: &'static str,
        }

        let cases = vec![
            Case {
                name: "not a git repo",
                setup: Box::new(|dir: &Path| {
                    fs::create_dir_all(dir).unwrap();
                }),
                identity: "github.com/user/test-repo",
                expect_err: "not a git repository",
            },
            Case {
                name: "no origin remote",
                setup: Box::new(|dir: &Path| {
                    fs::create_dir_all(dir).unwrap();
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
                            .current_dir(dir)
                            .output()
                            .unwrap();
                        assert!(output.status.success());
                    }
                }),
                identity: "github.com/user/test-repo",
                expect_err: "no origin remote",
            },
            Case {
                name: "identity mismatch",
                setup: Box::new(|dir: &Path| {
                    create_local_repo(dir, "git@github.com:other/wrong-repo.git");
                }),
                identity: "github.com/user/test-repo",
                expect_err: "doesn't match expected",
            },
        ];

        for tc in cases {
            let tmp = tempfile::tempdir().unwrap();
            let repo_dir = tmp.path().join("test-repo");
            (tc.setup)(&repo_dir);

            let result = validate_existing_dir(&repo_dir, tc.identity);
            assert!(result.is_err(), "{}: expected error, got Ok", tc.name);
            let err = result.unwrap_err().to_string();
            assert!(
                err.contains(tc.expect_err),
                "{}: expected error containing {:?}, got {:?}",
                tc.name,
                tc.expect_err,
                err
            );
        }
    }

    #[test]
    fn test_adopt_existing_dir_in_workspace() {
        let (paths, _d, _r, identity, upstream_urls) = setup_test_env();

        // Create workspace with the repo first
        let refs = BTreeMap::from([(identity.clone(), String::new())]);
        create(&paths, "adopt-ws", &refs, None, &upstream_urls).unwrap();

        let ws_dir = dir(&paths.workspaces_dir, "adopt-ws");
        let meta = load_metadata(&ws_dir).unwrap();
        let branch = meta.branch.clone();

        // Create a second "upstream" repo and its mirror
        let repo2_dir = tempfile::tempdir().unwrap();
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
                .current_dir(repo2_dir.path())
                .output()
                .unwrap();
            assert!(output.status.success());
        }

        let parsed2 = giturl::Parsed {
            host: "test.local".into(),
            owner: "user".into(),
            repo: "local-repo".into(),
        };
        mirror::clone(
            &paths.mirrors_dir,
            &parsed2,
            repo2_dir.path().to_str().unwrap(),
        )
        .unwrap();

        // Set up mirror HEAD ref
        let mirror_dir2 = mirror::dir(&paths.mirrors_dir, &parsed2);
        let output = Command::new("git")
            .args([
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/heads/main",
            ])
            .current_dir(&mirror_dir2)
            .output()
            .unwrap();
        assert!(output.status.success());

        let identity2 = parsed2.identity();

        // Manually create a repo directory inside the workspace (simulating user workflow)
        // Use an SSH-style URL that matches the identity so validation passes
        let local_dir = ws_dir.join("local-repo");
        create_local_repo(&local_dir, "git@test.local:user/local-repo.git");

        // Checkout the workspace branch so adoption is silent
        git::checkout_new_branch(&local_dir, &branch, "HEAD").unwrap();

        // Now add_repos should adopt it instead of cloning
        let refs2 = BTreeMap::from([(identity2.clone(), String::new())]);
        let upstream_urls2 = BTreeMap::from([(
            identity2.clone(),
            repo2_dir.path().to_str().unwrap().to_string(),
        )]);
        add_repos(&paths.mirrors_dir, &ws_dir, &refs2, &upstream_urls2).unwrap();

        // Verify it was registered in metadata
        let meta = load_metadata(&ws_dir).unwrap();
        assert!(
            meta.repos.contains_key(&identity2),
            "adopted repo should be in metadata"
        );

        // Verify the directory still exists with its .git
        assert!(local_dir.join(".git").exists());
    }

    #[test]
    fn test_adopt_rejects_identity_mismatch() {
        let (paths, _d, _r, identity, upstream_urls) = setup_test_env();

        let refs = BTreeMap::from([(identity.clone(), String::new())]);
        create(&paths, "adopt-mismatch", &refs, None, &upstream_urls).unwrap();

        let ws_dir = dir(&paths.workspaces_dir, "adopt-mismatch");

        // Create a directory with a different origin
        let local_dir = ws_dir.join("wrong-repo");
        create_local_repo(&local_dir, "git@github.com:other/wrong-repo.git");

        // Try to adopt it as a different identity — should fail
        let wrong_identity = "test.local/user/wrong-repo".to_string();
        let parsed_wrong = giturl::Parsed {
            host: "test.local".into(),
            owner: "user".into(),
            repo: "wrong-repo".into(),
        };
        // Create mirror for the wrong identity
        let wrong_upstream = tempfile::tempdir().unwrap();
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
                .current_dir(wrong_upstream.path())
                .output()
                .unwrap();
            assert!(output.status.success());
        }
        mirror::clone(
            &paths.mirrors_dir,
            &parsed_wrong,
            wrong_upstream.path().to_str().unwrap(),
        )
        .unwrap();

        let refs2 = BTreeMap::from([(wrong_identity.clone(), String::new())]);
        let upstream_urls2 = BTreeMap::from([(
            wrong_identity,
            wrong_upstream.path().to_str().unwrap().to_string(),
        )]);

        let result = add_repos(&paths.mirrors_dir, &ws_dir, &refs2, &upstream_urls2);
        assert!(result.is_err(), "should reject identity mismatch");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("doesn't match"),
            "error should mention mismatch, got: {}",
            err
        );
    }

    /// Regression: when the mirror's refs/heads/main was stale (behind
    /// refs/remotes/origin/main), git clone --local checked out the old tree
    /// and the subsequent checkout -b left a dirty index.
    #[test]
    fn test_create_clean_index_after_mirror_diverges() {
        let (paths, _d, repo_dir, identity, upstream_urls) = setup_test_env();

        // Push new commits to the upstream AFTER the mirror was created,
        // then fetch the mirror so refs/remotes/origin/main advances but
        // (pre-fix) refs/heads/main would stay stale.
        let cmds: Vec<Vec<&str>> = vec![
            vec!["git", "commit", "--allow-empty", "-m", "second"],
            vec!["git", "commit", "--allow-empty", "-m", "third"],
        ];
        for args in &cmds {
            let out = Command::new(args[0])
                .args(&args[1..])
                .current_dir(repo_dir.path())
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{:?}: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
        }

        let parsed = giturl::Parsed {
            host: "test.local".into(),
            owner: "user".into(),
            repo: "test-repo".into(),
        };
        mirror::fetch(&paths.mirrors_dir, &parsed).unwrap();

        // Create workspace — this used to leave staged diffs
        let refs = BTreeMap::from([(identity, String::new())]);
        create(&paths, "clean-idx", &refs, None, &upstream_urls).unwrap();

        let clone_dir = dir(&paths.workspaces_dir, "clean-idx").join("test-repo");

        // Index must match HEAD (no staged changes)
        let diff = git::run(Some(&clone_dir), &["diff", "--cached", "--stat"]).unwrap();
        assert!(
            diff.is_empty(),
            "expected clean index, got staged changes:\n{}",
            diff
        );
    }
}

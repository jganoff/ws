use std::fs;

use anyhow::Result;
use clap::{ArgMatches, Command};
use serde::Serialize;

use crate::agentmd;
use crate::config::{self, Paths};
use crate::gc;
use crate::git;
use crate::giturl;
use crate::mirror;
use crate::output::Output;
use crate::workspace;

// ---------------------------------------------------------------------------
// Output types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct DoctorOutput {
    pub ok: bool,
    pub checks: Vec<DoctorCheck>,
    pub summary: DoctorSummary,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorCheck {
    pub scope: String,
    pub check: String,
    pub status: CheckStatus,
    pub message: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub fixable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Ok,
    Warn,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorSummary {
    pub total: usize,
    pub ok: usize,
    pub warn: usize,
    pub error: usize,
    pub fixed: usize,
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[cfg(feature = "codegen")]
impl DoctorOutput {
    pub fn sample() -> Self {
        Self {
            ok: false,
            checks: vec![
                DoctorCheck {
                    scope: "global".into(),
                    check: "config-parseable".into(),
                    status: CheckStatus::Ok,
                    message: "config is valid (5 registered repos)".into(),
                    fixable: false,
                    details: None,
                },
                DoctorCheck {
                    scope: "workspace/my-feature/bar".into(),
                    check: "origin-url-match".into(),
                    status: CheckStatus::Warn,
                    message: "bar: origin URL differs from registered URL".into(),
                    fixable: true,
                    details: Some(serde_json::json!({
                        "clone_url": "git@github.com:acme/bar.git",
                        "registered_url": "https://github.com/acme/bar"
                    })),
                },
            ],
            summary: DoctorSummary {
                total: 8,
                ok: 7,
                warn: 1,
                error: 0,
                fixed: 0,
            },
        }
    }
}

pub fn cmd() -> Command {
    Command::new("doctor")
        .about("Check workspace and global state for problems")
        .long_about(
            "Check workspace and global state for problems.\n\n\
             Validates config, mirrors, and workspace clones for invariant violations. \
             Run inside a workspace to also check that workspace's repos. Use --fix to \
             auto-repair fixable issues.",
        )
        .arg(
            clap::Arg::new("fix")
                .long("fix")
                .action(clap::ArgAction::SetTrue)
                .help("Auto-fix fixable problems"),
        )
}

pub fn run(matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    let fix = matches.get_flag("fix");
    let mut checks = Vec::new();
    let mut fixed = 0usize;

    // --- Global checks ---
    eprintln!("Checking global state...");

    // 1. Config parseable
    let cfg_result = config::Config::load_from(&paths.config_path);
    match &cfg_result {
        Ok(cfg) => {
            checks.push(DoctorCheck {
                scope: "global".into(),
                check: "config-parseable".into(),
                status: CheckStatus::Ok,
                message: format!("config is valid ({} registered repos)", cfg.repos.len()),
                fixable: false,
                details: None,
            });
            eprintln!("  ✓ config is valid ({} registered repos)", cfg.repos.len());
        }
        Err(e) => {
            checks.push(DoctorCheck {
                scope: "global".into(),
                check: "config-parseable".into(),
                status: CheckStatus::Error,
                message: format!("config failed to load: {}", e),
                fixable: false,
                details: None,
            });
            eprintln!("  ✗ config failed to load: {}", e);
            // Can't proceed without config
            return Ok(Output::Doctor(build_output(checks, fixed)));
        }
    };
    let cfg = cfg_result.unwrap();

    // G2. Config version skew
    if cfg.version > config::CURRENT_CONFIG_VERSION {
        checks.push(DoctorCheck {
            scope: "global".into(),
            check: "config-version".into(),
            status: CheckStatus::Warn,
            message: format!(
                "config version {} is newer than supported version {}",
                cfg.version,
                config::CURRENT_CONFIG_VERSION
            ),
            fixable: false,
            details: Some(serde_json::json!({
                "config_version": cfg.version,
                "supported_version": config::CURRENT_CONFIG_VERSION,
            })),
        });
        eprintln!(
            "  ⚠ config version {} is newer than supported version {}",
            cfg.version,
            config::CURRENT_CONFIG_VERSION
        );
    } else {
        checks.push(DoctorCheck {
            scope: "global".into(),
            check: "config-version".into(),
            status: CheckStatus::Ok,
            message: format!("config version {}", cfg.version),
            fixable: false,
            details: None,
        });
        eprintln!("  ✓ config version {}", cfg.version);
    }

    // 2. Mirrors exist for registered repos
    let mut missing_mirrors = Vec::new();
    for (identity, entry) in &cfg.repos {
        if let Ok(parsed) = giturl::parse(&entry.url)
            && !mirror::exists(&paths.mirrors_dir, &parsed)
        {
            missing_mirrors.push((identity.clone(), entry.url.clone()));
        }
    }
    if missing_mirrors.is_empty() {
        let mirror_count = cfg.repos.len();
        checks.push(DoctorCheck {
            scope: "global".into(),
            check: "mirrors-exist".into(),
            status: CheckStatus::Ok,
            message: format!("{} mirrors present", mirror_count),
            fixable: false,
            details: None,
        });
        eprintln!("  ✓ {} mirrors present", mirror_count);
    } else {
        for (identity, url) in &missing_mirrors {
            let fixable = true;
            if fix && let Ok(parsed) = giturl::parse(url) {
                match mirror::clone(&paths.mirrors_dir, &parsed, url) {
                    Ok(()) => {
                        checks.push(DoctorCheck {
                            scope: "global".into(),
                            check: "mirrors-exist".into(),
                            status: CheckStatus::Ok,
                            message: format!("{}: re-cloned mirror", identity),
                            fixable,
                            details: None,
                        });
                        eprintln!("  ✓ {}: re-cloned mirror", identity);
                        fixed += 1;
                        continue;
                    }
                    Err(e) => {
                        checks.push(DoctorCheck {
                            scope: "global".into(),
                            check: "mirrors-exist".into(),
                            status: CheckStatus::Error,
                            message: format!("{}: mirror missing, fix failed: {}", identity, e),
                            fixable,
                            details: None,
                        });
                        eprintln!("  ✗ {}: mirror missing, fix failed: {}", identity, e);
                        continue;
                    }
                }
            }
            checks.push(DoctorCheck {
                scope: "global".into(),
                check: "mirrors-exist".into(),
                status: CheckStatus::Warn,
                message: format!("{}: mirror missing", identity),
                fixable,
                details: None,
            });
            eprintln!("  ⚠ {}: mirror missing", identity);
        }
    }

    // G1. Orphaned mirrors — mirrors dir entries with no config entry
    check_orphaned_mirrors(paths, &cfg, fix, &mut checks, &mut fixed);

    // G4. GC stale entries — entries past retention that should have been purged
    check_gc_stale_entries(paths, &cfg, fix, &mut checks, &mut fixed);

    // --- Workspace checks (if inside one) ---
    let cwd = std::env::current_dir()?;
    if let Ok(ws_dir) = workspace::detect(&cwd) {
        let meta = workspace::load_metadata(&ws_dir)?;
        let ws_scope = format!("workspace/{}", meta.name);
        eprintln!("\nChecking workspace {:?}...", meta.name);

        // W1. Metadata version skew
        if meta.version > workspace::CURRENT_METADATA_VERSION {
            checks.push(DoctorCheck {
                scope: ws_scope.clone(),
                check: "metadata-version".into(),
                status: CheckStatus::Warn,
                message: format!(
                    "metadata version {} is newer than supported version {}",
                    meta.version,
                    workspace::CURRENT_METADATA_VERSION
                ),
                fixable: false,
                details: Some(serde_json::json!({
                    "metadata_version": meta.version,
                    "supported_version": workspace::CURRENT_METADATA_VERSION,
                })),
            });
            eprintln!(
                "  ⚠ metadata version {} is newer than supported version {}",
                meta.version,
                workspace::CURRENT_METADATA_VERSION
            );
        } else {
            checks.push(DoctorCheck {
                scope: ws_scope.clone(),
                check: "metadata-version".into(),
                status: CheckStatus::Ok,
                message: format!("metadata version {}", meta.version),
                fixable: false,
                details: None,
            });
            eprintln!("  ✓ metadata version {}", meta.version);
        }

        // W3. Legacy ref field — stale @ref values in metadata
        check_legacy_ref_field(&ws_dir, &meta, &ws_scope, fix, &mut checks, &mut fixed);

        // W4. Stale dirs map — orphaned entries in dirs collision map
        check_stale_dirs_map(&ws_dir, &meta, &ws_scope, fix, &mut checks, &mut fixed);

        // W12. Unregistered repos — workspace repos not in global registry
        check_unregistered_repos(&meta, &cfg, &ws_scope, &mut checks);

        // W9. AGENTS.md / CLAUDE.md validity
        check_agents_md_valid(&ws_dir, &meta, &ws_scope, fix, &mut checks, &mut fixed);

        // Per-repo checks
        let repo_infos = meta.repo_infos(&ws_dir);
        for info in &repo_infos {
            let scope = format!("workspace/{}/{}", meta.name, info.dir_name);

            // Repo dir exists
            if info.error.is_some() || !info.clone_dir.exists() {
                checks.push(DoctorCheck {
                    scope: scope.clone(),
                    check: "repo-dir-exists".into(),
                    status: CheckStatus::Error,
                    message: format!("{}: directory missing", info.dir_name),
                    fixable: false,
                    details: None,
                });
                eprintln!("  ✗ {}: directory missing", info.dir_name);
                continue;
            }

            // Origin remote exists
            if !git::has_remote(&info.clone_dir, "origin") {
                checks.push(DoctorCheck {
                    scope: scope.clone(),
                    check: "origin-remote-exists".into(),
                    status: CheckStatus::Error,
                    message: format!("{}: no origin remote", info.dir_name),
                    fixable: false,
                    details: None,
                });
                eprintln!("  ✗ {}: no origin remote", info.dir_name);
                continue;
            }

            // W2. Legacy wsp-mirror remote
            check_legacy_wsp_mirror(
                &info.clone_dir,
                &info.dir_name,
                &scope,
                fix,
                &mut checks,
                &mut fixed,
            );

            // W7. In-progress git operation
            check_in_progress_op(&info.clone_dir, &info.dir_name, &scope, &mut checks);

            // W8. Clone on workspace branch
            check_clone_branch(
                &info.clone_dir,
                &info.dir_name,
                &meta.branch,
                &scope,
                &mut checks,
            );

            // Origin URL matches registered URL
            let clone_url = git::remote_get_url(&info.clone_dir, "origin")
                .unwrap_or_default()
                .trim()
                .to_string();
            let registered_url = cfg.upstream_url(&info.identity).unwrap_or("");

            if !urls_equivalent(&clone_url, registered_url) {
                let fixable = true;
                if fix && !registered_url.is_empty() {
                    match git::remote_set_url(&info.clone_dir, "origin", registered_url) {
                        Ok(()) => {
                            checks.push(DoctorCheck {
                                scope: scope.clone(),
                                check: "origin-url-match".into(),
                                status: CheckStatus::Ok,
                                message: format!(
                                    "{}: repointed origin to {}",
                                    info.dir_name, registered_url
                                ),
                                fixable,
                                details: None,
                            });
                            eprintln!(
                                "  ✓ {}: repointed origin to {}",
                                info.dir_name, registered_url
                            );
                            fixed += 1;
                            continue;
                        }
                        Err(e) => {
                            checks.push(DoctorCheck {
                                scope: scope.clone(),
                                check: "origin-url-match".into(),
                                status: CheckStatus::Warn,
                                message: format!(
                                    "{}: origin URL mismatch, fix failed: {}",
                                    info.dir_name, e
                                ),
                                fixable,
                                details: Some(serde_json::json!({
                                    "clone_url": clone_url,
                                    "registered_url": registered_url,
                                })),
                            });
                            eprintln!(
                                "  ⚠ {}: origin URL mismatch, fix failed: {}",
                                info.dir_name, e
                            );
                            continue;
                        }
                    }
                }
                checks.push(DoctorCheck {
                    scope: scope.clone(),
                    check: "origin-url-match".into(),
                    status: CheckStatus::Warn,
                    message: format!("{}: origin URL differs from registered URL", info.dir_name),
                    fixable,
                    details: Some(serde_json::json!({
                        "clone_url": clone_url,
                        "registered_url": registered_url,
                    })),
                });
                eprintln!(
                    "  ⚠ {}: origin URL differs from registered URL",
                    info.dir_name
                );
                eprintln!("      clone:      {}", clone_url);
                eprintln!("      registered: {}", registered_url);
                continue;
            }

            // Identity matches (origin URL resolves to same identity as .wsp.yaml)
            if let Ok(parsed) = giturl::parse(&clone_url) {
                let clone_identity = parsed.identity();
                if clone_identity != info.identity {
                    checks.push(DoctorCheck {
                        scope: scope.clone(),
                        check: "identity-match".into(),
                        status: CheckStatus::Warn,
                        message: format!(
                            "{}: origin URL resolves to {} but .wsp.yaml says {}",
                            info.dir_name, clone_identity, info.identity
                        ),
                        fixable: false,
                        details: Some(serde_json::json!({
                            "clone_identity": clone_identity,
                            "metadata_identity": info.identity,
                        })),
                    });
                    eprintln!(
                        "  ⚠ {}: identity mismatch (origin={}, metadata={})",
                        info.dir_name, clone_identity, info.identity
                    );
                    continue;
                }
            }

            // All checks passed for this repo
            checks.push(DoctorCheck {
                scope,
                check: "repo-ok".into(),
                status: CheckStatus::Ok,
                message: format!("{}: ok", info.dir_name),
                fixable: false,
                details: None,
            });
            eprintln!("  ✓ {}: ok", info.dir_name);
        }
    }

    // --- Summary ---
    let output = build_output(checks, fixed);
    let summary = &output.summary;

    eprintln!();
    if summary.warn == 0 && summary.error == 0 {
        eprintln!("All checks passed.");
    } else {
        let mut parts = Vec::new();
        if summary.warn > 0 {
            parts.push(format!(
                "{} warning{}",
                summary.warn,
                if summary.warn == 1 { "" } else { "s" }
            ));
        }
        if summary.error > 0 {
            parts.push(format!(
                "{} error{}",
                summary.error,
                if summary.error == 1 { "" } else { "s" }
            ));
        }
        if fixed > 0 {
            parts.push(format!(
                "{} fix{} applied",
                fixed,
                if fixed == 1 { "" } else { "es" }
            ));
        }
        let msg = parts.join(", ");
        eprintln!("{}.", msg);
        let any_fixable = output
            .checks
            .iter()
            .any(|c| c.status == CheckStatus::Warn && c.fixable);
        if any_fixable && !fix {
            eprintln!("Run `wsp doctor --fix` to auto-fix.");
        }
    }

    Ok(Output::Doctor(output))
}

// ---------------------------------------------------------------------------
// Global check helpers
// ---------------------------------------------------------------------------

/// G1. Orphaned mirrors — mirror dirs with no corresponding config entry.
fn check_orphaned_mirrors(
    paths: &Paths,
    cfg: &config::Config,
    fix: bool,
    checks: &mut Vec<DoctorCheck>,
    fixed: &mut usize,
) {
    if !paths.mirrors_dir.exists() {
        return;
    }

    let mut orphaned = Vec::new();

    // Walk mirrors_dir/<host>/<owner>/<repo>.git
    let hosts = match fs::read_dir(&paths.mirrors_dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for host_entry in hosts.flatten() {
        if !host_entry.path().is_dir() {
            continue;
        }
        // Skip non-UTF8 dir names to avoid lossy conversion producing identity collisions
        let host = match host_entry.file_name().to_str() {
            Some(s) => s.to_string(),
            None => continue,
        };
        let owners = match fs::read_dir(host_entry.path()) {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        for owner_entry in owners.flatten() {
            if !owner_entry.path().is_dir() {
                continue;
            }
            let owner = match owner_entry.file_name().to_str() {
                Some(s) => s.to_string(),
                None => continue,
            };
            let repos = match fs::read_dir(owner_entry.path()) {
                Ok(rd) => rd,
                Err(_) => continue,
            };
            for repo_entry in repos.flatten() {
                let name = match repo_entry.file_name().to_str() {
                    Some(s) => s.to_string(),
                    None => continue,
                };
                if !name.ends_with(".git") || !repo_entry.path().is_dir() {
                    continue;
                }
                let repo = name.trim_end_matches(".git");
                let identity = format!("{}/{}/{}", host, owner, repo);
                if !cfg.repos.contains_key(&identity) {
                    orphaned.push((identity, repo_entry.path()));
                }
            }
        }
    }

    if orphaned.is_empty() {
        checks.push(DoctorCheck {
            scope: "global".into(),
            check: "orphaned-mirrors".into(),
            status: CheckStatus::Ok,
            message: "no orphaned mirrors".into(),
            fixable: false,
            details: None,
        });
        eprintln!("  ✓ no orphaned mirrors");
    } else {
        for (identity, path) in &orphaned {
            let fixable = true;
            if fix {
                // Verify the path is still a real directory (not a symlink) to
                // narrow the TOCTOU window between scan and deletion.
                match fs::symlink_metadata(path) {
                    Ok(m) if m.file_type().is_symlink() => {
                        checks.push(DoctorCheck {
                            scope: "global".into(),
                            check: "orphaned-mirrors".into(),
                            status: CheckStatus::Warn,
                            message: format!(
                                "{}: orphaned mirror is a symlink, skipping removal",
                                identity
                            ),
                            fixable: false,
                            details: None,
                        });
                        eprintln!(
                            "  ⚠ {}: orphaned mirror is a symlink, skipping removal",
                            identity
                        );
                        continue;
                    }
                    Err(_) => continue, // vanished between scan and fix
                    _ => {}
                }
                match fs::remove_dir_all(path) {
                    Ok(()) => {
                        checks.push(DoctorCheck {
                            scope: "global".into(),
                            check: "orphaned-mirrors".into(),
                            status: CheckStatus::Ok,
                            message: format!("{}: removed orphaned mirror", identity),
                            fixable,
                            details: None,
                        });
                        eprintln!("  ✓ {}: removed orphaned mirror", identity);
                        *fixed += 1;
                        continue;
                    }
                    Err(e) => {
                        checks.push(DoctorCheck {
                            scope: "global".into(),
                            check: "orphaned-mirrors".into(),
                            status: CheckStatus::Warn,
                            message: format!(
                                "{}: orphaned mirror, removal failed: {}",
                                identity, e
                            ),
                            fixable,
                            details: None,
                        });
                        eprintln!("  ⚠ {}: orphaned mirror, removal failed: {}", identity, e);
                        continue;
                    }
                }
            }
            checks.push(DoctorCheck {
                scope: "global".into(),
                check: "orphaned-mirrors".into(),
                status: CheckStatus::Warn,
                message: format!("{}: mirror has no config entry", identity),
                fixable,
                details: None,
            });
            eprintln!("  ⚠ {}: mirror has no config entry", identity);
        }
    }
}

/// G4. GC stale entries — entries past retention that should have been purged.
fn check_gc_stale_entries(
    paths: &Paths,
    cfg: &config::Config,
    fix: bool,
    checks: &mut Vec<DoctorCheck>,
    fixed: &mut usize,
) {
    if !paths.gc_dir.exists() {
        checks.push(DoctorCheck {
            scope: "global".into(),
            check: "gc-stale-entries".into(),
            status: CheckStatus::Ok,
            message: "no gc entries".into(),
            fixable: false,
            details: None,
        });
        eprintln!("  ✓ no gc entries");
        return;
    }

    let retention_days = cfg.gc_retention_days.unwrap_or(gc::DEFAULT_RETENTION_DAYS);
    let cutoff = chrono::Utc::now() - chrono::Duration::days(retention_days as i64);

    let entries = match gc::list(&paths.gc_dir) {
        Ok(e) => e,
        Err(e) => {
            checks.push(DoctorCheck {
                scope: "global".into(),
                check: "gc-stale-entries".into(),
                status: CheckStatus::Warn,
                message: format!("failed to list gc entries: {}", e),
                fixable: false,
                details: None,
            });
            eprintln!("  ⚠ failed to list gc entries: {}", e);
            return;
        }
    };

    let stale: Vec<_> = entries.iter().filter(|e| e.trashed_at < cutoff).collect();

    if stale.is_empty() {
        checks.push(DoctorCheck {
            scope: "global".into(),
            check: "gc-stale-entries".into(),
            status: CheckStatus::Ok,
            message: format!(
                "{} gc entries, none past {}-day retention",
                entries.len(),
                retention_days
            ),
            fixable: false,
            details: None,
        });
        eprintln!(
            "  ✓ {} gc entries, none past {}-day retention",
            entries.len(),
            retention_days
        );
    } else {
        let fixable = true;
        if fix {
            match gc::purge(&paths.gc_dir, retention_days) {
                Ok(removed) => {
                    checks.push(DoctorCheck {
                        scope: "global".into(),
                        check: "gc-stale-entries".into(),
                        status: CheckStatus::Ok,
                        message: format!("purged {} stale gc entries", removed),
                        fixable,
                        details: None,
                    });
                    eprintln!("  ✓ purged {} stale gc entries", removed);
                    *fixed += 1;
                }
                Err(e) => {
                    checks.push(DoctorCheck {
                        scope: "global".into(),
                        check: "gc-stale-entries".into(),
                        status: CheckStatus::Warn,
                        message: format!("{} stale gc entries, purge failed: {}", stale.len(), e),
                        fixable,
                        details: None,
                    });
                    eprintln!("  ⚠ {} stale gc entries, purge failed: {}", stale.len(), e);
                }
            }
        } else {
            let names: Vec<_> = stale.iter().map(|e| e.name.as_str()).collect();
            checks.push(DoctorCheck {
                scope: "global".into(),
                check: "gc-stale-entries".into(),
                status: CheckStatus::Warn,
                message: format!(
                    "{} gc entries past {}-day retention",
                    stale.len(),
                    retention_days
                ),
                fixable,
                details: Some(serde_json::json!({ "stale_entries": names })),
            });
            eprintln!(
                "  ⚠ {} gc entries past {}-day retention",
                stale.len(),
                retention_days
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Workspace-level check helpers
// ---------------------------------------------------------------------------

/// W2. Legacy wsp-mirror remote — stale remote from pre-v0.5.
fn check_legacy_wsp_mirror(
    clone_dir: &std::path::Path,
    dir_name: &str,
    scope: &str,
    fix: bool,
    checks: &mut Vec<DoctorCheck>,
    fixed: &mut usize,
) {
    if !git::has_remote(clone_dir, "wsp-mirror") {
        return;
    }

    let fixable = true;
    if fix {
        match git::remove_remote(clone_dir, "wsp-mirror") {
            Ok(()) => {
                checks.push(DoctorCheck {
                    scope: scope.into(),
                    check: "legacy-wsp-mirror-remote".into(),
                    status: CheckStatus::Ok,
                    message: format!("{}: removed legacy wsp-mirror remote", dir_name),
                    fixable,
                    details: None,
                });
                eprintln!("  ✓ {}: removed legacy wsp-mirror remote", dir_name);
                *fixed += 1;
            }
            Err(e) => {
                checks.push(DoctorCheck {
                    scope: scope.into(),
                    check: "legacy-wsp-mirror-remote".into(),
                    status: CheckStatus::Warn,
                    message: format!(
                        "{}: legacy wsp-mirror remote, removal failed: {}",
                        dir_name, e
                    ),
                    fixable,
                    details: None,
                });
                eprintln!(
                    "  ⚠ {}: legacy wsp-mirror remote, removal failed: {}",
                    dir_name, e
                );
            }
        }
    } else {
        checks.push(DoctorCheck {
            scope: scope.into(),
            check: "legacy-wsp-mirror-remote".into(),
            status: CheckStatus::Warn,
            message: format!("{}: has legacy wsp-mirror remote", dir_name),
            fixable,
            details: None,
        });
        eprintln!("  ⚠ {}: has legacy wsp-mirror remote", dir_name);
    }
}

/// W7. In-progress git operation — interrupted rebase or merge.
fn check_in_progress_op(
    clone_dir: &std::path::Path,
    dir_name: &str,
    scope: &str,
    checks: &mut Vec<DoctorCheck>,
) {
    if let Some(op) = git::in_progress_op(clone_dir) {
        let op_name = match op {
            git::InProgressOp::Rebase => "rebase",
            git::InProgressOp::Merge => "merge",
        };
        checks.push(DoctorCheck {
            scope: scope.into(),
            check: "in-progress-git-op".into(),
            status: CheckStatus::Warn,
            message: format!("{}: interrupted {} in progress", dir_name, op_name),
            fixable: false,
            details: Some(serde_json::json!({ "operation": op_name })),
        });
        eprintln!("  ⚠ {}: interrupted {} in progress", dir_name, op_name);
    }
}

/// W8. Clone on workspace branch — repo not on expected branch.
fn check_clone_branch(
    clone_dir: &std::path::Path,
    dir_name: &str,
    workspace_branch: &str,
    scope: &str,
    checks: &mut Vec<DoctorCheck>,
) {
    match git::branch_current(clone_dir) {
        Ok(current) if current != workspace_branch => {
            checks.push(DoctorCheck {
                scope: scope.into(),
                check: "clone-on-workspace-branch".into(),
                status: CheckStatus::Warn,
                message: format!(
                    "{}: on branch {:?}, expected {:?}",
                    dir_name, current, workspace_branch
                ),
                fixable: false,
                details: Some(serde_json::json!({
                    "current_branch": current,
                    "workspace_branch": workspace_branch,
                })),
            });
            eprintln!(
                "  ⚠ {}: on branch {:?}, expected {:?}",
                dir_name, current, workspace_branch
            );
        }
        Ok(_) => {}  // on correct branch, no check emitted
        Err(_) => {} // detached HEAD or other issue, skip
    }
}

/// W3. Legacy ref field — stale @ref values in metadata.
fn check_legacy_ref_field(
    ws_dir: &std::path::Path,
    meta: &workspace::Metadata,
    ws_scope: &str,
    fix: bool,
    checks: &mut Vec<DoctorCheck>,
    fixed: &mut usize,
) {
    let stale_refs: Vec<String> = meta
        .repos
        .iter()
        .filter_map(|(identity, ref_opt)| {
            if let Some(repo_ref) = ref_opt
                && !repo_ref.r#ref.is_empty()
            {
                return Some(identity.clone());
            }
            None
        })
        .collect();

    if stale_refs.is_empty() {
        return;
    }

    let fixable = true;
    if fix {
        match crate::filelock::with_metadata(ws_dir, |m| {
            for (_, ref_opt) in m.repos.iter_mut() {
                if let Some(repo_ref) = ref_opt {
                    repo_ref.r#ref = String::new();
                }
            }
            Ok(())
        }) {
            Ok(_) => {
                checks.push(DoctorCheck {
                    scope: ws_scope.into(),
                    check: "legacy-ref-field".into(),
                    status: CheckStatus::Ok,
                    message: format!("cleared {} stale ref values", stale_refs.len()),
                    fixable,
                    details: None,
                });
                eprintln!("  ✓ cleared {} stale ref values", stale_refs.len());
                *fixed += 1;
            }
            Err(e) => {
                checks.push(DoctorCheck {
                    scope: ws_scope.into(),
                    check: "legacy-ref-field".into(),
                    status: CheckStatus::Warn,
                    message: format!(
                        "{} repos have stale ref values, fix failed: {}",
                        stale_refs.len(),
                        e
                    ),
                    fixable,
                    details: Some(serde_json::json!({ "identities": stale_refs })),
                });
                eprintln!(
                    "  ⚠ {} repos have stale ref values, fix failed: {}",
                    stale_refs.len(),
                    e
                );
            }
        }
    } else {
        checks.push(DoctorCheck {
            scope: ws_scope.into(),
            check: "legacy-ref-field".into(),
            status: CheckStatus::Warn,
            message: format!("{} repos have stale ref values", stale_refs.len()),
            fixable,
            details: Some(serde_json::json!({ "identities": stale_refs })),
        });
        eprintln!("  ⚠ {} repos have stale ref values", stale_refs.len());
    }
}

/// W4. Stale dirs map — orphaned entries in dirs collision map.
fn check_stale_dirs_map(
    ws_dir: &std::path::Path,
    meta: &workspace::Metadata,
    ws_scope: &str,
    fix: bool,
    checks: &mut Vec<DoctorCheck>,
    fixed: &mut usize,
) {
    let stale_entries: Vec<String> = meta
        .dirs
        .keys()
        .filter(|identity| !meta.repos.contains_key(*identity))
        .cloned()
        .collect();

    if stale_entries.is_empty() {
        return;
    }

    let fixable = true;
    if fix {
        match crate::filelock::with_metadata(ws_dir, |m| {
            m.dirs.retain(|identity, _| m.repos.contains_key(identity));
            Ok(())
        }) {
            Ok(_) => {
                checks.push(DoctorCheck {
                    scope: ws_scope.into(),
                    check: "stale-dirs-map".into(),
                    status: CheckStatus::Ok,
                    message: format!("removed {} stale dirs entries", stale_entries.len()),
                    fixable,
                    details: None,
                });
                eprintln!("  ✓ removed {} stale dirs entries", stale_entries.len());
                *fixed += 1;
            }
            Err(e) => {
                checks.push(DoctorCheck {
                    scope: ws_scope.into(),
                    check: "stale-dirs-map".into(),
                    status: CheckStatus::Warn,
                    message: format!(
                        "{} stale dirs entries, fix failed: {}",
                        stale_entries.len(),
                        e
                    ),
                    fixable,
                    details: Some(serde_json::json!({ "identities": stale_entries })),
                });
                eprintln!(
                    "  ⚠ {} stale dirs entries, fix failed: {}",
                    stale_entries.len(),
                    e
                );
            }
        }
    } else {
        checks.push(DoctorCheck {
            scope: ws_scope.into(),
            check: "stale-dirs-map".into(),
            status: CheckStatus::Warn,
            message: format!(
                "{} dirs entries for repos no longer in workspace",
                stale_entries.len()
            ),
            fixable,
            details: Some(serde_json::json!({ "identities": stale_entries })),
        });
        eprintln!(
            "  ⚠ {} dirs entries for repos no longer in workspace",
            stale_entries.len()
        );
    }
}

/// W12. Unregistered repos — workspace repos not in global registry.
fn check_unregistered_repos(
    meta: &workspace::Metadata,
    cfg: &config::Config,
    ws_scope: &str,
    checks: &mut Vec<DoctorCheck>,
) {
    let unregistered: Vec<&str> = meta
        .repos
        .keys()
        .filter(|identity| !cfg.repos.contains_key(identity.as_str()))
        .map(|s| s.as_str())
        .collect();

    if unregistered.is_empty() {
        checks.push(DoctorCheck {
            scope: ws_scope.into(),
            check: "unregistered-repos".into(),
            status: CheckStatus::Ok,
            message: "all workspace repos are in global registry".into(),
            fixable: false,
            details: None,
        });
        eprintln!("  ✓ all workspace repos are in global registry");
    } else {
        checks.push(DoctorCheck {
            scope: ws_scope.into(),
            check: "unregistered-repos".into(),
            status: CheckStatus::Warn,
            message: format!(
                "{} workspace repos not in global registry",
                unregistered.len()
            ),
            fixable: false,
            details: Some(serde_json::json!({ "identities": unregistered })),
        });
        eprintln!(
            "  ⚠ {} workspace repos not in global registry: {}",
            unregistered.len(),
            unregistered.join(", ")
        );
    }
}

/// W9. AGENTS.md / CLAUDE.md validity.
fn check_agents_md_valid(
    ws_dir: &std::path::Path,
    meta: &workspace::Metadata,
    ws_scope: &str,
    fix: bool,
    checks: &mut Vec<DoctorCheck>,
    fixed: &mut usize,
) {
    let agents_path = ws_dir.join("AGENTS.md");
    let claude_path = ws_dir.join("CLAUDE.md");

    let mut problems = Vec::new();

    // Check AGENTS.md markers
    if agents_path.exists() {
        match fs::read_to_string(&agents_path) {
            Ok(content) => {
                if !content.contains(agentmd::MARKER_BEGIN)
                    || !content.contains(agentmd::MARKER_END)
                {
                    problems.push("AGENTS.md missing wsp markers");
                }
            }
            Err(_) => {
                problems.push("AGENTS.md unreadable");
            }
        }
    } else {
        problems.push("AGENTS.md missing");
    }

    // Check CLAUDE.md symlink
    match fs::symlink_metadata(&claude_path) {
        Ok(m) => {
            if m.file_type().is_symlink() {
                match fs::read_link(&claude_path) {
                    Ok(target) if target != std::path::Path::new("AGENTS.md") => {
                        problems.push("CLAUDE.md symlinks to wrong target");
                    }
                    Err(_) => {
                        problems.push("CLAUDE.md symlink unreadable");
                    }
                    _ => {} // correct symlink
                }
            } else {
                problems.push("CLAUDE.md is not a symlink to AGENTS.md");
            }
        }
        Err(_) => {
            // CLAUDE.md doesn't exist — only a problem if AGENTS.md exists
            if agents_path.exists() {
                problems.push("CLAUDE.md missing (should be symlink to AGENTS.md)");
            }
        }
    }

    if problems.is_empty() {
        checks.push(DoctorCheck {
            scope: ws_scope.into(),
            check: "agents-md-valid".into(),
            status: CheckStatus::Ok,
            message: "AGENTS.md and CLAUDE.md are valid".into(),
            fixable: false,
            details: None,
        });
        eprintln!("  ✓ AGENTS.md and CLAUDE.md are valid");
    } else {
        let fixable = true;
        if fix {
            match agentmd::update(ws_dir, meta) {
                Ok(()) => {
                    // Also ensure CLAUDE.md symlink
                    let _ = fs::remove_file(&claude_path);
                    #[cfg(unix)]
                    let link_result: std::io::Result<()> =
                        std::os::unix::fs::symlink("AGENTS.md", &claude_path);
                    #[cfg(windows)]
                    let link_result: std::io::Result<()> =
                        std::os::windows::fs::symlink_file("AGENTS.md", &claude_path);
                    match link_result {
                        Ok(()) => {
                            checks.push(DoctorCheck {
                                scope: ws_scope.into(),
                                check: "agents-md-valid".into(),
                                status: CheckStatus::Ok,
                                message: "regenerated AGENTS.md and CLAUDE.md".into(),
                                fixable,
                                details: None,
                            });
                            eprintln!("  ✓ regenerated AGENTS.md and CLAUDE.md");
                            *fixed += 1;
                        }
                        Err(e) => {
                            checks.push(DoctorCheck {
                                scope: ws_scope.into(),
                                check: "agents-md-valid".into(),
                                status: CheckStatus::Warn,
                                message: format!(
                                    "AGENTS.md regenerated but CLAUDE.md symlink failed: {}",
                                    e
                                ),
                                fixable,
                                details: Some(serde_json::json!({ "problems": problems })),
                            });
                            eprintln!(
                                "  ⚠ AGENTS.md regenerated but CLAUDE.md symlink failed: {}",
                                e
                            );
                        }
                    }
                }
                Err(e) => {
                    checks.push(DoctorCheck {
                        scope: ws_scope.into(),
                        check: "agents-md-valid".into(),
                        status: CheckStatus::Warn,
                        message: format!("AGENTS.md/CLAUDE.md issues, fix failed: {}", e),
                        fixable,
                        details: Some(serde_json::json!({ "problems": problems })),
                    });
                    eprintln!("  ⚠ AGENTS.md/CLAUDE.md issues, fix failed: {}", e);
                }
            }
        } else {
            checks.push(DoctorCheck {
                scope: ws_scope.into(),
                check: "agents-md-valid".into(),
                status: CheckStatus::Warn,
                message: problems.join("; "),
                fixable,
                details: Some(serde_json::json!({ "problems": problems })),
            });
            for p in &problems {
                eprintln!("  ⚠ {}", p);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn build_output(checks: Vec<DoctorCheck>, fixed: usize) -> DoctorOutput {
    let total = checks.len();
    let ok_count = checks
        .iter()
        .filter(|c| c.status == CheckStatus::Ok)
        .count();
    let warn_count = checks
        .iter()
        .filter(|c| c.status == CheckStatus::Warn)
        .count();
    let error_count = checks
        .iter()
        .filter(|c| c.status == CheckStatus::Error)
        .count();
    let ok = warn_count == 0 && error_count == 0;

    DoctorOutput {
        ok,
        checks,
        summary: DoctorSummary {
            total,
            ok: ok_count,
            warn: warn_count,
            error: error_count,
            fixed,
        },
    }
}

/// Compare two git URLs for equivalence. Handles SSH vs HTTPS for the same repo.
/// Falls back to string comparison if parsing fails.
fn urls_equivalent(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    // Both parse to same identity → equivalent
    let pa = giturl::parse(a);
    let pb = giturl::parse(b);
    match (pa, pb) {
        (Ok(a), Ok(b)) => a.identity() == b.identity(),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Exit code
// ---------------------------------------------------------------------------

/// Returns the appropriate exit code: 0=ok, 1=any problems found.
pub fn exit_code(output: &DoctorOutput) -> i32 {
    if output.summary.error > 0 || output.summary.warn > 0 {
        1
    } else {
        0
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_equivalent_same_string() {
        assert!(urls_equivalent(
            "git@github.com:acme/repo.git",
            "git@github.com:acme/repo.git"
        ));
    }

    #[test]
    fn urls_equivalent_ssh_vs_https() {
        assert!(urls_equivalent(
            "git@github.com:acme/repo.git",
            "https://github.com/acme/repo"
        ));
    }

    #[test]
    fn urls_equivalent_different_repos() {
        assert!(!urls_equivalent(
            "git@github.com:acme/repo-a.git",
            "git@github.com:acme/repo-b.git"
        ));
    }

    #[test]
    fn build_output_counts() {
        let checks = vec![
            DoctorCheck {
                scope: "global".into(),
                check: "config-parseable".into(),
                status: CheckStatus::Ok,
                message: "ok".into(),
                fixable: false,
                details: None,
            },
            DoctorCheck {
                scope: "ws/foo".into(),
                check: "origin-url-match".into(),
                status: CheckStatus::Warn,
                message: "mismatch".into(),
                fixable: true,
                details: None,
            },
            DoctorCheck {
                scope: "ws/foo".into(),
                check: "repo-dir-exists".into(),
                status: CheckStatus::Error,
                message: "missing".into(),
                fixable: false,
                details: None,
            },
        ];
        let output = build_output(checks, 0);
        assert!(!output.ok);
        assert_eq!(output.summary.total, 3);
        assert_eq!(output.summary.ok, 1);
        assert_eq!(output.summary.warn, 1);
        assert_eq!(output.summary.error, 1);
        assert_eq!(output.summary.fixed, 0);
    }

    #[test]
    fn all_ok_output() {
        let checks = vec![DoctorCheck {
            scope: "global".into(),
            check: "config-parseable".into(),
            status: CheckStatus::Ok,
            message: "ok".into(),
            fixable: false,
            details: None,
        }];
        let output = build_output(checks, 0);
        assert!(output.ok);
    }

    #[test]
    fn exit_code_all_ok() {
        let output = build_output(
            vec![DoctorCheck {
                scope: "global".into(),
                check: "test".into(),
                status: CheckStatus::Ok,
                message: "ok".into(),
                fixable: false,
                details: None,
            }],
            0,
        );
        assert_eq!(exit_code(&output), 0);
    }

    #[test]
    fn exit_code_warnings() {
        let output = build_output(
            vec![DoctorCheck {
                scope: "global".into(),
                check: "test".into(),
                status: CheckStatus::Warn,
                message: "warn".into(),
                fixable: true,
                details: None,
            }],
            0,
        );
        assert_eq!(exit_code(&output), 1);
    }

    #[test]
    fn exit_code_errors() {
        let output = build_output(
            vec![DoctorCheck {
                scope: "global".into(),
                check: "test".into(),
                status: CheckStatus::Error,
                message: "err".into(),
                fixable: false,
                details: None,
            }],
            0,
        );
        assert_eq!(exit_code(&output), 1);
    }

    #[test]
    fn json_serialization() {
        let output = build_output(
            vec![DoctorCheck {
                scope: "global".into(),
                check: "config-parseable".into(),
                status: CheckStatus::Ok,
                message: "config is valid".into(),
                fixable: false,
                details: None,
            }],
            0,
        );
        let json = serde_json::to_string_pretty(&output).unwrap();
        assert!(json.contains("\"ok\": true"));
        assert!(json.contains("\"status\": \"ok\""));
        assert!(!json.contains("\"fixable\"")); // skip_serializing_if = false
        assert!(!json.contains("\"details\"")); // skip_serializing_if = None
    }

    #[test]
    fn json_with_details() {
        let output = build_output(
            vec![DoctorCheck {
                scope: "workspace/foo/bar".into(),
                check: "origin-url-match".into(),
                status: CheckStatus::Warn,
                message: "mismatch".into(),
                fixable: true,
                details: Some(serde_json::json!({
                    "clone_url": "git@github.com:acme/bar.git",
                    "registered_url": "https://github.com/acme/bar",
                })),
            }],
            0,
        );
        let json = serde_json::to_string_pretty(&output).unwrap();
        assert!(json.contains("\"fixable\": true"));
        assert!(json.contains("\"clone_url\""));
        assert!(json.contains("\"registered_url\""));
    }

    #[test]
    fn orphaned_mirrors_detection() {
        let tmp = tempfile::tempdir().unwrap();
        let mirrors_dir = tmp.path().join("mirrors");
        let cfg = config::Config {
            repos: std::collections::BTreeMap::from([(
                "github.com/acme/kept".to_string(),
                config::RepoEntry {
                    url: "git@github.com:acme/kept.git".into(),
                    added: chrono::Utc::now(),
                },
            )]),
            ..Default::default()
        };

        // Create a mirror that's in config
        let kept_dir = mirrors_dir.join("github.com/acme/kept.git");
        fs::create_dir_all(&kept_dir).unwrap();

        // Create a mirror that's orphaned
        let orphan_dir = mirrors_dir.join("github.com/acme/orphan.git");
        fs::create_dir_all(&orphan_dir).unwrap();

        let paths = Paths {
            config_path: tmp.path().join("config.yaml"),
            mirrors_dir,
            gc_dir: tmp.path().join("gc"),
            templates_dir: tmp.path().join("templates"),
            workspaces_dir: tmp.path().join("workspaces"),
        };

        let mut checks = Vec::new();
        let mut fixed = 0;
        check_orphaned_mirrors(&paths, &cfg, false, &mut checks, &mut fixed);

        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].check, "orphaned-mirrors");
        assert_eq!(checks[0].status, CheckStatus::Warn);
        assert!(checks[0].message.contains("orphan"));
    }

    #[test]
    fn orphaned_mirrors_none() {
        let tmp = tempfile::tempdir().unwrap();
        let mirrors_dir = tmp.path().join("mirrors");
        let cfg = config::Config {
            repos: std::collections::BTreeMap::from([(
                "github.com/acme/repo".to_string(),
                config::RepoEntry {
                    url: "git@github.com:acme/repo.git".into(),
                    added: chrono::Utc::now(),
                },
            )]),
            ..Default::default()
        };

        // Only create a mirror that's in config
        let kept_dir = mirrors_dir.join("github.com/acme/repo.git");
        fs::create_dir_all(&kept_dir).unwrap();

        let paths = Paths {
            config_path: tmp.path().join("config.yaml"),
            mirrors_dir,
            gc_dir: tmp.path().join("gc"),
            templates_dir: tmp.path().join("templates"),
            workspaces_dir: tmp.path().join("workspaces"),
        };

        let mut checks = Vec::new();
        let mut fixed = 0;
        check_orphaned_mirrors(&paths, &cfg, false, &mut checks, &mut fixed);

        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].status, CheckStatus::Ok);
    }

    #[test]
    fn orphaned_mirrors_fix() {
        let tmp = tempfile::tempdir().unwrap();
        let mirrors_dir = tmp.path().join("mirrors");
        let cfg = config::Config::default();

        // Create an orphaned mirror
        let orphan_dir = mirrors_dir.join("github.com/acme/orphan.git");
        fs::create_dir_all(&orphan_dir).unwrap();

        let paths = Paths {
            config_path: tmp.path().join("config.yaml"),
            mirrors_dir: mirrors_dir.clone(),
            gc_dir: tmp.path().join("gc"),
            templates_dir: tmp.path().join("templates"),
            workspaces_dir: tmp.path().join("workspaces"),
        };

        let mut checks = Vec::new();
        let mut fixed = 0;
        check_orphaned_mirrors(&paths, &cfg, true, &mut checks, &mut fixed);

        assert_eq!(fixed, 1);
        assert!(!orphan_dir.exists());
    }

    #[test]
    fn gc_stale_entries_none() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths {
            config_path: tmp.path().join("config.yaml"),
            mirrors_dir: tmp.path().join("mirrors"),
            gc_dir: tmp.path().join("gc"),
            templates_dir: tmp.path().join("templates"),
            workspaces_dir: tmp.path().join("workspaces"),
        };
        let cfg = config::Config::default();

        let mut checks = Vec::new();
        let mut fixed = 0;
        check_gc_stale_entries(&paths, &cfg, false, &mut checks, &mut fixed);

        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].status, CheckStatus::Ok);
    }

    #[test]
    fn unregistered_repos_detected() {
        let meta = workspace::Metadata {
            version: 0,
            name: "test".into(),
            branch: "test/branch".into(),
            repos: std::collections::BTreeMap::from([
                ("github.com/acme/known".into(), None),
                ("github.com/acme/unknown".into(), None),
            ]),
            created: chrono::Utc::now(),
            description: None,
            last_used: None,
            created_from: None,
            dirs: std::collections::BTreeMap::new(),
        };
        let cfg = config::Config {
            repos: std::collections::BTreeMap::from([(
                "github.com/acme/known".to_string(),
                config::RepoEntry {
                    url: "git@github.com:acme/known.git".into(),
                    added: chrono::Utc::now(),
                },
            )]),
            ..Default::default()
        };

        let mut checks = Vec::new();
        check_unregistered_repos(&meta, &cfg, "workspace/test", &mut checks);

        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].check, "unregistered-repos");
        assert_eq!(checks[0].status, CheckStatus::Warn);
    }

    #[test]
    fn legacy_ref_field_detected() {
        let meta = workspace::Metadata {
            version: 0,
            name: "test".into(),
            branch: "test/branch".into(),
            repos: std::collections::BTreeMap::from([(
                "github.com/acme/repo".into(),
                Some(workspace::WorkspaceRepoRef {
                    r#ref: "v1.0".into(),
                    url: None,
                }),
            )]),
            created: chrono::Utc::now(),
            description: None,
            last_used: None,
            created_from: None,
            dirs: std::collections::BTreeMap::new(),
        };

        let mut checks = Vec::new();
        let mut fixed = 0;
        // Can't easily test fix without a real workspace dir, so test detection only
        check_legacy_ref_field(
            std::path::Path::new("/nonexistent"),
            &meta,
            "workspace/test",
            false,
            &mut checks,
            &mut fixed,
        );

        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].check, "legacy-ref-field");
        assert_eq!(checks[0].status, CheckStatus::Warn);
    }

    #[test]
    fn legacy_ref_field_clean() {
        let meta = workspace::Metadata {
            version: 0,
            name: "test".into(),
            branch: "test/branch".into(),
            repos: std::collections::BTreeMap::from([
                ("github.com/acme/repo".into(), None),
                (
                    "github.com/acme/repo2".into(),
                    Some(workspace::WorkspaceRepoRef {
                        r#ref: String::new(),
                        url: None,
                    }),
                ),
            ]),
            created: chrono::Utc::now(),
            description: None,
            last_used: None,
            created_from: None,
            dirs: std::collections::BTreeMap::new(),
        };

        let mut checks = Vec::new();
        let mut fixed = 0;
        check_legacy_ref_field(
            std::path::Path::new("/nonexistent"),
            &meta,
            "workspace/test",
            false,
            &mut checks,
            &mut fixed,
        );

        // No stale refs → no check emitted
        assert!(checks.is_empty());
    }

    #[test]
    fn stale_dirs_map_detected() {
        let meta = workspace::Metadata {
            version: 0,
            name: "test".into(),
            branch: "test/branch".into(),
            repos: std::collections::BTreeMap::from([("github.com/acme/repo".into(), None)]),
            created: chrono::Utc::now(),
            description: None,
            last_used: None,
            created_from: None,
            dirs: std::collections::BTreeMap::from([
                ("github.com/acme/repo".into(), "repo".into()),
                ("github.com/acme/removed".into(), "removed".into()),
            ]),
        };

        let mut checks = Vec::new();
        let mut fixed = 0;
        check_stale_dirs_map(
            std::path::Path::new("/nonexistent"),
            &meta,
            "workspace/test",
            false,
            &mut checks,
            &mut fixed,
        );

        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].check, "stale-dirs-map");
        assert_eq!(checks[0].status, CheckStatus::Warn);
    }

    #[test]
    fn stale_dirs_map_clean() {
        let meta = workspace::Metadata {
            version: 0,
            name: "test".into(),
            branch: "test/branch".into(),
            repos: std::collections::BTreeMap::from([("github.com/acme/repo".into(), None)]),
            created: chrono::Utc::now(),
            description: None,
            last_used: None,
            created_from: None,
            dirs: std::collections::BTreeMap::from([(
                "github.com/acme/repo".into(),
                "repo".into(),
            )]),
        };

        let mut checks = Vec::new();
        let mut fixed = 0;
        check_stale_dirs_map(
            std::path::Path::new("/nonexistent"),
            &meta,
            "workspace/test",
            false,
            &mut checks,
            &mut fixed,
        );

        assert!(checks.is_empty());
    }
}

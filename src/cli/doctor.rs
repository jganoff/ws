use anyhow::Result;
use clap::{ArgMatches, Command};
use serde::Serialize;

use crate::config::{self, Paths};
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

    // --- Workspace checks (if inside one) ---
    let cwd = std::env::current_dir()?;
    if let Ok(ws_dir) = workspace::detect(&cwd) {
        let meta = workspace::load_metadata(&ws_dir)?;
        eprintln!("\nChecking workspace {:?}...", meta.name);

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
}

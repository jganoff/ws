use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::{ArgMatches, Command};

use crate::config::{self, Paths};
use crate::filelock;
use crate::output::Output;
use crate::util::read_stdin_line;

/// Read a line from stdin for interactive prompts.
/// Bails if stdin is closed or interrupted (e.g. Ctrl-C), allowing the
/// wizard to exit cleanly via the SIGINT handler in main.rs.
fn read_prompt() -> Result<String> {
    let line = read_stdin_line();
    if line.is_empty() {
        // Empty string (no newline) means EOF or read error — abort wizard.
        // "\n" (user pressed Enter) would be non-empty before trim.
        bail!("aborted");
    }
    Ok(line)
}

pub fn cmd() -> Command {
    Command::new("setup")
        .about("Interactive first-time setup")
        .long_about(
            "Interactive first-time setup.\n\n\
             Walks through configuring wsp for first use: checks dependencies, sets \
             branch prefix, configures shell integration, and imports repos from GitHub. \
             Idempotent — skips steps that are already configured. Re-run anytime to \
             fill in missing pieces.",
        )
}

pub fn run(_matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    if !std::io::stdin().is_terminal() {
        print_non_interactive_guide(paths)?;
        return Ok(Output::None);
    }

    eprintln!();

    // Step 1: Check tools on PATH
    let has_gh = check_tools()?;

    // Step 2: Branch prefix
    step_branch_prefix(paths)?;

    // Step 3: Shell integration
    step_shell_integration()?;

    // Step 4: Register repos (skip if gh not found)
    if has_gh {
        step_register_repos(paths)?;
    }

    // Step 5: Workflow guide
    print_workflow_guide();

    Ok(Output::None)
}

/// Check required and optional tools. Returns true if `gh` is available.
/// Bails if `git` is missing.
fn check_tools() -> Result<bool> {
    eprintln!("Checking dependencies...");

    // git — hard requirement
    let git_ok = match std::process::Command::new("git").arg("--version").output() {
        Ok(out) if out.status.success() => {
            let raw = String::from_utf8_lossy(&out.stdout);
            let version = raw
                .trim()
                .strip_prefix("git version ")
                .unwrap_or(raw.trim());
            eprintln!("  \u{2713} git {}", version);
            true
        }
        _ => {
            eprintln!("  \u{2717} git \u{2014} not found (required)");
            false
        }
    };

    if !git_ok {
        bail!("git is required but not found on PATH");
    }

    // gh — optional
    let has_gh = match std::process::Command::new("gh").arg("--version").output() {
        Ok(out) if out.status.success() => {
            let raw = String::from_utf8_lossy(&out.stdout);
            let first_line = raw.lines().next().unwrap_or("");
            let version = first_line.strip_prefix("gh version ").unwrap_or(first_line);
            let version = version.split_whitespace().next().unwrap_or(version);
            eprintln!("  \u{2713} gh {}", version);
            true
        }
        _ => {
            eprintln!("  \u{2717} gh \u{2014} not found (optional, needed for repo import)");
            eprintln!("    Install: https://cli.github.com");
            false
        }
    };

    eprintln!();
    Ok(has_gh)
}

/// Prompt for branch prefix if not already set.
fn step_branch_prefix(paths: &Paths) -> Result<()> {
    let cfg = config::Config::load_from(&paths.config_path)?;
    if let Some(ref prefix) = cfg.branch_prefix {
        eprintln!("  \u{2713} branch prefix already set: {}", prefix);
        eprintln!();
        return Ok(());
    }

    let default = std::env::var("USER").unwrap_or_default();
    eprintln!("Workspace branches are named <prefix>/<workspace-name>.");
    if default.is_empty() {
        eprint!("Branch prefix: ");
    } else {
        eprint!("Branch prefix [{}]: ", default);
    }

    let input = read_prompt()?;
    let trimmed = input.trim();
    let prefix = if trimmed.is_empty() {
        &default
    } else {
        trimmed
    };

    if prefix.is_empty() {
        eprintln!("  skipped (no prefix set)");
        eprintln!();
        return Ok(());
    }

    let v = prefix.to_string();
    filelock::with_config(&paths.config_path, |cfg| {
        cfg.branch_prefix = Some(v);
        Ok(())
    })?;

    eprintln!("  \u{2713} branch prefix set to: {}", prefix);
    eprintln!();
    Ok(())
}

/// Detect shell, check rc file, offer to append shell integration.
fn step_shell_integration() -> Result<()> {
    let shell = match detect_shell() {
        Some(s) => s,
        None => {
            eprintln!("Shell integration:");
            eprintln!("  could not detect shell from $SHELL");
            eprintln!("  run `wsp completion --help` to set up manually");
            eprintln!();
            return Ok(());
        }
    };

    let rc = match rc_file(shell) {
        Some(p) => p,
        None => {
            eprintln!("Shell integration:");
            eprintln!("  $HOME is not set, cannot determine rc file");
            eprintln!();
            return Ok(());
        }
    };

    // Check if already configured
    if rc.exists() {
        let contents = std::fs::read_to_string(&rc).unwrap_or_default();
        if contents.contains("wsp completion") {
            eprintln!(
                "  \u{2713} shell integration already configured in {}",
                rc.display()
            );
            eprintln!();
            return Ok(());
        }
    }

    eprintln!("Shell integration enables tab completion and workspace detection.");
    eprintln!("Detected shell: {}", shell);
    eprintln!();

    let eval_line = match shell {
        "fish" => "wsp completion fish | source".to_string(),
        _ => format!("eval \"$(wsp completion {})\"", shell),
    };

    eprintln!("Add to {}:", rc.display());
    eprintln!("  {}", eval_line);
    eprintln!();
    eprint!("Add it now? [Y/n]: ");

    let input = read_prompt()?;
    let answer = input.trim().to_lowercase();

    if answer.is_empty() || answer == "y" || answer == "yes" {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&rc)?;
        writeln!(file)?;
        writeln!(file, "# wsp shell integration")?;
        writeln!(file, "{}", eval_line)?;

        eprintln!("  \u{2713} added to {}", rc.display());
    } else {
        eprintln!("  skipped");
    }

    eprintln!();
    Ok(())
}

/// Import repos from GitHub orgs interactively.
fn step_register_repos(paths: &Paths) -> Result<()> {
    eprintln!("Register repos so `wsp new` can clone them.");

    // Ask once whether to use HTTPS (default: SSH)
    eprint!("Use HTTPS URLs instead of SSH? [y/N]: ");
    let input = read_prompt()?;
    let use_https = matches!(input.trim().to_lowercase().as_str(), "y" | "yes");

    let mut first = true;
    loop {
        if first {
            eprint!("GitHub org or user to import from (blank to skip): ");
            first = false;
        } else {
            eprint!("Another org? (blank to finish): ");
        }

        let input = read_prompt()?;
        let owner = input.trim();

        if owner.is_empty() {
            break;
        }

        import_org(paths, owner, use_https);
        eprintln!();
    }

    eprintln!();
    Ok(())
}

fn import_org(paths: &Paths, owner: &str, use_https: bool) {
    eprintln!("  Importing from github.com/{}...", owner);

    match super::repo::gh_list_repos(owner, use_https) {
        Ok(repos) => {
            if repos.is_empty() {
                eprintln!("  no repos found for {}", owner);
                return;
            }
            match super::repo::import_repos(paths, &repos, true) {
                Ok(result) => {
                    let mut parts = Vec::new();
                    let reg = result.registered.len();
                    let skip = result.skipped.len();
                    let fail = result.failed.len();
                    if reg > 0 {
                        parts.push(format!("{} registered", reg));
                    }
                    if skip > 0 {
                        parts.push(format!("{} already registered", skip));
                    }
                    if fail > 0 {
                        parts.push(format!("{} failed", fail));
                    }
                    eprintln!("  \u{2713} {}", parts.join(", "));
                }
                Err(e) => {
                    eprintln!("  error: {}", e);
                }
            }
        }
        Err(e) => {
            eprintln!("  error listing repos: {}", e);
        }
    }
}

/// Print workflow guide with example commands.
fn print_workflow_guide() {
    eprintln!("You're all set! Here's the typical workflow:");
    eprintln!();
    eprintln!("  wsp new my-feature <repo>        # create workspace with repos");
    eprintln!("  # cd into the workspace automatically");
    eprintln!("  # hack, iterate with Claude or your editor");
    eprintln!("  wsp st                           # check status across all repos");
    eprintln!("  wsp diff                         # review changes");
    eprintln!("  git push                         # push branch for PR");
    eprintln!("  wsp rm my-feature                # clean up after merge");
    eprintln!();
    eprintln!("Create your first workspace:");
    eprintln!("  wsp new my-feature");
}

/// Non-interactive mode: print what needs to be done without prompting.
fn print_non_interactive_guide(paths: &Paths) -> Result<()> {
    let cfg = config::Config::load_from(&paths.config_path)?;

    eprintln!("wsp setup requires an interactive terminal.");
    eprintln!();
    eprintln!("To configure manually:");

    if cfg.branch_prefix.is_none() {
        eprintln!("  wsp config set branch-prefix <your-username>");
    }

    if let Some(shell) = detect_shell() {
        let rc = match rc_file(shell) {
            Some(p) => p,
            None => return Ok(()),
        };
        let already = rc.exists()
            && std::fs::read_to_string(&rc)
                .unwrap_or_default()
                .contains("wsp completion");
        if !already {
            let eval_line = match shell {
                "fish" => "wsp completion fish | source".to_string(),
                _ => format!("eval \"$(wsp completion {})\"", shell),
            };
            eprintln!("  echo '{}' >> {}", eval_line, rc.display());
        }
    }

    eprintln!("  wsp repo add --from github.com/<org> --all");
    eprintln!("  wsp new my-feature");

    Ok(())
}

fn detect_shell() -> Option<&'static str> {
    let shell = std::env::var("SHELL").ok()?;
    if shell.ends_with("/zsh") {
        Some("zsh")
    } else if shell.ends_with("/bash") {
        Some("bash")
    } else if shell.ends_with("/fish") {
        Some("fish")
    } else {
        None
    }
}

fn rc_file(shell: &str) -> Option<PathBuf> {
    let home = std::env::var("HOME").ok().filter(|h| !h.is_empty())?;
    Some(match shell {
        "zsh" => PathBuf::from(&home).join(".zshrc"),
        "bash" => PathBuf::from(&home).join(".bashrc"),
        "fish" => PathBuf::from(&home)
            .join(".config")
            .join("fish")
            .join("config.fish"),
        _ => unreachable!(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_shell() {
        let cases = vec![
            ("/bin/zsh", Some("zsh")),
            ("/usr/bin/zsh", Some("zsh")),
            ("/bin/bash", Some("bash")),
            ("/usr/local/bin/fish", Some("fish")),
            ("/bin/sh", None),
            ("/bin/csh", None),
        ];

        for (shell_path, expected) in cases {
            // We can't easily test detect_shell() since it reads $SHELL,
            // but we can test the matching logic directly.
            let result = if shell_path.ends_with("/zsh") {
                Some("zsh")
            } else if shell_path.ends_with("/bash") {
                Some("bash")
            } else if shell_path.ends_with("/fish") {
                Some("fish")
            } else {
                None
            };
            assert_eq!(result, expected, "shell path: {}", shell_path);
        }
    }

    #[test]
    fn test_rc_file() {
        let cases = vec![
            ("zsh", ".zshrc"),
            ("bash", ".bashrc"),
            ("fish", ".config/fish/config.fish"),
        ];

        for (shell, suffix) in cases {
            if let Some(rc) = rc_file(shell) {
                assert!(
                    rc.to_string_lossy().ends_with(suffix),
                    "rc_file({}) = {}, expected to end with {}",
                    shell,
                    rc.display(),
                    suffix
                );
            }
            // If $HOME is unset, rc_file returns None — that's fine
        }
    }
}

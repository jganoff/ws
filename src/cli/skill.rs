use std::fs;

use anyhow::Result;
use clap::{ArgMatches, Command};

use crate::config::Paths;
use crate::output::{MutationOutput, Output};

const SKILL_CONTENT: &str = include_str!("../../skills/wsp-manage/SKILL.md");

pub fn install_cmd() -> Command {
    Command::new("install").about("Install wsp Claude Code skill to ~/.claude/skills/")
}

pub fn run_install(_matches: &ArgMatches, _paths: &Paths) -> Result<Output> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
    let skill_dir = home.join(".claude").join("skills").join("wsp-manage");
    fs::create_dir_all(&skill_dir)?;

    let skill_path = skill_dir.join("SKILL.md");
    fs::write(&skill_path, SKILL_CONTENT)?;

    Ok(Output::Mutation(MutationOutput {
        ok: true,
        message: format!("Installed skill to {}", skill_path.display()),
    }))
}

// ---------------------------------------------------------------------------
// SKILL.md generation (codegen only)
// ---------------------------------------------------------------------------

#[cfg(feature = "codegen")]
pub fn generate_cmd() -> Command {
    Command::new("generate").about("Generate SKILL.md from CLI introspection (dev only)")
}

#[cfg(feature = "codegen")]
pub fn run_generate(_matches: &ArgMatches, _paths: &Paths) -> Result<Output> {
    use crate::output::{
        ConfigGetOutput, ConfigListOutput, DiffOutput, ErrorOutput, FetchOutput, GroupListOutput,
        GroupShowOutput, LogOutput, MutationOutput, RepoListOutput, StatusOutput, SyncOutput,
        WorkspaceListOutput, WorkspaceRepoListOutput,
    };

    let cli = super::build_cli();
    let mut out = String::new();

    // --- Front-matter ---
    out.push_str(FRONT_MATTER);

    // --- Quick Reference: introspected from clap ---
    out.push_str("## Quick Reference\n\n");

    // Repos (global registry) — under `setup repo`
    out.push_str("### Repos (global registry)\n\n```bash\n");
    write_setup_section(&cli, &mut out, "repo", &["wsp", "setup", "repo"], None);
    out.push_str("```\n\n");

    // Groups — under `setup group`
    out.push_str("### Groups (named sets of repos)\n\n```bash\n");
    write_setup_section(&cli, &mut out, "group", &["wsp", "setup", "group"], None);
    out.push_str("```\n\n");

    // Workspaces — top-level workspace commands + `repo` subcommands
    out.push_str("### Workspaces\n\n```bash\n");
    let ws_cmds = ["new", "ls", "st", "diff", "log", "sync", "exec", "cd", "rm"];
    for name in &ws_cmds {
        if let Some(sub) = cli.find_subcommand(name) {
            write_cmd_line(&mut out, &["wsp"], sub);
        }
    }
    // Workspace-scoped repo commands
    if let Some(repo) = cli.find_subcommand("repo") {
        for sub in repo.get_subcommands() {
            write_cmd_line(&mut out, &["wsp", "repo"], sub);
        }
    }
    out.push_str("```\n\n");

    // Config — under `setup config`
    out.push_str("### Config\n\n```bash\n");
    write_setup_section(&cli, &mut out, "config", &["wsp", "setup", "config"], None);
    out.push_str("```\n\n");

    // Skill management
    out.push_str("### Skill management\n\n```bash\n");
    write_setup_section(
        &cli,
        &mut out,
        "skill",
        &["wsp", "setup", "skill"],
        Some(&["generate"]),
    );
    out.push_str("```\n\n");

    // --- JSON Output Schemas ---
    out.push_str("## JSON Output Schemas\n\n");

    write_schema::<RepoListOutput>(&mut out, "wsp setup repo list --json");
    write_schema::<WorkspaceListOutput>(&mut out, "wsp ls --json");
    write_schema::<StatusOutput>(&mut out, "wsp st --json");
    write_schema::<DiffOutput>(&mut out, "wsp diff --json");
    write_schema::<LogOutput>(&mut out, "wsp log --json");
    write_schema::<SyncOutput>(&mut out, "wsp sync --json");
    write_schema::<WorkspaceRepoListOutput>(&mut out, "wsp repo ls --json");
    write_schema::<FetchOutput>(&mut out, "wsp repo fetch --json");
    write_schema::<GroupListOutput>(&mut out, "wsp setup group list --json");
    write_schema::<GroupShowOutput>(&mut out, "wsp setup group show <name> --json");
    write_schema::<ConfigListOutput>(&mut out, "wsp setup config list --json");
    write_schema::<ConfigGetOutput>(&mut out, "wsp setup config get <key> --json");
    write_schema::<MutationOutput>(
        &mut out,
        "Mutation commands (new, rm, add, remove, set, etc.)",
    );
    write_schema::<ErrorOutput>(&mut out, "Errors");

    // --- Static reference sections ---
    out.push_str(REFERENCE_SECTIONS);

    print!("{}", out);
    Ok(Output::None)
}

#[cfg(feature = "codegen")]
trait Sample {
    fn sample() -> Self;
}

#[cfg(feature = "codegen")]
macro_rules! impl_sample {
    ($($t:ty),+ $(,)?) => {
        $(impl Sample for $t {
            fn sample() -> Self { <$t>::sample() }
        })+
    };
}

#[cfg(feature = "codegen")]
impl_sample!(
    crate::output::RepoListOutput,
    crate::output::GroupListOutput,
    crate::output::GroupShowOutput,
    crate::output::WorkspaceListOutput,
    crate::output::StatusOutput,
    crate::output::DiffOutput,
    crate::output::LogOutput,
    crate::output::SyncOutput,
    crate::output::ConfigListOutput,
    crate::output::ConfigGetOutput,
    crate::output::WorkspaceRepoListOutput,
    crate::output::FetchOutput,
    crate::output::MutationOutput,
    crate::output::ErrorOutput,
);

#[cfg(feature = "codegen")]
fn write_schema<T: Sample + serde::Serialize>(out: &mut String, heading: &str) {
    use std::fmt::Write;
    let sample = T::sample();
    let json = serde_json::to_string_pretty(&sample).expect("sample serialization");
    writeln!(out, "### `{}`\n```json\n{}\n```\n", heading, json).unwrap();
}

#[cfg(feature = "codegen")]
fn write_cmd_line(out: &mut String, prefix: &[&str], cmd: &Command) {
    use std::fmt::Write;

    let name = cmd.get_name();
    let about = cmd.get_about().map(|a| a.to_string()).unwrap_or_default();

    // Build the usage string: prefix + name + args + flags
    let mut usage = prefix.join(" ");
    write!(usage, " {}", name).unwrap();

    for arg in cmd.get_arguments() {
        let id = arg.get_id().as_str();
        if id == "json" || id == "help" || id == "version" {
            continue;
        }
        if arg.is_positional() {
            if arg.is_required_set() {
                write!(usage, " <{}>", id).unwrap();
            } else {
                write!(usage, " [<{}>]", id).unwrap();
            }
            if let Some(num_args) = arg.get_num_args()
                && num_args.max_values() > 1
            {
                usage.push_str("...");
            }
        } else {
            // Named flags/options
            let long = arg
                .get_long()
                .map(|l| format!("--{}", l))
                .unwrap_or_default();
            let short = arg.get_short().map(|s| format!("-{}", s));
            let flag_name = short.unwrap_or(long);
            if flag_name.is_empty() {
                continue;
            }
            if arg.get_action().takes_values() {
                write!(usage, " [{} <{}>]", flag_name, id).unwrap();
                if let Some(num_args) = arg.get_num_args()
                    && num_args.max_values() > 1
                {
                    usage.push_str("...");
                }
            } else {
                write!(usage, " [{}]", flag_name).unwrap();
            }
        }
    }

    // Visible aliases
    let aliases: Vec<&str> = cmd.get_visible_aliases().collect();
    let alias_suffix = if aliases.is_empty() {
        String::new()
    } else {
        format!(" (alias: {})", aliases.join(", "))
    };

    let pad = 48usize.saturating_sub(usage.len()).max(1);
    writeln!(
        out,
        "{}{}# {}{}",
        usage,
        " ".repeat(pad),
        about,
        alias_suffix
    )
    .unwrap();
}

/// Write all subcommands of a `setup <section>` group, optionally skipping some.
#[cfg(feature = "codegen")]
fn write_setup_section(
    cli: &Command,
    out: &mut String,
    section: &str,
    prefix: &[&str],
    skip: Option<&[&str]>,
) {
    if let Some(setup) = cli.find_subcommand("setup")
        && let Some(group) = setup.find_subcommand(section)
    {
        for sub in group.get_subcommands() {
            if let Some(skip) = skip
                && skip.contains(&sub.get_name())
            {
                continue;
            }
            write_cmd_line(out, prefix, sub);
        }
    }
}

// ---------------------------------------------------------------------------
// Static prose sections
// ---------------------------------------------------------------------------

#[cfg(feature = "codegen")]
const FRONT_MATTER: &str = r#"---
name: wsp-manage
description: Manage multi-repo workspaces with wsp
user_invocable: true
---

# wsp — Multi-Repo Workspace Manager

Use `wsp` to manage workspaces that span multiple git repositories. Each workspace creates local clones from bare mirror clones, sharing a single branch name across repos.

**Always use `--json` when calling wsp programmatically.** JSON output goes to stdout; progress messages go to stderr.

"#;

#[cfg(feature = "codegen")]
const REFERENCE_SECTIONS: &str = r#"## Shortname Resolution

Repos are identified by `host/owner/repo` (e.g., `github.com/acme/api-gateway`). You can use the shortest unique suffix:
- `api-gateway` if unambiguous
- `acme/api-gateway` to disambiguate from `other-org/api-gateway`

## `@ref` Syntax for Context Repos

When creating a workspace, pin a repo to a specific branch/tag/SHA:
```bash
wsp new my-feature api-gateway user-service@main proto@v1.0
```
- `api-gateway` — active repo, gets the workspace branch
- `user-service@main` — context repo, checked out at `main`
- `proto@v1.0` — context repo, checked out at tag `v1.0`

## Directory Layout

```
~/dev/workspaces/<workspace-name>/
  .wsp.yaml              # Workspace metadata
  <repo-name>/          # Local clone for each repo
```

## Common Agent Workflows

### Create a workspace and start working
```bash
wsp setup repo list --json                     # See available repos
wsp new my-feature api-gateway user-service    # Create workspace
cd ~/dev/workspaces/my-feature                # Enter workspace
```

### Check what's changed
```bash
wsp st --json          # From inside a workspace
wsp diff --json        # See all diffs
```

### Sync with upstream
```bash
wsp sync --json        # Fetch + rebase all repos
wsp sync --strategy merge --json  # Use merge instead of rebase
```

### Run tests across all repos
```bash
wsp exec my-feature -- make test
```

### Clean up when done
```bash
wsp rm my-feature      # Removes clones + branch (if merged)
wsp rm my-feature -f   # Force remove even if unmerged
```
"#;

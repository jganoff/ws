use clap::{Arg, Command};
use serde::Serialize;

use crate::output::Output;

/// Built-in help topics. Each is (name, short description, full text).
const TOPICS: &[(&str, &str, &str)] = &[
    (
        "wspignore",
        "Suppress files from workspace root checks",
        "\
wspignore — suppress files from workspace root checks

`wsp st` checks the workspace root directory for files that aren't managed
by wsp (repos, .wsp.yaml, etc.). OS noise, editor configs, and other
harmless files can be suppressed with wspignore patterns.

PATTERN SYNTAX

  One pattern per line. Blank lines and lines starting with # are ignored.

  .DS_Store         exact filename match
  .claude/          trailing / matches a directory and everything inside it

IGNORE FILES

  There are two wspignore files, checked in order:

  1. Global:        ~/.local/share/wsp/wspignore
                    Created automatically on first use with sensible defaults
                    (.DS_Store, Thumbs.db, etc.). Edit to add patterns that
                    apply to all workspaces.

  2. Per-workspace: <workspace-root>/.wspignore
                    Patterns specific to a single workspace.

  Patterns from both files are merged. A match in either file suppresses
  the path.

EXAMPLES

  # Suppress all Claude Code local settings
  .claude/settings.local.json

  # Suppress an entire directory
  .idea/

  # Suppress a one-off file in this workspace
  echo 'notes.md' >> .wspignore
",
    ),
    (
        "config",
        "Configuration keys and their effects",
        "\
config — configuration keys and their effects

All settings are stored in ~/.local/share/wsp/config.yaml. Manage them
with `wsp config ls`, `wsp config get <key>`, `wsp config set <key> <value>`,
and `wsp config unset <key>`.

GENERAL

  branch-prefix         String. Prefix prepended to workspace branch names.
                        Example: `jganoff` → branch `jganoff/my-feature`.
                        Default: not set (branches are just the workspace name).

  workspaces-dir        Absolute path. Where workspaces are created.
                        Default: ~/dev/workspaces

  sync-strategy         `rebase` or `merge`. How `wsp sync` integrates upstream.
                        Default: rebase

  agent-md              Boolean. Generate AGENTS.md (+ CLAUDE.md symlink) in
                        workspace roots. Provides context for AI agents.
                        Default: true

GC (GARBAGE COLLECTION)

  gc.retention-days     Integer (≥1). How many days `wsp rm` keeps deleted
                        workspaces recoverable via `wsp recover`.
                        Default: 7

GIT CONFIG

  git_config.<key>      Override git config applied to every clone. The key
                        is any valid git config key (e.g., push.default).
                        These merge with built-in defaults:

                          push.autoSetupRemote  true
                          push.default          current
                          rerere.enabled        true
                          branch.sort           -committerdate

                        Example: `wsp config set git_config.merge.conflictstyle zdiff3`
                        Unset reverts to the built-in default (if any).

LANGUAGE INTEGRATIONS

  language-integrations.<name>
                        Boolean. Enable/disable per-language workspace support.
                        Available: go (generates go.work for multi-module repos).
                        Default: false

EXPERIMENTAL

  experimental          Boolean. Top-level gate for unstable features. When false,
                        experimental features are hidden from config ls and tab
                        completion. Must be true for any experimental.* flag to
                        take effect.
                        Default: false

  experimental.shell-prompt
                        Boolean. Emit a shell hook that sets the WSP_WORKSPACE
                        environment variable to the current workspace name.
                        Use in your prompt: PS1='${WSP_WORKSPACE:+[wsp:$WSP_WORKSPACE] }%~ $ '
                        Requires re-sourcing: eval \"$(wsp completion zsh)\"
                        Default: false

  experimental.shell-tmux-title
                        Boolean. Emit a shell hook that sets the tmux pane/window
                        title to `wsp:<workspace>` when inside a workspace.
                        Clears the title when outside. Only active when $TMUX is set.
                        Requires re-sourcing: eval \"$(wsp completion zsh)\"
                        Default: false

EXAMPLES

  wsp config ls                                   # show all settings
  wsp config set branch-prefix jganoff            # prefix branches
  wsp config set sync-strategy merge              # use merge instead of rebase
  wsp config set gc.retention-days 30             # keep deleted workspaces 30 days
  wsp config set git_config.merge.conflictstyle zdiff3
  wsp config set experimental.shell-prompt true   # enable prompt variable
  wsp config unset branch-prefix                  # revert to default
",
    ),
];

#[derive(Serialize)]
struct HelpTopicOutput {
    name: String,
    summary: String,
    text: String,
}

#[derive(Serialize)]
struct HelpTopicListOutput {
    topics: Vec<HelpTopicSummary>,
}

#[derive(Serialize)]
struct HelpTopicSummary {
    name: String,
    summary: String,
}

pub fn cmd() -> Command {
    Command::new("help")
        .about("Display help for a command or topic [read-only]")
        .long_about(
            "Display help for a command or topic.\n\n\
             Without arguments, shows the top-level help. With a topic argument, shows \
             detailed documentation for that topic. Use `wsp help -g` to list \
             available guides.",
        )
        .arg(Arg::new("topic").help("Command name or help topic"))
        .arg(
            Arg::new("guides")
                .short('g')
                .long("guides")
                .action(clap::ArgAction::SetTrue)
                .help("List available concept guides"),
        )
}

pub fn run(matches: &clap::ArgMatches, cli: &mut Command, json: bool) -> anyhow::Result<Output> {
    if matches.get_flag("guides") {
        if json {
            let out = HelpTopicListOutput {
                topics: TOPICS
                    .iter()
                    .map(|(name, desc, _)| HelpTopicSummary {
                        name: name.to_string(),
                        summary: desc.to_string(),
                    })
                    .collect(),
            };
            println!("{}", serde_json::to_string_pretty(&out)?);
        } else {
            println!("Available guides:\n");
            for (name, desc, _) in TOPICS {
                println!("  {:16}{}", name, desc);
            }
            println!("\nUse `wsp help <guide>` for details.");
        }
        return Ok(Output::None);
    }

    let topic = match matches.get_one::<String>("topic") {
        Some(t) => t,
        None => {
            cli.print_long_help()?;
            eprintln!(
                "\n'wsp help -g' lists available concept guides.\n\
                 See 'wsp help <command>' or 'wsp help <guide>' for details."
            );
            return Ok(Output::None);
        }
    };

    // Check built-in topics first
    for (name, summary, text) in TOPICS {
        if *name == topic.as_str() {
            if json {
                let out = HelpTopicOutput {
                    name: name.to_string(),
                    summary: summary.to_string(),
                    text: text.to_string(),
                };
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                print!("{}", text);
            }
            return Ok(Output::None);
        }
    }

    // Fall back to subcommand --help (text only — clap doesn't support JSON help)
    if let Some(mut sub) = cli.find_subcommand(topic).cloned() {
        sub.print_long_help()?;
        return Ok(Output::None);
    }

    anyhow::bail!(
        "no help topic or command named {:?}. Use `wsp help -g` to list guides.",
        topic
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_topics_have_content() {
        for (name, summary, text) in TOPICS {
            assert!(!name.is_empty(), "topic name must not be empty");
            assert!(!summary.is_empty(), "topic {:?} has empty summary", name);
            assert!(!text.is_empty(), "topic {:?} has empty text", name);
        }
    }

    #[test]
    fn test_topic_lookup() {
        let found = TOPICS.iter().find(|(name, _, _)| *name == "wspignore");
        assert!(found.is_some(), "wspignore topic should exist");
        let (_, _, text) = found.unwrap();
        assert!(text.contains("PATTERN SYNTAX"));
        assert!(text.contains("IGNORE FILES"));
        assert!(text.contains("EXAMPLES"));
    }

    #[test]
    fn test_topic_not_found() {
        let found = TOPICS.iter().find(|(name, _, _)| *name == "nonexistent");
        assert!(found.is_none());
    }

    #[test]
    fn test_help_topic_json_serialization() {
        let out = HelpTopicOutput {
            name: "test".to_string(),
            summary: "A test topic".to_string(),
            text: "Full text here.".to_string(),
        };
        let json = serde_json::to_string_pretty(&out).unwrap();
        assert!(json.contains("\"name\": \"test\""));
        assert!(json.contains("\"summary\": \"A test topic\""));
        assert!(json.contains("\"text\": \"Full text here.\""));
    }

    #[test]
    fn test_help_topic_list_json_serialization() {
        let out = HelpTopicListOutput {
            topics: vec![HelpTopicSummary {
                name: "foo".to_string(),
                summary: "bar".to_string(),
            }],
        };
        let json = serde_json::to_string_pretty(&out).unwrap();
        assert!(json.contains("\"name\": \"foo\""));
    }
}

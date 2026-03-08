pub mod add;
pub mod cd;
pub mod cfg;
pub mod completers;
pub mod completion;
pub mod delete;
pub mod describe;
pub mod diff;
pub mod exec;
pub mod fetch;
pub mod group;
pub mod list;
pub mod log;
pub mod man;
pub mod new;
pub mod recover;
pub mod registry;
pub mod remove;
pub mod rename;
pub mod repo;
pub mod repo_list;
pub mod skill;
pub mod status;
pub mod sync;
pub mod template;

use clap::{Arg, ArgMatches, Command};

use crate::config::Paths;
use crate::output::Output;
use crate::workspace;

/// Command categories for `--help` output. Each entry is (heading, [command_names]).
/// Command categories for `--help`, ordered by workflow stage.
const HELP_CATEGORIES: &[(&str, &[&str])] = &[
    (
        "Workspace",
        &[
            "new", "repo", "cd", "ls", "rename", "describe", "rm", "recover",
        ],
    ),
    ("Workflow", &["st", "diff", "log", "sync", "exec"]),
    (
        "Admin",
        &["registry", "group", "template", "config", "completion"],
    ),
];

pub fn build_cli() -> Command {
    let repo_ws = Command::new("repo")
        .about("Manage repos in the current workspace")
        .subcommand_required(true)
        .subcommand(add::cmd())
        .subcommand(remove::cmd())
        .subcommand(fetch::cmd())
        .subcommand(repo_list::cmd());

    // Hidden backward-compat alias: `wsp setup <noun>` dispatches to
    // the new top-level nouns with a deprecation warning.
    let setup = Command::new("setup")
        .about("Deprecated: use top-level registry/group/config/completion commands")
        .hide(true)
        .subcommand_required(true)
        .subcommand(
            Command::new("repo")
                .about("Manage registered repositories")
                .subcommand_required(true)
                .subcommand(repo::add_cmd())
                .subcommand(repo::list_cmd())
                .subcommand(repo::rm_cmd()),
        )
        .subcommand(group::cmd())
        .subcommand(cfg::cmd())
        .subcommand(completion::cmd());

    #[allow(unused_mut)]
    let mut cli = Command::new("wsp")
        .about("Multi-repo workspace manager")
        .version(env!("WSP_VERSION_STRING"))
        .arg(
            Arg::new("json")
                .long("json")
                .global(true)
                .action(clap::ArgAction::SetTrue)
                .help("Output as JSON"),
        )
        // Workspace commands
        .subcommand(new::cmd())
        .subcommand(delete::cmd())
        .subcommand(list::cmd())
        .subcommand(status::cmd())
        .subcommand(diff::cmd())
        .subcommand(log::cmd())
        .subcommand(sync::cmd())
        .subcommand(exec::cmd())
        .subcommand(cd::cmd())
        .subcommand(recover::cmd())
        .subcommand(rename::cmd())
        .subcommand(describe::cmd())
        // Workspace-scoped repo commands
        .subcommand(repo_ws)
        // Admin commands (promoted from `wsp setup`)
        .subcommand(registry::cmd())
        .subcommand(group::cmd())
        .subcommand(template::cmd())
        .subcommand(cfg::cmd())
        .subcommand(completion::cmd())
        // Hidden backward-compat
        .subcommand(setup);

    #[cfg(feature = "codegen")]
    {
        cli = cli.subcommand(skill::generate_cmd().hide(true));
        cli = cli.subcommand(man::generate_man_cmd().hide(true));
    }

    // Build categorized help from the command definitions, then set
    // a custom help_template that replaces clap's flat subcommand list.
    let categorized = build_categorized_help(&cli);
    cli.help_template("{about-with-newline}\n{usage-heading} {usage}\n\n{options}\n{after-help}")
        .after_help(categorized)
}

/// Build categorized help text by introspecting subcommand about strings.
fn build_categorized_help(cli: &Command) -> String {
    use std::fmt::Write;

    let mut out = String::new();

    for (heading, names) in HELP_CATEGORIES {
        writeln!(out, "{}:", heading).unwrap();
        for name in *names {
            if let Some(sub) = cli.find_subcommand(name) {
                let about = sub.get_about().map(|a| a.to_string()).unwrap_or_default();
                let aliases: Vec<&str> = sub.get_visible_aliases().collect();
                let alias_suffix = if aliases.is_empty() {
                    String::new()
                } else {
                    format!(" [aliases: {}]", aliases.join(", "))
                };
                writeln!(out, "  {:12}{}{}", name, about, alias_suffix).unwrap();
            }
        }
        out.push('\n');
    }

    // Trim trailing newline
    while out.ends_with('\n') {
        out.pop();
    }

    out
}

pub fn dispatch(matches: &ArgMatches, paths: &Paths) -> anyhow::Result<Output> {
    match matches.subcommand() {
        // --- Backward-compat: `wsp setup` dispatches with deprecation warnings ---
        Some(("setup", sub)) => dispatch_setup(sub, paths),

        // --- Workspace-scoped repo commands ---
        Some(("repo", sub)) => match sub.subcommand() {
            Some(("add", m)) => add::run(m, paths),
            Some(("rm", m)) => remove::run(m, paths),
            Some(("fetch", m)) => fetch::run(m, paths),
            Some(("ls", m)) => repo_list::run(m, paths),
            _ => unreachable!(),
        },

        // --- Workspace commands ---
        Some(("new", m)) => new::run(m, paths),
        Some(("rm", m)) => delete::run(m, paths),
        Some(("cd", m)) => cd::run(m, paths),
        Some(("ls", m)) => list::run(m, paths),
        Some(("st", m)) => status::run(m, paths),
        Some(("diff", m)) => diff::run(m, paths),
        Some(("log", m)) => log::run(m, paths),
        Some(("sync", m)) => sync::run(m, paths),
        Some(("exec", m)) => exec::run(m, paths),
        Some(("recover", m)) => recover::run(m, paths),
        Some(("rename", m)) => rename::run(m, paths),
        Some(("describe", m)) => describe::run(m, paths),

        // --- Admin commands (promoted from setup) ---
        Some(("registry", sub)) => registry::dispatch(sub, paths),
        Some(("group", sub)) => group::dispatch(sub, paths),
        Some(("template", sub)) => template::dispatch(sub, paths),
        Some(("config", sub)) => cfg::dispatch(sub, paths),
        Some(("completion", m)) => completion::run(m, paths),

        // --- Dev-only codegen ---
        #[cfg(feature = "codegen")]
        Some(("generate", m)) => skill::run_generate(m, paths),
        #[cfg(feature = "codegen")]
        Some(("generate-man", m)) => man::run_generate_man(m, paths),

        // --- No subcommand: default behavior ---
        None => {
            let cwd = std::env::current_dir()?;
            if workspace::detect(&cwd).is_ok() {
                status::run(matches, paths)
            } else {
                let mut output = list::run(matches, paths)?;
                if let Output::WorkspaceList(ref mut wl) = output {
                    wl.hint =
                        Some("Not in a workspace. Use `wsp cd <name>` to enter one.".to_string());
                }
                Ok(output)
            }
        }
        _ => unreachable!(),
    }
}

/// Dispatch `wsp setup <noun>` with deprecation warnings on stderr.
fn dispatch_setup(sub: &ArgMatches, paths: &Paths) -> anyhow::Result<Output> {
    match sub.subcommand() {
        Some(("repo", sub2)) => {
            eprintln!("warning: `wsp setup repo` is deprecated, use `wsp registry` instead");
            match sub2.subcommand() {
                Some(("add", m)) => repo::run_add(m, paths),
                Some(("ls", m)) => repo::run_list(m, paths),
                Some(("rm", m)) => repo::run_remove(m, paths),
                _ => unreachable!(),
            }
        }
        Some(("group", sub2)) => {
            eprintln!("warning: `wsp setup group` is deprecated, use `wsp group` instead");
            group::dispatch(sub2, paths)
        }
        Some(("config", sub2)) => {
            eprintln!("warning: `wsp setup config` is deprecated, use `wsp config` instead");
            cfg::dispatch(sub2, paths)
        }
        Some(("completion", m)) => {
            eprintln!(
                "warning: `wsp setup completion` is deprecated, use `wsp completion` instead"
            );
            completion::run(m, paths)
        }
        _ => unreachable!(),
    }
}

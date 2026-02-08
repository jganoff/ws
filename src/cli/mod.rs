pub mod add;
pub mod completion;
pub mod exec;
pub mod group;
pub mod list;
pub mod new;
pub mod remove;
pub mod repo;
pub mod status;

use clap::{Arg, Command};

pub fn build_cli() -> Command {
    let repo = Command::new("repo")
        .about("Manage registered repositories")
        .subcommand_required(true)
        .subcommand(repo::add_cmd())
        .subcommand(repo::list_cmd())
        .subcommand(repo::remove_cmd())
        .subcommand(repo::fetch_cmd());

    let group = Command::new("group")
        .about("Manage repo groups")
        .subcommand_required(true)
        .subcommand(group::new_cmd())
        .subcommand(group::list_cmd())
        .subcommand(group::show_cmd())
        .subcommand(group::delete_cmd());

    Command::new("ws")
        .about("Multi-repo workspace manager")
        .subcommand_required(true)
        .subcommand(repo)
        .subcommand(group)
        .subcommand(new::cmd())
        .subcommand(add::cmd())
        .subcommand(list::cmd())
        .subcommand(status::cmd())
        .subcommand(remove::cmd())
        .subcommand(exec::cmd())
        .subcommand(
            Command::new("completion")
                .about("Output shell integration (completions + wrapper function)")
                .hide(true)
                .arg(Arg::new("shell").required(true).value_parser(["zsh"])),
        )
}

pub fn run() -> anyhow::Result<()> {
    let app = build_cli();
    let matches = app.get_matches();

    match matches.subcommand() {
        Some(("repo", sub)) => match sub.subcommand() {
            Some(("add", m)) => repo::run_add(m),
            Some(("list", m)) => repo::run_list(m),
            Some(("remove", m)) => repo::run_remove(m),
            Some(("fetch", m)) => repo::run_fetch(m),
            _ => unreachable!(),
        },
        Some(("group", sub)) => match sub.subcommand() {
            Some(("new", m)) => group::run_new(m),
            Some(("list", m)) => group::run_list(m),
            Some(("show", m)) => group::run_show(m),
            Some(("delete", m)) => group::run_delete(m),
            _ => unreachable!(),
        },
        Some(("new", m)) => new::run(m),
        Some(("add", m)) => add::run(m),
        Some(("list", m)) => list::run(m),
        Some(("status", m)) => status::run(m),
        Some(("remove", m)) => remove::run(m),
        Some(("exec", m)) => exec::run(m),
        Some(("completion", m)) => completion::run(m),
        _ => unreachable!(),
    }
}

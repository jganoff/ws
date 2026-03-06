use anyhow::Result;
use clap::{ArgMatches, Command};

use crate::config::Paths;
use crate::output::Output;

use super::repo;

pub fn cmd() -> Command {
    Command::new("registry")
        .about("Manage the global repo registry")
        .subcommand_required(true)
        .subcommand(repo::add_cmd())
        .subcommand(repo::list_cmd())
        .subcommand(repo::rm_cmd())
}

pub fn dispatch(matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    match matches.subcommand() {
        Some(("add", m)) => repo::run_add(m, paths),
        Some(("ls", m)) => repo::run_list(m, paths),
        Some(("rm", m)) => repo::run_remove(m, paths),
        _ => unreachable!(),
    }
}

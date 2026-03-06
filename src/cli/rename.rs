use anyhow::Result;
use clap::{Arg, ArgMatches, Command};
use clap_complete::engine::ArgValueCandidates;

use crate::config::Paths;
use crate::output::{MutationOutput, Output};
use crate::workspace;

use super::completers;

pub fn cmd() -> Command {
    Command::new("rename")
        .about("Rename a workspace, its directory, and git branches")
        .arg(
            Arg::new("old")
                .required(true)
                .add(ArgValueCandidates::new(completers::complete_workspaces)),
        )
        .arg(Arg::new("new").required(true))
}

pub fn run(matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    let old_name = matches.get_one::<String>("old").unwrap();
    let new_name = matches.get_one::<String>("new").unwrap();

    let results = workspace::rename(paths, old_name, new_name)?;

    let mut lines = vec![format!(
        "Renamed workspace {:?} -> {:?}",
        old_name, new_name
    )];
    for r in &results {
        if r.skipped {
            lines.push(format!("  {}    (context repo, skipped)", r.name));
        } else {
            lines.push(format!(
                "  {}    branch: {} -> {}",
                r.name, r.old_branch, r.new_branch,
            ));
        }
    }

    Ok(Output::Mutation(MutationOutput::new(lines.join("\n"))))
}

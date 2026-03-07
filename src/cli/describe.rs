use anyhow::{Result, bail};
use clap::{Arg, ArgMatches, Command};
use clap_complete::engine::ArgValueCandidates;

use crate::config::Paths;
use crate::filelock;
use crate::output::{MutationOutput, Output};
use crate::workspace;

use super::completers;

pub fn cmd() -> Command {
    Command::new("describe")
        .about("Set or update a workspace description")
        .arg(
            Arg::new("workspace")
                .required(true)
                .add(ArgValueCandidates::new(completers::complete_workspaces)),
        )
        .arg(Arg::new("text").required(true))
}

pub fn run(matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    let ws_name = matches.get_one::<String>("workspace").unwrap();
    let text = matches.get_one::<String>("text").unwrap();

    workspace::validate_name(ws_name)?;
    let ws_dir = workspace::dir(&paths.workspaces_dir, ws_name);
    if !ws_dir.join(workspace::METADATA_FILE).exists() {
        bail!("workspace '{}' not found", ws_name);
    }

    let desc = if text.is_empty() {
        None
    } else {
        Some(text.clone())
    };

    filelock::with_metadata(&ws_dir, |meta| {
        meta.description = desc;
        Ok(())
    })?;

    if text.is_empty() {
        Ok(Output::Mutation(MutationOutput::new(format!(
            "Description cleared for {}",
            ws_name
        ))))
    } else {
        Ok(Output::Mutation(MutationOutput::new(format!(
            "Description set for {}",
            ws_name
        ))))
    }
}

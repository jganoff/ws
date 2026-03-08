use anyhow::Result;
use clap::{Arg, ArgMatches, Command};

use crate::config::Paths;
use crate::gc;
use crate::output::{MutationOutput, Output, RecoverListOutput};

pub fn cmd() -> Command {
    Command::new("recover")
        .about("List or restore recently removed workspaces [read-only without args]")
        .long_about(
            "List or restore recently removed workspaces [read-only without args].\n\n\
             Workspaces removed with `wsp rm` are held in a gc directory for 7 days \
             (configurable via gc.retention-days). Run without arguments to list recoverable \
             workspaces, or pass a name to restore one to its original location.",
        )
        .arg(Arg::new("workspace").help("Name of workspace to restore"))
}

pub fn run(matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    if let Some(name) = matches.get_one::<String>("workspace") {
        gc::restore(paths, name)?;
        Ok(Output::Mutation(MutationOutput::new(format!(
            "Workspace {:?} restored.",
            name
        ))))
    } else {
        let entries = gc::list(&paths.gc_dir)?;
        Ok(Output::RecoverList(RecoverListOutput { entries }))
    }
}

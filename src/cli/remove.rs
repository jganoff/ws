use anyhow::{Result, bail};
use clap::{Arg, ArgMatches, Command};

use crate::workspace;

pub fn cmd() -> Command {
    Command::new("remove")
        .visible_alias("rm")
        .about("Remove a workspace and its worktrees")
        .arg(Arg::new("workspace"))
        .arg(
            Arg::new("delete-branches")
                .long("delete-branches")
                .action(clap::ArgAction::SetTrue)
                .help("Also delete workspace branches from mirrors"),
        )
        .arg(
            Arg::new("force")
                .short('f')
                .long("force")
                .action(clap::ArgAction::SetTrue)
                .help("Remove even if repos have pending changes"),
        )
}

pub fn run(matches: &ArgMatches) -> Result<()> {
    let delete_branches = matches.get_flag("delete-branches");
    let force = matches.get_flag("force");

    let name = if let Some(n) = matches.get_one::<String>("workspace") {
        n.clone()
    } else {
        let cwd = std::env::current_dir()?;
        let ws_dir = workspace::detect(&cwd)?;
        let meta = workspace::load_metadata(&ws_dir)
            .map_err(|e| anyhow::anyhow!("reading workspace: {}", e))?;
        meta.name
    };

    if !force {
        let ws_dir = workspace::dir(&name)?;
        let dirty = workspace::has_pending_changes(&ws_dir)?;
        if !dirty.is_empty() {
            let mut sorted = dirty;
            sorted.sort();
            let mut list = String::new();
            for r in &sorted {
                list.push_str(&format!("\n  - {}", r));
            }
            bail!(
                "workspace {:?} has pending changes:{}\n\nUse --force to remove anyway",
                name,
                list
            );
        }
    }

    println!("Removing workspace {:?}...", name);
    workspace::remove(&name, delete_branches)?;

    println!("Workspace {:?} removed.", name);
    Ok(())
}

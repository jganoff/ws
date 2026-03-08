use anyhow::Result;
use clap::{ArgMatches, Command};

use crate::config::Paths;
use crate::output::{Output, WorkspaceListEntry, WorkspaceListOutput};
use crate::workspace;

pub fn cmd() -> Command {
    Command::new("ls")
        .visible_alias("list")
        .about("List active workspaces [read-only]")
        .long_about(
            "List active workspaces [read-only].\n\n\
             Shows all workspaces under the workspaces directory, with their branch, repo \
             count, and description. Supports sorting by name (default), last-used time, \
             or creation date.",
        )
}

pub fn run(_matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    let names = workspace::list_all(&paths.workspaces_dir)?;

    let mut workspaces = Vec::new();
    for name in &names {
        let ws_dir = workspace::dir(&paths.workspaces_dir, name);
        let meta = match workspace::load_metadata(&ws_dir) {
            Ok(m) => m,
            Err(_) => {
                workspaces.push(WorkspaceListEntry {
                    name: name.clone(),
                    branch: "ERROR".to_string(),
                    repo_count: 0,
                    path: ws_dir.display().to_string(),
                    description: None,
                    created: String::new(),
                    last_used: None,
                    created_from: None,
                });
                continue;
            }
        };
        workspaces.push(WorkspaceListEntry {
            name: name.clone(),
            branch: meta.branch,
            repo_count: meta.repos.len(),
            path: ws_dir.display().to_string(),
            description: meta.description,
            created: meta.created.to_rfc3339(),
            last_used: None,
            created_from: meta.created_from,
        });
    }

    Ok(Output::WorkspaceList(WorkspaceListOutput {
        hint: None,
        workspaces,
    }))
}

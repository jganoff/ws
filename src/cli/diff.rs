use std::path::PathBuf;

use anyhow::Result;
use clap::{Arg, ArgMatches, Command};

use crate::config::Paths;
use crate::git;
use crate::giturl;
use crate::workspace;

pub fn cmd() -> Command {
    Command::new("diff")
        .about("Show git diff across workspace repos")
        .arg(Arg::new("workspace"))
        .arg(
            Arg::new("args")
                .num_args(1..)
                .last(true)
                .allow_hyphen_values(true),
        )
}

pub fn run(matches: &ArgMatches, paths: &Paths) -> Result<()> {
    let ws_dir: PathBuf = if let Some(name) = matches.get_one::<String>("workspace") {
        workspace::dir(&paths.workspaces_dir, name)
    } else {
        let cwd = std::env::current_dir()?;
        workspace::detect(&cwd)?
    };

    let meta = workspace::load_metadata(&ws_dir)
        .map_err(|e| anyhow::anyhow!("reading workspace: {}", e))?;

    let extra_args: Vec<&str> = matches
        .get_many::<String>("args")
        .map(|vals| vals.map(|s| s.as_str()).collect())
        .unwrap_or_default();

    let mut first = true;
    for identity in meta.repos.keys() {
        let parsed = match giturl::Parsed::from_identity(identity) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[{}] error: {}", identity, e);
                continue;
            }
        };

        let repo_dir = ws_dir.join(&parsed.repo);

        let mut args = vec!["diff"];
        args.extend(&extra_args);

        let output = match git::run(Some(&repo_dir), &args) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("[{}] error: {}", parsed.repo, e);
                continue;
            }
        };

        if output.is_empty() {
            continue;
        }

        if !first {
            println!();
        }
        println!("==> [{}]", parsed.repo);
        println!("{}", output);
        first = false;
    }

    Ok(())
}

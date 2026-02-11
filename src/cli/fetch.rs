use std::sync::Mutex;

use anyhow::{Result, bail};
use clap::{ArgMatches, Command};

use crate::config::{self, Paths};
use crate::git;
use crate::giturl;
use crate::mirror;
use crate::output::{FetchOutput, FetchRepoResult, Output};
use crate::workspace;

pub fn cmd() -> Command {
    Command::new("fetch")
        .about("Fetch updates for workspace repos")
        .arg(
            clap::Arg::new("all")
                .long("all")
                .action(clap::ArgAction::SetTrue)
                .help("Fetch all registered repos (not just current workspace)"),
        )
        .arg(
            clap::Arg::new("prune")
                .long("prune")
                .action(clap::ArgAction::SetTrue)
                .help("Prune deleted remote branches"),
        )
}

pub fn run(matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    let all = matches.get_flag("all");
    let prune = matches.get_flag("prune");

    let identities: Vec<String> = if all {
        let cfg = config::Config::load_from(&paths.config_path)
            .map_err(|e| anyhow::anyhow!("loading config: {}", e))?;
        cfg.repos.keys().cloned().collect()
    } else {
        let cwd = std::env::current_dir()?;
        let ws_dir = match workspace::detect(&cwd) {
            Ok(d) => d,
            Err(_) => bail!("not in a workspace, use --all to fetch all registered repos"),
        };
        let meta = workspace::load_metadata(&ws_dir)?;
        meta.repos.keys().cloned().collect()
    };

    if identities.is_empty() {
        return Ok(Output::Fetch(FetchOutput { repos: vec![] }));
    }

    // Resolve each identity to its mirror path
    let repos: Vec<(String, std::path::PathBuf)> = identities
        .into_iter()
        .filter_map(|id| match giturl::Parsed::from_identity(&id) {
            Ok(parsed) => Some((id, mirror::dir(&paths.mirrors_dir, &parsed))),
            Err(e) => {
                eprintln!("  {}: error parsing identity: {}", id, e);
                None
            }
        })
        .collect();

    let ids: Vec<String> = repos.iter().map(|(id, _)| id.clone()).collect();
    let shortnames = giturl::shortnames(&ids);

    if repos.len() == 1 {
        let name = shortnames
            .get(&repos[0].0)
            .map(|s| s.as_str())
            .unwrap_or(&repos[0].0);
        eprintln!("Fetching {}...", name);
    } else {
        eprintln!("Fetching {} repos...", repos.len());
    }

    // Parallel fetch with per-repo progress
    let progress = Mutex::new(());
    let results: Vec<(String, Result<()>)> = std::thread::scope(|s| {
        let handles: Vec<_> = repos
            .iter()
            .map(|(id, mirror_dir)| {
                let progress = &progress;
                let shortnames = &shortnames;
                s.spawn(move || {
                    let result = git::fetch(mirror_dir, prune);
                    let _lock = progress.lock().unwrap();
                    let name = shortnames.get(id).map(|s| s.as_str()).unwrap_or(id);
                    match &result {
                        Ok(()) => eprintln!("  ok    {}", name),
                        Err(e) => eprintln!("  FAIL  {} ({})", name, e),
                    }
                    result
                })
            })
            .collect();

        repos
            .iter()
            .zip(handles)
            .map(|((id, _), h)| (id.clone(), h.join().unwrap()))
            .collect()
    });

    let output = FetchOutput {
        repos: results
            .into_iter()
            .map(|(id, result)| {
                let name = shortnames.get(&id).cloned().unwrap_or_else(|| id.clone());
                FetchRepoResult {
                    identity: id,
                    shortname: name,
                    ok: result.is_ok(),
                    error: result.err().map(|e| e.to_string()),
                }
            })
            .collect(),
    };

    Ok(Output::Fetch(output))
}

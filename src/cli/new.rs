use std::collections::BTreeMap;
use std::sync::Mutex;

use anyhow::{Result, bail};
use clap::{Arg, ArgMatches, Command};
use clap_complete::engine::ArgValueCandidates;

use crate::config::{self, Paths};
use crate::git;
use crate::giturl;
use crate::group;
use crate::mirror;
use crate::output::{MutationOutput, Output};
use crate::workspace;

use super::completers;

pub fn cmd() -> Command {
    Command::new("new")
        .about("Create a new workspace")
        .arg(Arg::new("workspace").required(true))
        .arg(
            Arg::new("repos")
                .num_args(0..)
                .add(ArgValueCandidates::new(completers::complete_repos)),
        )
        .arg(
            Arg::new("group")
                .short('g')
                .long("group")
                .help("Add repos from a group")
                .add(ArgValueCandidates::new(completers::complete_groups)),
        )
        .arg(
            Arg::new("no-fetch")
                .long("no-fetch")
                .action(clap::ArgAction::SetTrue)
                .help("Skip fetching mirrors before cloning"),
        )
}

pub fn run(matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    let ws_name = matches.get_one::<String>("workspace").unwrap();
    let repo_args: Vec<&String> = matches
        .get_many::<String>("repos")
        .map(|v| v.collect())
        .unwrap_or_default();
    let group_name = matches.get_one::<String>("group");
    let no_fetch = matches.get_flag("no-fetch");

    let cfg = config::Config::load_from(&paths.config_path)
        .map_err(|e| anyhow::anyhow!("loading config: {}", e))?;

    let identities: Vec<String> = cfg.repos.keys().cloned().collect();

    let mut repo_refs: BTreeMap<String, String> = BTreeMap::new();

    // Add repos from group (active, no ref)
    if let Some(gn) = group_name {
        let group_repos = group::get(&cfg, gn)?;
        for id in group_repos {
            repo_refs.insert(id, String::new());
        }
    }

    // Add individual repos (may have @ref)
    for rn in &repo_args {
        let (name, r) = giturl::parse_repo_ref(rn);
        let id = giturl::resolve(name, &identities)?;
        repo_refs.insert(id, r.to_string());
    }

    if repo_refs.is_empty() {
        bail!("no repos specified (use repo args or --group)");
    }

    // Build upstream URL map from config
    let mut upstream_urls: BTreeMap<String, String> = BTreeMap::new();
    for identity in repo_refs.keys() {
        if let Some(url) = cfg.upstream_url(identity) {
            upstream_urls.insert(identity.clone(), url.to_string());
        }
    }

    // Pre-fetch mirrors (parallel) unless --no-fetch
    if !no_fetch {
        let mirrors: Vec<(String, std::path::PathBuf)> = repo_refs
            .keys()
            .filter_map(|id| {
                giturl::Parsed::from_identity(id)
                    .ok()
                    .map(|p| (id.clone(), mirror::dir(&paths.mirrors_dir, &p)))
            })
            .collect();

        if !mirrors.is_empty() {
            eprintln!("Fetching {} mirrors...", mirrors.len());
            let progress = Mutex::new(());
            std::thread::scope(|s| {
                let handles: Vec<_> = mirrors
                    .iter()
                    .map(|(id, mirror_dir)| {
                        let progress = &progress;
                        s.spawn(move || {
                            let result = git::fetch(mirror_dir, true);
                            let _lock = progress.lock().unwrap();
                            match &result {
                                Ok(()) => eprintln!("  ok    {}", id),
                                Err(e) => eprintln!("  FAIL  {} ({})", id, e),
                            }
                        })
                    })
                    .collect();
                for h in handles {
                    let _ = h.join();
                }
            });
        }
    }

    let branch_prefix = cfg.branch_prefix.as_deref();
    let branch = match branch_prefix.filter(|p| !p.is_empty()) {
        Some(prefix) => format!("{}/{}", prefix, ws_name),
        None => ws_name.to_string(),
    };

    eprintln!(
        "Creating workspace {:?} (branch: {}) with {} repos...",
        ws_name,
        branch,
        repo_refs.len()
    );
    workspace::create(paths, ws_name, &repo_refs, branch_prefix, &upstream_urls)?;

    let ws_dir = workspace::dir(&paths.workspaces_dir, ws_name);
    let meta_result = workspace::load_metadata(&ws_dir);
    match &meta_result {
        Ok(meta) => crate::lang::run_integrations(&ws_dir, meta, &cfg),
        Err(e) => eprintln!("warning: skipping language integrations: {}", e),
    }
    if cfg.agent_md.unwrap_or(true)
        && let Ok(meta) = &meta_result
        && let Err(e) = crate::agentmd::update(&ws_dir, meta)
    {
        eprintln!("warning: AGENTS.md generation failed: {}", e);
    }

    Ok(Output::Mutation(MutationOutput {
        ok: true,
        message: format!("Workspace created: {}", ws_dir.display()),
    }))
}

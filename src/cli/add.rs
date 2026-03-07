use std::collections::BTreeMap;

use anyhow::{Result, bail};
use chrono::Utc;
use clap::{Arg, ArgMatches, Command};
use clap_complete::engine::ArgValueCandidates;

use crate::config::{self, Paths, RepoEntry};
use crate::filelock;
use crate::giturl;
use crate::group;
use crate::mirror;
use crate::output::{MutationOutput, Output};
use crate::workspace;

use super::completers;

pub fn cmd() -> Command {
    Command::new("add")
        .about("Add repos to current workspace")
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
}

pub fn run(matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    let repo_args: Vec<&String> = matches
        .get_many::<String>("repos")
        .map(|v| v.collect())
        .unwrap_or_default();
    let group_name = matches.get_one::<String>("group");

    let cwd = std::env::current_dir()?;
    let ws_dir = workspace::detect(&cwd)?;

    let cfg = config::Config::load_from(&paths.config_path)
        .map_err(|e| anyhow::anyhow!("loading config: {}", e))?;

    let identities: Vec<String> = cfg.repos.keys().cloned().collect();

    let mut repo_refs: BTreeMap<String, String> = BTreeMap::new();

    if let Some(gn) = group_name {
        let group_repos = group::get(&cfg, gn)?;
        for id in group_repos {
            repo_refs.insert(id, String::new());
        }
    }

    // Track URLs that need global registration (not yet in config.yaml)
    let mut to_register: Vec<(String, String)> = Vec::new(); // (identity, url)

    for rn in &repo_args {
        let name = giturl::parse_repo_ref(rn);

        // Try resolving as a registered shortname first
        match giturl::resolve(name, &identities) {
            Ok(id) => {
                repo_refs.insert(id, String::new());
            }
            Err(_) => {
                // Not a registered shortname — try parsing as a URL
                let parsed = giturl::parse(name).map_err(|_| {
                    anyhow::anyhow!("repo {:?} not found in config and is not a valid URL", name)
                })?;
                let identity = parsed.identity();
                to_register.push((identity.clone(), name.to_string()));
                repo_refs.insert(identity, String::new());
            }
        }
    }

    if repo_refs.is_empty() {
        bail!("no repos specified (use repo args or --group)");
    }

    // Auto-register any unregistered repos (create mirror + add to config.yaml)
    for (identity, url) in &to_register {
        let parsed = giturl::parse(url)?;

        // Phase 1: check if already registered (race with concurrent add)
        let snapshot = filelock::read_config(&paths.config_path)?;
        if snapshot.repos.contains_key(identity) {
            continue; // another process registered it
        }

        // Phase 2: create mirror from upstream (slow, no lock)
        eprintln!("Registering {}...", identity);
        mirror::clone(&paths.mirrors_dir, &parsed, url)
            .map_err(|e| anyhow::anyhow!("cloning mirror for {}: {}", identity, e))?;
        mirror::fetch(&paths.mirrors_dir, &parsed)
            .map_err(|e| anyhow::anyhow!("fetching mirror for {}: {}", identity, e))?;

        // Phase 3: register under lock (fast, re-check)
        filelock::with_config(&paths.config_path, |cfg_mut| {
            if cfg_mut.repos.contains_key(identity) {
                // Another process registered it concurrently — desired state achieved.
                // Clean up the duplicate mirror we cloned in phase 2.
                let _ = mirror::remove(&paths.mirrors_dir, &parsed);
                return Ok(());
            }
            cfg_mut.repos.insert(
                identity.clone(),
                RepoEntry {
                    url: url.clone(),
                    added: Utc::now(),
                },
            );
            Ok(())
        })?;
    }

    // Reload config to pick up newly registered repos
    let cfg = if to_register.is_empty() {
        cfg
    } else {
        config::Config::load_from(&paths.config_path)
            .map_err(|e| anyhow::anyhow!("reloading config: {}", e))?
    };

    // Build upstream URL map from config
    let mut upstream_urls: BTreeMap<String, String> = BTreeMap::new();
    for identity in repo_refs.keys() {
        if let Some(url) = cfg.upstream_url(identity) {
            upstream_urls.insert(identity.clone(), url.to_string());
        }
    }

    eprintln!("Adding {} repos to workspace...", repo_refs.len());
    workspace::add_repos(&paths.mirrors_dir, &ws_dir, &repo_refs, &upstream_urls)?;
    workspace::touch_last_used(&ws_dir);

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

    Ok(Output::Mutation(MutationOutput::new("Done.")))
}

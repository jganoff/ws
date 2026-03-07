use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Instant;

use anyhow::{Result, bail};
use chrono::Utc;
use clap::{Arg, ArgMatches, Command};
use clap_complete::engine::ArgValueCandidates;

use crate::config::{self, Paths, RepoEntry};
use crate::filelock;
use crate::git;
use crate::giturl;
use crate::group;
use crate::mirror;
use crate::output::{MutationOutput, Output};
use crate::template;
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
            Arg::new("template")
                .short('t')
                .long("template")
                .help("Create workspace from a template")
                .add(ArgValueCandidates::new(completers::complete_templates)),
        )
        .arg(
            Arg::new("group")
                .short('g')
                .long("group")
                .help("Add repos from a group (deprecated, use --template)")
                .add(ArgValueCandidates::new(completers::complete_groups)),
        )
        .arg(
            Arg::new("no-fetch")
                .long("no-fetch")
                .action(clap::ArgAction::SetTrue)
                .help("Skip fetching mirrors before cloning"),
        )
        .arg(
            Arg::new("description")
                .short('d')
                .long("description")
                .help("Purpose of the workspace"),
        )
}

pub fn run(matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    let ws_name = matches.get_one::<String>("workspace").unwrap();
    let repo_args: Vec<&String> = matches
        .get_many::<String>("repos")
        .map(|v| v.collect())
        .unwrap_or_default();
    let template_name = matches.get_one::<String>("template");
    let group_name = matches.get_one::<String>("group");
    let no_fetch = matches.get_flag("no-fetch");
    let description = matches.get_one::<String>("description");

    if group_name.is_some() {
        eprintln!("warning: --group is deprecated, use --template instead");
    }

    let mut cfg = config::Config::load_from(&paths.config_path)
        .map_err(|e| anyhow::anyhow!("loading config: {}", e))?;

    let mut repo_refs: BTreeMap<String, String> = BTreeMap::new();
    let mut created_from: Option<String> = None;

    // Add repos from template
    if let Some(tn) = template_name {
        let tmpl = template::load(&paths.templates_dir, tn)?;

        // Auto-register unknown repos from template
        auto_register_template_repos(&tmpl, &mut cfg, paths)?;

        let identities = tmpl.identities()?;
        for id in identities {
            repo_refs.insert(id, String::new());
        }
        created_from = Some(tn.clone());
    }

    // Add repos from group (active, no ref)
    if let Some(gn) = group_name {
        let group_repos = group::get(&cfg, gn)?;
        for id in group_repos {
            repo_refs.insert(id, String::new());
        }
    }

    // Add individual repos
    let identities: Vec<String> = cfg.repos.keys().cloned().collect();
    for rn in &repo_args {
        let name = giturl::parse_repo_ref(rn);
        let id = giturl::resolve(name, &identities)?;
        repo_refs.insert(id, String::new());
    }

    if repo_refs.is_empty() {
        bail!("no repos specified (use repo args, --template, or --group)");
    }

    // Validate early before expensive I/O
    workspace::validate_name(ws_name)?;
    let ws_dir = workspace::dir(&paths.workspaces_dir, ws_name);
    if ws_dir.exists() {
        bail!("workspace {:?} already exists", ws_name);
    }

    // Build upstream URL map from config
    let mut upstream_urls: BTreeMap<String, String> = BTreeMap::new();
    for identity in repo_refs.keys() {
        if let Some(url) = cfg.upstream_url(identity) {
            upstream_urls.insert(identity.clone(), url.to_string());
        }
    }

    let start = Instant::now();

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
    workspace::create(
        paths,
        ws_name,
        &repo_refs,
        branch_prefix,
        &upstream_urls,
        description.map(|s| s.as_str()),
        created_from.as_deref(),
    )?;

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

    let duration_ms = start.elapsed().as_millis() as u64;

    Ok(Output::Mutation(
        MutationOutput::new(format!("Workspace created: {}", ws_dir.display()))
            .with_duration(duration_ms),
    ))
}

/// Auto-register any repos from a template that aren't already in the registry.
/// Clones mirrors and adds entries to config.
fn auto_register_template_repos(
    tmpl: &template::Template,
    cfg: &mut config::Config,
    paths: &Paths,
) -> Result<()> {
    let mut to_register = Vec::new();

    for repo in &tmpl.repos {
        let parsed = giturl::parse(&repo.url)?;
        let identity = parsed.identity();
        if !cfg.repos.contains_key(&identity) {
            to_register.push((identity, parsed, repo.url.clone()));
        }
    }

    if to_register.is_empty() {
        return Ok(());
    }

    eprintln!(
        "Auto-registering {} repos from template...",
        to_register.len()
    );

    for (identity, parsed, url) in &to_register {
        if !mirror::exists(&paths.mirrors_dir, parsed) {
            eprintln!("  cloning {}...", url);
            mirror::clone(&paths.mirrors_dir, parsed, url)
                .map_err(|e| anyhow::anyhow!("cloning {}: {}", identity, e))?;
        }
    }

    // Register under lock
    filelock::with_config(&paths.config_path, |locked_cfg| {
        for (identity, _, url) in &to_register {
            if !locked_cfg.repos.contains_key(identity) {
                locked_cfg.repos.insert(
                    identity.clone(),
                    RepoEntry {
                        url: url.clone(),
                        added: Utc::now(),
                    },
                );
            }
        }
        Ok(())
    })?;

    // Update the in-memory config to reflect the new repos
    for (identity, _, url) in to_register {
        cfg.repos.insert(
            identity,
            RepoEntry {
                url,
                added: Utc::now(),
            },
        );
    }

    Ok(())
}

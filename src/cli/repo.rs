use anyhow::{Result, bail};
use chrono::Utc;
use clap::{Arg, ArgMatches, Command};
use clap_complete::engine::ArgValueCandidates;

use crate::config::{self, Paths, RepoEntry};
use crate::giturl;
use crate::mirror;
use crate::output::{
    ImportFailure, ImportOutput, MutationOutput, Output, RepoListEntry, RepoListOutput,
};

use super::completers;

pub fn add_cmd() -> Command {
    Command::new("add")
        .about("Register and bare-clone a repository")
        .arg(Arg::new("url").required_unless_present("from"))
        .arg(
            Arg::new("from")
                .long("from")
                .help("Import repos from a GitHub org/user (e.g. github.com/acme)")
                .conflicts_with("url"),
        )
        .arg(
            Arg::new("pattern")
                .long("pattern")
                .help("Glob pattern(s) to filter repo names, comma-separated")
                .requires("from"),
        )
        .arg(
            Arg::new("all")
                .long("all")
                .action(clap::ArgAction::SetTrue)
                .help("Import all repos (required if --pattern not given)")
                .requires("from")
                .conflicts_with("pattern"),
        )
        .arg(
            Arg::new("https")
                .long("https")
                .action(clap::ArgAction::SetTrue)
                .help("Use HTTPS URLs instead of SSH")
                .requires("from"),
        )
}

pub fn list_cmd() -> Command {
    Command::new("list").about("List registered repositories [read-only]")
}

pub fn remove_cmd() -> Command {
    Command::new("remove")
        .about("Remove a repository and its mirror")
        .arg(
            Arg::new("name")
                .required(true)
                .add(ArgValueCandidates::new(completers::complete_repos)),
        )
}

pub fn run_add(matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    if matches.get_one::<String>("from").is_some() {
        return run_add_from(matches, paths);
    }

    let raw_url = matches.get_one::<String>("url").unwrap();

    let parsed = giturl::parse(raw_url)?;
    let mut cfg = config::Config::load_from(&paths.config_path)?;

    let identity = parsed.identity();
    if cfg.repos.contains_key(&identity) {
        bail!("repo {} already registered", identity);
    }

    let exists = mirror::exists(&paths.mirrors_dir, &parsed);
    if exists {
        bail!("mirror already exists for {}", identity);
    }

    eprintln!("Cloning {}...", raw_url);
    mirror::clone(&paths.mirrors_dir, &parsed, raw_url)
        .map_err(|e| anyhow::anyhow!("cloning: {}", e))?;

    cfg.repos.insert(
        identity.clone(),
        RepoEntry {
            url: raw_url.clone(),
            added: Utc::now(),
        },
    );

    cfg.save_to(&paths.config_path)
        .map_err(|e| anyhow::anyhow!("saving config: {}", e))?;

    Ok(Output::Mutation(MutationOutput {
        ok: true,
        message: format!("Registered {}", identity),
    }))
}

fn run_add_from(matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    let from = matches.get_one::<String>("from").unwrap();
    let use_https = matches.get_flag("https");
    let patterns: Vec<&str> = matches
        .get_one::<String>("pattern")
        .map(|p| p.split(',').map(|s| s.trim()).collect())
        .unwrap_or_default();
    let all = matches.get_flag("all");

    if !all && patterns.is_empty() {
        bail!("--from requires either --pattern or --all");
    }

    let (host, owner) = parse_from_arg(from)?;
    if host != "github.com" {
        bail!("only github.com is supported (got {})", host);
    }

    let repos = gh_list_repos(&owner, use_https)?;

    let filtered: Vec<_> = if all {
        repos
    } else {
        repos
            .into_iter()
            .filter(|(name, _url)| patterns.iter().any(|p| glob_match(p, name)))
            .collect()
    };

    if filtered.is_empty() {
        bail!("no repos matched");
    }

    let mut cfg = config::Config::load_from(&paths.config_path)?;
    let mut registered = Vec::new();
    let mut skipped = Vec::new();
    let mut failed = Vec::new();

    for (name, url) in &filtered {
        let parsed = match giturl::parse(url) {
            Ok(p) => p,
            Err(e) => {
                failed.push(ImportFailure {
                    name: name.clone(),
                    error: e.to_string(),
                });
                continue;
            }
        };
        let identity = parsed.identity();

        if cfg.repos.contains_key(&identity) {
            skipped.push(identity);
            continue;
        }

        // Mirror exists on disk but not in config (e.g. crash recovery) — re-register
        if mirror::exists(&paths.mirrors_dir, &parsed) {
            cfg.repos.insert(
                identity.clone(),
                RepoEntry {
                    url: url.clone(),
                    added: Utc::now(),
                },
            );
            registered.push(identity);
            continue;
        }

        eprintln!("Cloning {}...", url);
        if let Err(e) = mirror::clone(&paths.mirrors_dir, &parsed, url) {
            failed.push(ImportFailure {
                name: name.clone(),
                error: e.to_string(),
            });
            continue;
        }

        cfg.repos.insert(
            identity.clone(),
            RepoEntry {
                url: url.clone(),
                added: Utc::now(),
            },
        );
        registered.push(identity);
    }

    if !registered.is_empty() {
        cfg.save_to(&paths.config_path)?;
    }

    Ok(Output::Import(ImportOutput {
        registered,
        skipped,
        failed,
    }))
}

fn parse_from_arg(from: &str) -> Result<(String, String)> {
    // Strip protocol prefix if copy-pasted from browser
    let from = from
        .strip_prefix("https://")
        .or_else(|| from.strip_prefix("http://"))
        .unwrap_or(from);

    if from.is_empty() {
        bail!("--from value cannot be empty");
    }

    let (host, owner) = if let Some(idx) = from.find('/') {
        let h = &from[..idx];
        let o = &from[idx + 1..];
        // Trim trailing slash from owner (e.g. "github.com/acme/")
        let o = o.trim_end_matches('/');
        if h.is_empty() {
            bail!("missing host in --from");
        }
        if o.is_empty() {
            bail!("missing org/user name in --from");
        }
        (h.to_string(), o.to_string())
    } else {
        ("github.com".to_string(), from.to_string())
    };

    if owner.starts_with('-') {
        bail!("org/user name cannot start with '-': {:?}", owner);
    }

    Ok((host, owner))
}

fn gh_list_repos(owner: &str, use_https: bool) -> Result<Vec<(String, String)>> {
    let limit = 1000;
    let output = std::process::Command::new("gh")
        .args([
            "repo",
            "list",
            "--json",
            "name,sshUrl,url",
            "--limit",
            &limit.to_string(),
            "--no-archived",
            "--", // end of flags — owner is always treated as positional
            owner,
        ])
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run gh: {} (is gh installed?)", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!("gh repo list failed: {}", stderr);
    }

    let entries: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout)?;

    if entries.len() >= limit {
        eprintln!(
            "warning: gh returned {} repos; results may be truncated",
            entries.len()
        );
    }

    let repos: Vec<(String, String)> = entries
        .iter()
        .filter_map(|e| {
            let name = e["name"].as_str()?;
            let url = if use_https {
                e["url"].as_str()?
            } else {
                e["sshUrl"].as_str()?
            };
            Some((name.to_string(), url.to_string()))
        })
        .collect();

    Ok(repos)
}

fn glob_match(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == name;
    }
    let mut pos = 0;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        match name[pos..].find(part) {
            Some(idx) => {
                if i == 0 && idx != 0 {
                    return false;
                }
                pos += idx + part.len();
            }
            None => return false,
        }
    }
    if !pattern.ends_with('*') {
        pos == name.len()
    } else {
        true
    }
}

pub fn run_list(_matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    let cfg = config::Config::load_from(&paths.config_path)
        .map_err(|e| anyhow::anyhow!("loading config: {}", e))?;

    let mut identities: Vec<String> = cfg.repos.keys().cloned().collect();
    identities.sort();

    let shortnames = giturl::shortnames(&identities);

    let repos = identities
        .iter()
        .map(|id| {
            let entry = &cfg.repos[id];
            let short = shortnames.get(id).cloned().unwrap_or_else(|| id.clone());
            RepoListEntry {
                identity: id.clone(),
                shortname: short,
                url: entry.url.clone(),
            }
        })
        .collect();

    Ok(Output::RepoList(RepoListOutput { repos }))
}

pub fn run_remove(matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    let name = matches.get_one::<String>("name").unwrap();

    let mut cfg = config::Config::load_from(&paths.config_path)
        .map_err(|e| anyhow::anyhow!("loading config: {}", e))?;

    let identities: Vec<String> = cfg.repos.keys().cloned().collect();
    let identity = giturl::resolve(name, &identities)?;

    let entry = &cfg.repos[&identity];
    let parsed = giturl::parse(&entry.url)?;

    eprintln!("Removing mirror for {}...", identity);
    mirror::remove(&paths.mirrors_dir, &parsed)
        .map_err(|e| anyhow::anyhow!("removing mirror: {}", e))?;

    cfg.repos.remove(&identity);
    cfg.save_to(&paths.config_path)
        .map_err(|e| anyhow::anyhow!("saving config: {}", e))?;

    Ok(Output::Mutation(MutationOutput {
        ok: true,
        message: format!("Removed {}", identity),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_glob_match() {
        let cases = vec![
            ("prefix wildcard", "api-*", "api-gateway", true),
            ("prefix wildcard 2", "api-*", "api-v2", true),
            ("prefix no match", "api-*", "user-api", false),
            ("suffix wildcard", "*-service", "user-service", true),
            ("suffix no match", "*-service", "service-mesh", false),
            ("contains wildcard", "*core*", "core", true),
            ("contains wildcard 2", "*core*", "core-lib", true),
            ("contains wildcard 3", "*core*", "my-core-service", true),
            ("exact match", "exact", "exact", true),
            ("exact no match", "exact", "exactly", false),
            ("match all", "*", "anything", true),
            ("match all empty", "*", "", true),
            ("empty pattern", "", "", true),
            ("empty pattern no match", "", "x", false),
            ("multi wildcard", "a*b*c", "aXbYc", true),
            ("multi wildcard no match", "a*b*c", "aXYc", false),
            ("anchored start", "api-*", "xapi-foo", false),
            ("anchored end", "*-api", "api-x", false),
        ];
        for (name, pattern, input, want) in cases {
            assert_eq!(
                glob_match(pattern, input),
                want,
                "{}: glob_match({:?}, {:?})",
                name,
                pattern,
                input
            );
        }
    }

    #[test]
    fn test_parse_from_arg() {
        let cases = vec![
            ("full host", "github.com/acme", Ok(("github.com", "acme"))),
            ("shorthand", "acme", Ok(("github.com", "acme"))),
            ("gitlab", "gitlab.com/team", Ok(("gitlab.com", "team"))),
            (
                "https prefix stripped",
                "https://github.com/acme",
                Ok(("github.com", "acme")),
            ),
            (
                "http prefix stripped",
                "http://github.com/acme",
                Ok(("github.com", "acme")),
            ),
            (
                "trailing slash trimmed",
                "github.com/acme/",
                Ok(("github.com", "acme")),
            ),
        ];
        for (name, input, want) in cases {
            let result = parse_from_arg(input);
            match want {
                Ok((host, owner)) => {
                    let (got_host, got_owner) =
                        result.unwrap_or_else(|e| panic!("{}: unexpected error: {}", name, e));
                    assert_eq!(got_host, host, "{}", name);
                    assert_eq!(got_owner, owner, "{}", name);
                }
                Err(()) => {
                    assert!(result.is_err(), "{}: expected error", name);
                }
            }
        }
    }

    #[test]
    fn test_parse_from_arg_errors() {
        let cases = vec![
            ("empty", ""),
            ("only slash", "github.com/"),
            ("leading slash", "/acme"),
            ("dash prefix", "--help"),
            ("dash org", "-R"),
        ];
        for (name, input) in cases {
            assert!(
                parse_from_arg(input).is_err(),
                "{}: expected error for {:?}",
                name,
                input
            );
        }
    }
}

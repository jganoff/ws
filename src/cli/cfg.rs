use std::collections::BTreeMap;

use anyhow::{Result, bail};
use clap::{Arg, ArgMatches, Command};

use crate::config::{self, Paths};
use crate::filelock;
use crate::output::{ConfigGetOutput, ConfigListEntry, ConfigListOutput, MutationOutput, Output};

pub fn cmd() -> Command {
    Command::new("config")
        .about("Manage wsp settings")
        .long_about(
            "Manage wsp settings.\n\n\
             Settings are stored in ~/.local/share/wsp/config.yaml. Keys include \
             branch-prefix, workspaces-dir, gc.retention-days, and language integration \
             toggles. Use `wsp config ls` to see all current values.",
        )
        .subcommand_required(true)
        .subcommand(list_cmd())
        .subcommand(get_cmd())
        .subcommand(set_cmd())
        .subcommand(unset_cmd())
}

pub fn dispatch(matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    match matches.subcommand() {
        Some(("ls", m)) => run_list(m, paths),
        Some(("get", m)) => run_get(m, paths),
        Some(("set", m)) => run_set(m, paths),
        Some(("unset", m)) => run_unset(m, paths),
        _ => unreachable!(),
    }
}

pub fn list_cmd() -> Command {
    Command::new("ls")
        .visible_alias("list")
        .about("List all config values [read-only]")
}

pub fn get_cmd() -> Command {
    Command::new("get")
        .about("Get a config value [read-only]")
        .arg(Arg::new("key").required(true))
}

pub fn set_cmd() -> Command {
    Command::new("set")
        .about("Set a config value")
        .arg(Arg::new("key").required(true))
        .arg(Arg::new("value").required(true))
}

pub fn unset_cmd() -> Command {
    Command::new("unset")
        .about("Unset a config value")
        .arg(Arg::new("key").required(true))
}

pub fn run_list(_matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    let cfg = config::Config::load_from(&paths.config_path)?;
    let mut entries = vec![
        ConfigListEntry {
            key: "branch-prefix".into(),
            value: cfg
                .branch_prefix
                .as_deref()
                .unwrap_or("(not set)")
                .to_string(),
        },
        ConfigListEntry {
            key: "workspaces-dir".into(),
            value: paths.workspaces_dir.display().to_string(),
        },
        ConfigListEntry {
            key: "sync-strategy".into(),
            value: cfg.sync_strategy.as_deref().unwrap_or("rebase").to_string(),
        },
        ConfigListEntry {
            key: "agent-md".into(),
            value: cfg.agent_md.unwrap_or(true).to_string(),
        },
        ConfigListEntry {
            key: "gc.retention-days".into(),
            value: cfg.gc_retention_days.unwrap_or(7).to_string(),
        },
    ];

    // language integrations: show effective value for all known integrations
    for name in crate::lang::integration_names() {
        let enabled = cfg
            .language_integrations
            .as_ref()
            .and_then(|m| m.get(name.as_str()))
            .copied()
            .unwrap_or(true);
        entries.push(ConfigListEntry {
            key: format!("language-integrations.{}", name),
            value: enabled.to_string(),
        });
    }

    Ok(Output::ConfigList(ConfigListOutput { entries }))
}

pub fn run_get(matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    let key = matches.get_one::<String>("key").unwrap();
    let cfg = config::Config::load_from(&paths.config_path)?;

    match key.as_str() {
        "branch-prefix" => Ok(Output::ConfigGet(ConfigGetOutput {
            key: key.clone(),
            value: cfg.branch_prefix,
        })),
        "workspaces-dir" => Ok(Output::ConfigGet(ConfigGetOutput {
            key: key.clone(),
            value: Some(paths.workspaces_dir.display().to_string()),
        })),
        "sync-strategy" => Ok(Output::ConfigGet(ConfigGetOutput {
            key: key.clone(),
            value: Some(cfg.sync_strategy.as_deref().unwrap_or("rebase").to_string()),
        })),
        "agent-md" => Ok(Output::ConfigGet(ConfigGetOutput {
            key: key.clone(),
            value: Some(cfg.agent_md.unwrap_or(true).to_string()),
        })),
        "gc.retention-days" => Ok(Output::ConfigGet(ConfigGetOutput {
            key: key.clone(),
            value: Some(cfg.gc_retention_days.unwrap_or(7).to_string()),
        })),
        k if k.starts_with("language-integrations.") => {
            let lang = &k["language-integrations.".len()..];
            let enabled = cfg
                .language_integrations
                .as_ref()
                .and_then(|m| m.get(lang))
                .copied()
                .unwrap_or(true);
            Ok(Output::ConfigGet(ConfigGetOutput {
                key: key.clone(),
                value: Some(enabled.to_string()),
            }))
        }
        _ => bail!("unknown config key: {}", key),
    }
}

pub fn run_set(matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    let key = matches.get_one::<String>("key").unwrap();
    let value = matches.get_one::<String>("value").unwrap();

    // Validate inputs before acquiring lock
    let message = match key.as_str() {
        "branch-prefix" => {
            let v = value.clone();
            filelock::with_config(&paths.config_path, |cfg| {
                cfg.branch_prefix = Some(v);
                Ok(())
            })?;
            format!("branch-prefix = {}", value)
        }
        "workspaces-dir" => {
            let path = std::path::Path::new(value.as_str());
            if !path.is_absolute() {
                bail!("workspaces-dir must be an absolute path");
            }
            let v = value.clone();
            filelock::with_config(&paths.config_path, |cfg| {
                cfg.workspaces_dir = Some(v);
                Ok(())
            })?;
            format!("workspaces-dir = {}", value)
        }
        "sync-strategy" => {
            match value.as_str() {
                "rebase" | "merge" => {}
                _ => bail!("sync-strategy must be 'rebase' or 'merge'"),
            }
            let v = value.clone();
            filelock::with_config(&paths.config_path, |cfg| {
                cfg.sync_strategy = Some(v);
                Ok(())
            })?;
            format!("sync-strategy = {}", value)
        }
        "agent-md" => {
            let enabled: bool = value
                .parse()
                .map_err(|_| anyhow::anyhow!("value must be true or false"))?;
            filelock::with_config(&paths.config_path, |cfg| {
                cfg.agent_md = Some(enabled);
                Ok(())
            })?;
            format!("agent-md = {}", enabled)
        }
        "gc.retention-days" => {
            let days: u32 = value
                .parse()
                .map_err(|_| anyhow::anyhow!("value must be a positive integer"))?;
            if days < 1 {
                bail!("gc.retention-days must be at least 1");
            }
            filelock::with_config(&paths.config_path, |cfg| {
                cfg.gc_retention_days = Some(days);
                Ok(())
            })?;
            format!("gc.retention-days = {}", days)
        }
        k if k.starts_with("language-integrations.") => {
            let lang = &k["language-integrations.".len()..];
            let known = crate::lang::integration_names();
            if !known.iter().any(|n| n == lang) {
                bail!("unknown language integration: {}", lang);
            }
            let enabled: bool = value
                .parse()
                .map_err(|_| anyhow::anyhow!("value must be true or false"))?;
            let lang = lang.to_string();
            filelock::with_config(&paths.config_path, |cfg| {
                let integrations = cfg.language_integrations.get_or_insert_with(BTreeMap::new);
                integrations.insert(lang.clone(), enabled);
                Ok(())
            })?;
            format!("language-integrations.{} = {}", lang, enabled)
        }
        _ => bail!("unknown config key: {}", key),
    };

    Ok(Output::Mutation(MutationOutput::new(message)))
}

pub fn run_unset(matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    let key = matches.get_one::<String>("key").unwrap();

    let message = match key.as_str() {
        "branch-prefix" => {
            filelock::with_config(&paths.config_path, |cfg| {
                cfg.branch_prefix = None;
                Ok(())
            })?;
            "branch-prefix unset".into()
        }
        "workspaces-dir" => {
            filelock::with_config(&paths.config_path, |cfg| {
                cfg.workspaces_dir = None;
                Ok(())
            })?;
            "workspaces-dir unset (default: ~/dev/workspaces)".into()
        }
        "sync-strategy" => {
            filelock::with_config(&paths.config_path, |cfg| {
                cfg.sync_strategy = None;
                Ok(())
            })?;
            "sync-strategy unset (default: rebase)".into()
        }
        "agent-md" => {
            filelock::with_config(&paths.config_path, |cfg| {
                cfg.agent_md = None;
                Ok(())
            })?;
            "agent-md unset (default: true)".into()
        }
        "gc.retention-days" => {
            filelock::with_config(&paths.config_path, |cfg| {
                cfg.gc_retention_days = None;
                Ok(())
            })?;
            "gc.retention-days unset (default: 7)".into()
        }
        k if k.starts_with("language-integrations.") => {
            let lang = &k["language-integrations.".len()..];
            let known = crate::lang::integration_names();
            if !known.iter().any(|n| n == lang) {
                bail!("unknown language integration: {}", lang);
            }
            let lang = lang.to_string();
            filelock::with_config(&paths.config_path, |cfg| {
                if let Some(ref mut m) = cfg.language_integrations {
                    m.remove(&lang);
                    if m.is_empty() {
                        cfg.language_integrations = None;
                    }
                }
                Ok(())
            })?;
            format!("language-integrations.{} unset (default: true)", lang)
        }
        _ => bail!("unknown config key: {}", key),
    };

    Ok(Output::Mutation(MutationOutput::new(message)))
}

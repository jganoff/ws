use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Result, bail};
use clap::{Arg, ArgAction, ArgMatches, Command};
use clap_complete::engine::ArgValueCandidates;

use crate::cli::completers;
use crate::config::{self, Paths};
use crate::filelock;
use crate::output::{ConfigGetOutput, ConfigListEntry, ConfigListOutput, MutationOutput, Output};
use crate::template;
use crate::workspace;

pub fn cmd() -> Command {
    Command::new("config")
        .about("Manage wsp settings")
        .long_about(
            "Manage wsp settings.\n\n\
             Settings are stored in ~/.local/share/wsp/config.yaml (global) or per-workspace \
             in .wsp.yaml (workspace-scoped). When run inside a workspace, set/get/unset/ls \
             operate on workspace config by default. Use --global to target global config \
             instead. Workspace config overrides global for: sync-strategy, git_config.*, \
             language-integrations.*. Keys like branch-prefix, workspaces-dir, gc.retention-days, \
             agent-md, and experimental.* are global-only.",
        )
        .subcommand(list_cmd())
        .subcommand(get_cmd())
        .subcommand(set_cmd())
        .subcommand(unset_cmd())
}

pub fn dispatch(matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    let (sub_name, sub_matches) = match matches.subcommand() {
        Some((name, m)) => (name, m),
        None => ("ls", matches),
    };

    // Use try_get_one: bare `wsp config` dispatches as "ls" with the parent ArgMatches
    // which doesn't define --global, so get_flag would panic.
    let global = sub_matches
        .try_get_one::<bool>("global")
        .ok()
        .flatten()
        .copied()
        .unwrap_or(false);
    let ws_dir = if global {
        None
    } else {
        std::env::current_dir()
            .ok()
            .and_then(|cwd| workspace::detect(&cwd).ok())
    };

    match (sub_name, ws_dir) {
        ("ls", Some(ws)) => run_list_workspace(sub_matches, &ws, paths),
        ("ls", None) => run_list(sub_matches, paths),
        ("get", Some(ws)) => run_get_workspace(sub_matches, &ws, paths),
        ("get", None) => run_get(sub_matches, paths),
        ("set", Some(ws)) => run_set_workspace(sub_matches, &ws, paths),
        ("set", None) => run_set(sub_matches, paths),
        ("unset", Some(ws)) => run_unset_workspace(sub_matches, &ws, paths),
        ("unset", None) => run_unset(sub_matches, paths),
        _ => unreachable!(),
    }
}

/// Keys that are global-only and cannot be set at workspace level.
const GLOBAL_ONLY_KEYS: &[&str] = &[
    "branch-prefix",
    "workspaces-dir",
    "gc.retention-days",
    "agent-md",
    "experimental",
];

fn is_global_only_key(key: &str) -> bool {
    let normalized = template::normalize_key(key);
    GLOBAL_ONLY_KEYS.contains(&normalized.as_str()) || normalized.starts_with("experimental.")
}

fn global_arg() -> Arg {
    Arg::new("global")
        .long("global")
        .action(ArgAction::SetTrue)
        .help("Use global config even when inside a workspace")
}

pub fn list_cmd() -> Command {
    Command::new("ls")
        .visible_alias("list")
        .about("List all config values [read-only]")
        .arg(global_arg())
}

pub fn get_cmd() -> Command {
    Command::new("get")
        .about("Get a config value [read-only]")
        .arg(
            Arg::new("key")
                .required(true)
                .add(ArgValueCandidates::new(completers::complete_config_keys)),
        )
        .arg(global_arg())
}

pub fn set_cmd() -> Command {
    Command::new("set")
        .about("Set a config value")
        .arg(
            Arg::new("key")
                .required(true)
                .add(ArgValueCandidates::new(completers::complete_config_keys)),
        )
        .arg(
            Arg::new("value")
                .required(true)
                .add(ArgValueCandidates::new(completers::complete_config_values)),
        )
        .arg(global_arg())
}

pub fn unset_cmd() -> Command {
    Command::new("unset")
        .about("Unset a config value")
        .arg(
            Arg::new("key")
                .required(true)
                .add(ArgValueCandidates::new(completers::complete_config_keys)),
        )
        .arg(global_arg())
}

// ---------------------------------------------------------------------------
// Workspace-scoped config operations
// ---------------------------------------------------------------------------

fn run_set_workspace(matches: &ArgMatches, ws_dir: &Path, _paths: &Paths) -> Result<Output> {
    let key = matches.get_one::<String>("key").unwrap();
    let value = matches.get_one::<String>("value").unwrap();

    if is_global_only_key(key) {
        bail!("{} is a global-only key; use --global to set it", key);
    }

    template::validate_template_config_key(key)?;

    let normalized = template::normalize_key(key);

    let meta = filelock::with_metadata(ws_dir, |meta| {
        let config = meta
            .config
            .get_or_insert_with(template::TemplateConfig::default);

        if normalized == "sync-strategy" {
            match value.as_str() {
                "rebase" | "merge" => {}
                _ => bail!("sync-strategy must be 'rebase' or 'merge'"),
            }
            config.sync_strategy = Some(value.to_string());
        } else if let Some(lang) = normalized.strip_prefix("language-integrations.") {
            let known = crate::lang::integration_names();
            if !known.iter().any(|n| n == lang) {
                bail!("unknown language integration: {}", lang);
            }
            let enabled: bool = value
                .parse()
                .map_err(|_| anyhow::anyhow!("value must be true or false"))?;
            let li = config
                .language_integrations
                .get_or_insert_with(BTreeMap::new);
            li.insert(lang.to_string(), enabled);
        } else if let Some(git_key) = normalized.strip_prefix("git-config.") {
            if git_key.is_empty() {
                bail!("git-config key cannot be empty");
            }
            let gc = config.git_config.get_or_insert_with(BTreeMap::new);
            gc.insert(git_key.to_string(), value.to_string());
        }
        Ok(())
    })?;

    // Apply git config to clones immediately (using metadata from the locked read above)
    if let Some(git_key) = normalized.strip_prefix("git-config.") {
        let mut gc = BTreeMap::new();
        gc.insert(git_key.to_string(), value.to_string());
        workspace::apply_git_config(ws_dir, &meta, &gc, None);
    }

    let message = format!("{} = {} (workspace: {})", key, value, meta.name);
    Ok(Output::Mutation(MutationOutput::new(message)))
}

fn run_get_workspace(matches: &ArgMatches, ws_dir: &Path, paths: &Paths) -> Result<Output> {
    let key = matches.get_one::<String>("key").unwrap();
    let meta = workspace::load_metadata(ws_dir)?;
    let cfg = config::Config::load_from(&paths.config_path)?;
    let effective = meta.apply_workspace_config(&cfg);

    // For workspace-scoped keys, return effective value; for global-only, delegate.
    // Normalize key for matching so both underscore and hyphen variants work.
    let normalized = template::normalize_key(key);
    match normalized.as_str() {
        "sync-strategy" => Ok(Output::ConfigGet(ConfigGetOutput {
            key: key.clone(),
            value: Some(
                effective
                    .sync_strategy
                    .as_deref()
                    .unwrap_or("rebase")
                    .to_string(),
            ),
        })),
        k if k.starts_with("language-integrations.") => {
            let lang = &k["language-integrations.".len()..];
            let enabled = effective
                .language_integrations
                .as_ref()
                .and_then(|m| m.get(lang))
                .copied()
                .unwrap_or(false);
            Ok(Output::ConfigGet(ConfigGetOutput {
                key: key.clone(),
                value: Some(enabled.to_string()),
            }))
        }
        k if k.starts_with("git-config.") => {
            let git_key = &k["git-config.".len()..];
            let effective_gc = effective.effective_git_config();
            Ok(Output::ConfigGet(ConfigGetOutput {
                key: key.clone(),
                value: effective_gc.get(git_key).cloned(),
            }))
        }
        // Global-only keys: delegate to global get
        _ => run_get(matches, paths),
    }
}

fn run_unset_workspace(matches: &ArgMatches, ws_dir: &Path, paths: &Paths) -> Result<Output> {
    let key = matches.get_one::<String>("key").unwrap();

    if is_global_only_key(key) {
        bail!("{} is a global-only key; use --global to unset it", key);
    }

    template::validate_template_config_key(key)?;

    let normalized = template::normalize_key(key);
    let cfg = config::Config::load_from(&paths.config_path)?;

    filelock::with_metadata(ws_dir, |meta| {
        let config = match &mut meta.config {
            Some(c) => c,
            None => return Ok(()),
        };

        if normalized == "sync-strategy" {
            config.sync_strategy = None;
        } else if let Some(lang) = normalized.strip_prefix("language-integrations.") {
            if let Some(ref mut m) = config.language_integrations {
                m.remove(lang);
                if m.is_empty() {
                    config.language_integrations = None;
                }
            }
        } else if let Some(git_key) = normalized.strip_prefix("git-config.")
            && let Some(ref mut m) = config.git_config
        {
            m.remove(git_key);
            if m.is_empty() {
                config.git_config = None;
            }
        }

        // Clean up empty config
        if *config == template::TemplateConfig::default() {
            meta.config = None;
        }

        Ok(())
    })?;

    // Build message with fallback info
    // Use normalized key for matching so both underscore and hyphen variants work
    let fallback = match normalized.as_str() {
        "sync-strategy" => {
            let global = cfg.sync_strategy.as_deref().unwrap_or("rebase");
            format!(" (using global: {})", global)
        }
        k if k.starts_with("language-integrations.") => {
            let lang = &k["language-integrations.".len()..];
            let global = cfg
                .language_integrations
                .as_ref()
                .and_then(|m| m.get(lang))
                .copied()
                .unwrap_or(false);
            format!(" (using global: {})", global)
        }
        k if k.starts_with("git-config.") => {
            let git_key = &k["git-config.".len()..];
            let defaults = config::Config::default_git_config();
            let global = cfg
                .git_config
                .as_ref()
                .and_then(|m| m.get(git_key))
                .or_else(|| defaults.get(git_key))
                .cloned()
                .unwrap_or_default();
            if global.is_empty() {
                String::new()
            } else {
                format!(" (using global: {})", global)
            }
        }
        _ => String::new(),
    };

    let message = format!("{} unset in workspace{}", key, fallback);
    Ok(Output::Mutation(MutationOutput::new(message)))
}

fn run_list_workspace(_matches: &ArgMatches, ws_dir: &Path, paths: &Paths) -> Result<Output> {
    let meta = workspace::load_metadata(ws_dir)?;
    let cfg = config::Config::load_from(&paths.config_path)?;
    let ws_config = meta.config.as_ref();

    let mut entries = vec![
        ConfigListEntry {
            key: "branch-prefix".into(),
            value: cfg
                .branch_prefix
                .as_deref()
                .unwrap_or("(not set)")
                .to_string(),
            source: None,
        },
        ConfigListEntry {
            key: "workspaces-dir".into(),
            value: paths.workspaces_dir.display().to_string(),
            source: None,
        },
        ConfigListEntry {
            key: "sync-strategy".into(),
            value: ws_config
                .and_then(|c| c.sync_strategy.as_deref())
                .or(cfg.sync_strategy.as_deref())
                .unwrap_or("rebase")
                .to_string(),
            source: ws_config
                .and_then(|c| c.sync_strategy.as_ref())
                .map(|_| "workspace".to_string()),
        },
        ConfigListEntry {
            key: "agent-md".into(),
            value: cfg.agent_md.unwrap_or(true).to_string(),
            source: None,
        },
        ConfigListEntry {
            key: "gc.retention-days".into(),
            value: cfg.gc_retention_days.unwrap_or(7).to_string(),
            source: None,
        },
    ];

    // git config: merge workspace overrides over effective global
    let mut effective_gc = cfg.effective_git_config();
    if let Some(wc) = ws_config
        && let Some(ref gc) = wc.git_config
    {
        for (k, v) in gc {
            effective_gc.insert(k.clone(), v.clone());
        }
    }
    for (key, value) in &effective_gc {
        let from_ws = ws_config
            .and_then(|c| c.git_config.as_ref())
            .is_some_and(|gc| gc.contains_key(key));
        entries.push(ConfigListEntry {
            key: format!("git_config.{}", key),
            value: value.clone(),
            source: if from_ws {
                Some("workspace".to_string())
            } else {
                None
            },
        });
    }

    // language integrations: merge workspace overrides
    for name in crate::lang::integration_names() {
        let from_ws = ws_config
            .and_then(|c| c.language_integrations.as_ref())
            .is_some_and(|li| li.contains_key(name.as_str()));
        let enabled = if from_ws {
            ws_config
                .and_then(|c| c.language_integrations.as_ref())
                .and_then(|li| li.get(name.as_str()))
                .copied()
                .unwrap_or(false)
        } else {
            cfg.language_integrations
                .as_ref()
                .and_then(|m| m.get(name.as_str()))
                .copied()
                .unwrap_or(false)
        };
        entries.push(ConfigListEntry {
            key: format!("language-integrations.{}", name),
            value: enabled.to_string(),
            source: if from_ws {
                Some("workspace".to_string())
            } else {
                None
            },
        });
    }

    // experimental: same as global (workspace can't override)
    let exp = cfg.experimental.as_ref();
    let exp_enabled = exp.is_some_and(|e| e.enabled);
    entries.push(ConfigListEntry {
        key: "experimental".into(),
        value: exp_enabled.to_string(),
        source: None,
    });
    if exp_enabled {
        for feature in config::EXPERIMENTAL_FEATURES {
            let value = if *feature == "shell-tmux" {
                exp.and_then(|e| e.shell_tmux_mode())
                    .unwrap_or("false")
                    .to_string()
            } else {
                exp.is_some_and(|e| e.is_feature_enabled(feature))
                    .to_string()
            };
            entries.push(ConfigListEntry {
                key: format!("experimental.{}", feature),
                value,
                source: None,
            });
        }
    }

    Ok(Output::ConfigList(ConfigListOutput { entries }))
}

// ---------------------------------------------------------------------------
// Global config operations (existing behavior)
// ---------------------------------------------------------------------------

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
            source: None,
        },
        ConfigListEntry {
            key: "workspaces-dir".into(),
            value: paths.workspaces_dir.display().to_string(),
            source: None,
        },
        ConfigListEntry {
            key: "sync-strategy".into(),
            value: cfg.sync_strategy.as_deref().unwrap_or("rebase").to_string(),
            source: None,
        },
        ConfigListEntry {
            key: "agent-md".into(),
            value: cfg.agent_md.unwrap_or(true).to_string(),
            source: None,
        },
        ConfigListEntry {
            key: "gc.retention-days".into(),
            value: cfg.gc_retention_days.unwrap_or(7).to_string(),
            source: None,
        },
    ];

    // git config: show effective values (defaults merged with overrides)
    let git_config = cfg.effective_git_config();
    for (key, value) in &git_config {
        entries.push(ConfigListEntry {
            key: format!("git_config.{}", key),
            value: value.clone(),
            source: None,
        });
    }

    // language integrations: show effective value for all known integrations
    for name in crate::lang::integration_names() {
        let enabled = cfg
            .language_integrations
            .as_ref()
            .and_then(|m| m.get(name.as_str()))
            .copied()
            .unwrap_or(false);
        entries.push(ConfigListEntry {
            key: format!("language-integrations.{}", name),
            value: enabled.to_string(),
            source: None,
        });
    }

    // experimental: show gate and individual features (only when enabled)
    let exp = cfg.experimental.as_ref();
    let exp_enabled = exp.is_some_and(|e| e.enabled);
    entries.push(ConfigListEntry {
        key: "experimental".into(),
        value: exp_enabled.to_string(),
        source: None,
    });
    if exp_enabled {
        for feature in config::EXPERIMENTAL_FEATURES {
            let value = if *feature == "shell-tmux" {
                exp.and_then(|e| e.shell_tmux_mode())
                    .unwrap_or("false")
                    .to_string()
            } else {
                exp.is_some_and(|e| e.is_feature_enabled(feature))
                    .to_string()
            };
            entries.push(ConfigListEntry {
                key: format!("experimental.{}", feature),
                value,
                source: None,
            });
        }
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
                .unwrap_or(false);
            Ok(Output::ConfigGet(ConfigGetOutput {
                key: key.clone(),
                value: Some(enabled.to_string()),
            }))
        }
        k if k.starts_with("git_config.") => {
            let git_key = &k["git_config.".len()..];
            let effective = cfg.effective_git_config();
            Ok(Output::ConfigGet(ConfigGetOutput {
                key: key.clone(),
                value: effective.get(git_key).cloned(),
            }))
        }
        "experimental" => {
            let enabled = cfg.experimental.as_ref().is_some_and(|e| e.enabled);
            Ok(Output::ConfigGet(ConfigGetOutput {
                key: key.clone(),
                value: Some(enabled.to_string()),
            }))
        }
        "experimental.shell-tmux" => {
            let mode = cfg
                .experimental
                .as_ref()
                .and_then(|e| e.shell_tmux_mode())
                .unwrap_or("false");
            Ok(Output::ConfigGet(ConfigGetOutput {
                key: key.clone(),
                value: Some(mode.to_string()),
            }))
        }
        k if k.starts_with("experimental.") => {
            let feature = &k["experimental.".len()..];
            if !config::EXPERIMENTAL_FEATURES.contains(&feature) {
                bail!(
                    "unknown experimental feature: {} (known: {})",
                    feature,
                    config::EXPERIMENTAL_FEATURES.join(", ")
                );
            }
            let enabled = cfg
                .experimental
                .as_ref()
                .is_some_and(|e| e.is_feature_enabled(feature));
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
    let (message, hint) = match key.as_str() {
        "branch-prefix" => {
            let v = value.clone();
            filelock::with_config(&paths.config_path, |cfg| {
                cfg.branch_prefix = Some(v);
                Ok(())
            })?;
            (
                format!("branch-prefix = {}", value),
                Some(
                    "new workspaces will use this prefix; existing workspaces are unchanged".into(),
                ),
            )
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
            (
                format!("workspaces-dir = {}", value),
                Some(
                    "new workspaces will be created here; existing workspaces are not moved".into(),
                ),
            )
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
            (
                format!("sync-strategy = {}", value),
                Some(format!("wsp sync will use {} for all workspaces", value)),
            )
        }
        "agent-md" => {
            let enabled: bool = value
                .parse()
                .map_err(|_| anyhow::anyhow!("value must be true or false"))?;
            filelock::with_config(&paths.config_path, |cfg| {
                cfg.agent_md = Some(enabled);
                Ok(())
            })?;
            (
                format!("agent-md = {}", enabled),
                Some("takes effect on next wsp new or wsp sync".into()),
            )
        }
        "gc.retention-days" => {
            let days: u32 = value
                .parse()
                .map_err(|_| anyhow::anyhow!("value must be a non-negative integer"))?;
            filelock::with_config(&paths.config_path, |cfg| {
                cfg.gc_retention_days = Some(days);
                Ok(())
            })?;
            let hint = if days == 0 {
                "gc disabled: deleted workspaces kept indefinitely until manually purged".into()
            } else {
                format!(
                    "deleted workspaces recoverable via wsp recover for {} days",
                    days
                )
            };
            (format!("gc.retention-days = {}", days), Some(hint))
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
            (
                format!("language-integrations.{} = {}", lang, enabled),
                Some("takes effect on next wsp new or wsp sync".into()),
            )
        }
        k if k.starts_with("git_config.") => {
            let git_key = k["git_config.".len()..].to_string();
            if git_key.is_empty() {
                bail!("git_config key cannot be empty");
            }
            let v = value.clone();
            filelock::with_config(&paths.config_path, |cfg| {
                let gc = cfg.git_config.get_or_insert_with(BTreeMap::new);
                gc.insert(git_key.clone(), v);
                Ok(())
            })?;
            (
                format!("git_config.{} = {}", git_key, value),
                Some("applied to new clones; run wsp sync to update existing repos".into()),
            )
        }
        "experimental" => {
            let enabled: bool = value
                .parse()
                .map_err(|_| anyhow::anyhow!("value must be true or false"))?;
            filelock::with_config(&paths.config_path, |cfg| {
                let exp = cfg
                    .experimental
                    .get_or_insert_with(config::ExperimentalConfig::default);
                exp.enabled = enabled;
                Ok(())
            })?;
            let hint = if enabled {
                "use wsp config set experimental.<feature> true to enable specific features"
            } else {
                "all experimental features are now disabled"
            };
            (format!("experimental = {}", enabled), Some(hint.into()))
        }
        "experimental.shell-tmux" => {
            if !config::SHELL_TMUX_VALUES.contains(&value.as_str()) {
                bail!(
                    "shell-tmux must be one of: {}",
                    config::SHELL_TMUX_VALUES.join(", ")
                );
            }
            let val = if value == "false" {
                config::ExperimentalValue::Bool(false)
            } else {
                config::ExperimentalValue::String(value.clone())
            };
            let is_enabled = val.is_truthy();
            filelock::with_config(&paths.config_path, |cfg| {
                let exp = cfg
                    .experimental
                    .get_or_insert_with(config::ExperimentalConfig::default);
                if is_enabled {
                    exp.enabled = true;
                }
                exp.features.insert("shell-tmux".into(), val.clone());
                // Remove deprecated key if present
                exp.features.remove("shell-tmux-title");
                Ok(())
            })?;
            let msg = format!("experimental.shell-tmux = {}", value);
            (
                msg,
                config_set_hint_for_experimental("shell-tmux", is_enabled),
            )
        }
        k if k.starts_with("experimental.") => {
            let feature = &k["experimental.".len()..];
            if !config::EXPERIMENTAL_FEATURES.contains(&feature) {
                bail!(
                    "unknown experimental feature: {} (known: {})",
                    feature,
                    config::EXPERIMENTAL_FEATURES.join(", ")
                );
            }
            let enabled: bool = value
                .parse()
                .map_err(|_| anyhow::anyhow!("value must be true or false"))?;
            let feature = feature.to_string();
            filelock::with_config(&paths.config_path, |cfg| {
                let exp = cfg
                    .experimental
                    .get_or_insert_with(config::ExperimentalConfig::default);
                // Auto-enable the gate when enabling a specific feature
                if enabled {
                    exp.enabled = true;
                }
                exp.features
                    .insert(feature.clone(), config::ExperimentalValue::Bool(enabled));
                Ok(())
            })?;
            let msg = if enabled {
                format!("experimental.{} = true (experimental enabled)", feature)
            } else {
                format!("experimental.{} = false", feature)
            };
            (msg, config_set_hint_for_experimental(&feature, enabled))
        }
        _ => bail!("unknown config key: {}", key),
    };

    let mut out = MutationOutput::new(message);
    if let Some(h) = hint {
        out = out.with_hint(h);
    }
    Ok(Output::Mutation(out))
}

/// Extracts the hint from an Output::Mutation, if present.
#[cfg(test)]
fn extract_hint(output: &Output) -> Option<&str> {
    match output {
        Output::Mutation(m) => m.hint.as_deref(),
        _ => None,
    }
}

/// Returns a hint for setting an experimental feature, if applicable.
fn config_set_hint_for_experimental(feature: &str, enabled: bool) -> Option<String> {
    if !enabled {
        return None;
    }
    match feature {
        "shell-prompt" | "shell-tmux" => {
            Some("re-source your shell to activate: eval \"$(wsp completion zsh)\"".into())
        }
        _ => None,
    }
}

pub fn run_unset(matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    let key = matches.get_one::<String>("key").unwrap();

    let (message, hint) = match key.as_str() {
        "branch-prefix" => {
            filelock::with_config(&paths.config_path, |cfg| {
                cfg.branch_prefix = None;
                Ok(())
            })?;
            ("branch-prefix unset".into(), None)
        }
        "workspaces-dir" => {
            filelock::with_config(&paths.config_path, |cfg| {
                cfg.workspaces_dir = None;
                Ok(())
            })?;
            (
                "workspaces-dir unset (default: ~/dev/workspaces)".into(),
                None,
            )
        }
        "sync-strategy" => {
            filelock::with_config(&paths.config_path, |cfg| {
                cfg.sync_strategy = None;
                Ok(())
            })?;
            ("sync-strategy unset (default: rebase)".into(), None)
        }
        "agent-md" => {
            filelock::with_config(&paths.config_path, |cfg| {
                cfg.agent_md = None;
                Ok(())
            })?;
            ("agent-md unset (default: true)".into(), None)
        }
        "gc.retention-days" => {
            filelock::with_config(&paths.config_path, |cfg| {
                cfg.gc_retention_days = None;
                Ok(())
            })?;
            ("gc.retention-days unset (default: 7)".into(), None)
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
            (
                format!("language-integrations.{} unset (default: false)", lang),
                None,
            )
        }
        k if k.starts_with("git_config.") => {
            let git_key = k["git_config.".len()..].to_string();
            // Validate key even on unset — prevents confusing "unset" messages for invalid keys
            if git_key.is_empty() {
                bail!("git_config key cannot be empty");
            }
            let default_val = config::Config::default_git_config().get(&git_key).cloned();
            filelock::with_config(&paths.config_path, |cfg| {
                if let Some(ref mut m) = cfg.git_config {
                    m.remove(&git_key);
                    if m.is_empty() {
                        cfg.git_config = None;
                    }
                }
                Ok(())
            })?;
            let msg = match default_val {
                Some(v) => format!("git_config.{} unset (default: {})", git_key, v),
                None => format!("git_config.{} unset", git_key),
            };
            (msg, None)
        }
        "experimental" => {
            filelock::with_config(&paths.config_path, |cfg| {
                cfg.experimental = None;
                Ok(())
            })?;
            (
                "experimental unset (default: false)".into(),
                Some("all experimental features and their settings have been reset".into()),
            )
        }
        k if k.starts_with("experimental.") => {
            let feature = &k["experimental.".len()..];
            if !config::EXPERIMENTAL_FEATURES.contains(&feature) {
                bail!(
                    "unknown experimental feature: {} (known: {})",
                    feature,
                    config::EXPERIMENTAL_FEATURES.join(", ")
                );
            }
            let feature = feature.to_string();
            filelock::with_config(&paths.config_path, |cfg| {
                if let Some(ref mut exp) = cfg.experimental {
                    exp.features.remove(&feature);
                    // Also remove deprecated key when unsetting shell-tmux
                    if feature == "shell-tmux" {
                        exp.features.remove("shell-tmux-title");
                    }
                }
                Ok(())
            })?;
            let hint = config_set_hint_for_experimental(&feature, false);
            (
                format!("experimental.{} unset (default: false)", feature),
                hint,
            )
        }
        _ => bail!("unknown config key: {}", key),
    };

    let mut out = MutationOutput::new(message);
    if let Some(h) = hint {
        out = out.with_hint(h);
    }
    Ok(Output::Mutation(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Paths;

    fn test_paths(tmp: &std::path::Path) -> Paths {
        Paths {
            config_path: tmp.join("config.yaml"),
            mirrors_dir: tmp.join("mirrors"),
            gc_dir: tmp.join("gc"),
            templates_dir: tmp.join("templates"),
            workspaces_dir: tmp.join("workspaces"),
        }
    }

    /// Helper: run `wsp config set <key> <value>` and return the Output.
    fn do_set(paths: &Paths, key: &str, value: &str) -> Output {
        let cmd = set_cmd();
        let matches = cmd.get_matches_from(["set", key, value]);
        run_set(&matches, paths).unwrap()
    }

    /// Helper: run `wsp config unset <key>` and return the Output.
    fn do_unset(paths: &Paths, key: &str) -> Output {
        let cmd = unset_cmd();
        let matches = cmd.get_matches_from(["unset", key]);
        run_unset(&matches, paths).unwrap()
    }

    #[test]
    fn set_hints_present_for_all_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        config::Config::default()
            .save_to(&paths.config_path)
            .unwrap();

        let cases = vec![
            ("branch-prefix", "jg"),
            ("workspaces-dir", "/tmp/ws"),
            ("sync-strategy", "merge"),
            ("agent-md", "true"),
            ("gc.retention-days", "14"),
            ("language-integrations.go", "true"),
            ("git_config.push.default", "current"),
            ("experimental", "true"),
            ("experimental.shell-prompt", "true"),
            ("experimental.shell-tmux", "window-title"),
        ];

        for (key, value) in cases {
            let out = do_set(&paths, key, value);
            assert!(
                extract_hint(&out).is_some(),
                "config set {} should produce a hint",
                key,
            );
        }
    }

    #[test]
    fn set_experimental_shell_hint_mentions_shell() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        config::Config::default()
            .save_to(&paths.config_path)
            .unwrap();

        let out = do_set(&paths, "experimental.shell-prompt", "true");
        let hint = extract_hint(&out).unwrap();
        assert!(
            hint.contains("eval") && hint.contains("completion"),
            "shell feature hint should mention re-sourcing: got {:?}",
            hint
        );

        // shell-tmux also gets a re-source hint
        let out = do_set(&paths, "experimental.shell-tmux", "window-title");
        let hint = extract_hint(&out).unwrap();
        assert!(
            hint.contains("eval") && hint.contains("completion"),
            "shell-tmux hint should mention re-sourcing: got {:?}",
            hint
        );
    }

    #[test]
    fn set_experimental_disabled_no_shell_hint() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        config::Config::default()
            .save_to(&paths.config_path)
            .unwrap();

        // Enable first so we can disable
        do_set(&paths, "experimental.shell-prompt", "true");
        let out = do_set(&paths, "experimental.shell-prompt", "false");
        // Disabling should not produce a "re-source" hint
        let hint = extract_hint(&out);
        assert!(
            hint.is_none() || !hint.unwrap().contains("eval"),
            "disabling shell feature should not suggest re-sourcing"
        );
    }

    #[test]
    fn unset_experimental_warns_about_reset() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        config::Config::default()
            .save_to(&paths.config_path)
            .unwrap();

        do_set(&paths, "experimental.shell-prompt", "true");
        let out = do_unset(&paths, "experimental");
        let hint = extract_hint(&out).unwrap();
        assert!(
            hint.contains("reset"),
            "unsetting experimental should warn about reset: got {:?}",
            hint
        );
    }

    // -----------------------------------------------------------------------
    // Workspace-scoped config tests
    // -----------------------------------------------------------------------

    /// Create a minimal workspace directory with .wsp.yaml for testing.
    fn setup_workspace(tmp: &std::path::Path) -> std::path::PathBuf {
        let ws_dir = tmp.join("ws");
        std::fs::create_dir_all(&ws_dir).unwrap();
        let meta = workspace::Metadata {
            version: 0,
            name: "test-ws".into(),
            branch: "test/test-ws".into(),
            repos: BTreeMap::new(),
            created: chrono::Utc::now(),
            description: None,
            last_used: None,
            created_from: None,
            dirs: BTreeMap::new(),
            config: None,
        };
        workspace::save_metadata(&ws_dir, &meta).unwrap();
        ws_dir
    }

    fn extract_message(output: &Output) -> &str {
        match output {
            Output::Mutation(m) => &m.message,
            _ => panic!("expected Mutation output"),
        }
    }

    fn extract_config_entries(output: &Output) -> &[ConfigListEntry] {
        match output {
            Output::ConfigList(l) => &l.entries,
            _ => panic!("expected ConfigList output"),
        }
    }

    fn extract_config_value(output: &Output) -> Option<&str> {
        match output {
            Output::ConfigGet(g) => g.value.as_deref(),
            _ => panic!("expected ConfigGet output"),
        }
    }

    #[test]
    fn workspace_set_and_get_sync_strategy() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        config::Config::default()
            .save_to(&paths.config_path)
            .unwrap();
        let ws_dir = setup_workspace(tmp.path());

        let cmd = set_cmd();
        let m = cmd.get_matches_from(["set", "sync-strategy", "merge"]);
        let out = run_set_workspace(&m, &ws_dir, &paths).unwrap();
        assert!(
            extract_message(&out).contains("workspace: test-ws"),
            "should mention workspace name"
        );

        let cmd = get_cmd();
        let m = cmd.get_matches_from(["get", "sync-strategy"]);
        let out = run_get_workspace(&m, &ws_dir, &paths).unwrap();
        assert_eq!(extract_config_value(&out), Some("merge"));
    }

    #[test]
    fn workspace_get_falls_back_to_global() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let mut cfg = config::Config::default();
        cfg.sync_strategy = Some("merge".into());
        cfg.save_to(&paths.config_path).unwrap();
        let ws_dir = setup_workspace(tmp.path());

        let cmd = get_cmd();
        let m = cmd.get_matches_from(["get", "sync-strategy"]);
        let out = run_get_workspace(&m, &ws_dir, &paths).unwrap();
        assert_eq!(extract_config_value(&out), Some("merge"));
    }

    #[test]
    fn workspace_set_rejects_global_only_key() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        config::Config::default()
            .save_to(&paths.config_path)
            .unwrap();
        let ws_dir = setup_workspace(tmp.path());

        let cases = vec![
            "branch-prefix",
            "workspaces-dir",
            "gc.retention-days",
            "agent-md",
            "experimental",
            "experimental.shell-prompt",
        ];
        for key in cases {
            let cmd = set_cmd();
            let m = cmd.get_matches_from(["set", key, "test"]);
            let result = run_set_workspace(&m, &ws_dir, &paths);
            assert!(result.is_err(), "set {} should fail", key);
            let err = result.err().unwrap();
            assert!(
                format!("{}", err).contains("global-only"),
                "set {} should be rejected as global-only, got: {}",
                key,
                err
            );
        }
    }

    #[test]
    fn workspace_unset_falls_back_to_global() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let mut cfg = config::Config::default();
        cfg.sync_strategy = Some("merge".into());
        cfg.save_to(&paths.config_path).unwrap();
        let ws_dir = setup_workspace(tmp.path());

        // Set workspace override then unset it
        let cmd = set_cmd();
        let m = cmd.get_matches_from(["set", "sync-strategy", "rebase"]);
        run_set_workspace(&m, &ws_dir, &paths).unwrap();

        let cmd = unset_cmd();
        let m = cmd.get_matches_from(["unset", "sync-strategy"]);
        let out = run_unset_workspace(&m, &ws_dir, &paths).unwrap();
        let msg = extract_message(&out);
        assert!(msg.contains("using global: merge"), "got: {}", msg);

        // Verify get returns global value
        let cmd = get_cmd();
        let m = cmd.get_matches_from(["get", "sync-strategy"]);
        let out = run_get_workspace(&m, &ws_dir, &paths).unwrap();
        assert_eq!(extract_config_value(&out), Some("merge"));
    }

    #[test]
    fn workspace_list_shows_source_annotation() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        config::Config::default()
            .save_to(&paths.config_path)
            .unwrap();
        let ws_dir = setup_workspace(tmp.path());

        // Set a workspace override
        let cmd = set_cmd();
        let m = cmd.get_matches_from(["set", "sync-strategy", "merge"]);
        run_set_workspace(&m, &ws_dir, &paths).unwrap();

        let cmd = list_cmd();
        let m = cmd.get_matches_from(["ls"]);
        let out = run_list_workspace(&m, &ws_dir, &paths).unwrap();
        let entries = extract_config_entries(&out);

        let sync_entry = entries.iter().find(|e| e.key == "sync-strategy").unwrap();
        assert_eq!(sync_entry.value, "merge");
        assert_eq!(
            sync_entry.source.as_deref(),
            Some("workspace"),
            "should be annotated as workspace source"
        );

        let prefix_entry = entries.iter().find(|e| e.key == "branch-prefix").unwrap();
        assert!(
            prefix_entry.source.is_none(),
            "global-only keys should have no source annotation"
        );
    }

    #[test]
    fn workspace_config_metadata_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let ws_dir = setup_workspace(tmp.path());

        // Verify config survives save/load
        filelock::with_metadata(&ws_dir, |meta| {
            let config = meta
                .config
                .get_or_insert_with(template::TemplateConfig::default);
            config.sync_strategy = Some("merge".into());
            config.git_config = Some({
                let mut m = BTreeMap::new();
                m.insert("push.default".into(), "simple".into());
                m
            });
            Ok(())
        })
        .unwrap();

        let meta = workspace::load_metadata(&ws_dir).unwrap();
        let config = meta.config.as_ref().unwrap();
        assert_eq!(config.sync_strategy.as_deref(), Some("merge"));
        assert_eq!(
            config.git_config.as_ref().unwrap().get("push.default"),
            Some(&"simple".to_string())
        );
    }

    #[test]
    fn apply_workspace_config_hierarchy() {
        let mut global = config::Config::default();
        global.sync_strategy = Some("rebase".into());
        global.git_config = Some({
            let mut m = BTreeMap::new();
            m.insert("push.default".into(), "current".into());
            m.insert("rerere.enabled".into(), "true".into());
            m
        });

        let meta = workspace::Metadata {
            version: 0,
            name: "test".into(),
            branch: "test/test".into(),
            repos: BTreeMap::new(),
            created: chrono::Utc::now(),
            description: None,
            last_used: None,
            created_from: None,
            dirs: BTreeMap::new(),
            config: Some(template::TemplateConfig {
                sync_strategy: Some("merge".into()),
                git_config: Some({
                    let mut m = BTreeMap::new();
                    m.insert("push.default".into(), "simple".into());
                    m
                }),
                language_integrations: None,
            }),
        };

        let effective = meta.apply_workspace_config(&global);
        // Workspace overrides
        assert_eq!(effective.sync_strategy.as_deref(), Some("merge"));
        assert_eq!(
            effective.git_config.as_ref().unwrap().get("push.default"),
            Some(&"simple".to_string())
        );
        // Global preserved for non-overridden keys
        assert_eq!(
            effective.git_config.as_ref().unwrap().get("rerere.enabled"),
            Some(&"true".to_string())
        );
    }
}

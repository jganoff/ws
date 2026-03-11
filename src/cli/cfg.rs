use std::collections::BTreeMap;

use anyhow::{Result, bail};
use clap::{Arg, ArgMatches, Command};
use clap_complete::engine::ArgValueCandidates;

use crate::cli::completers;
use crate::config::{self, Paths};
use crate::filelock;
use crate::output::{ConfigGetOutput, ConfigListEntry, ConfigListOutput, MutationOutput, Output};

pub fn cmd() -> Command {
    Command::new("config")
        .about("Manage wsp settings")
        .long_about(
            "Manage wsp settings.\n\n\
             Settings are stored in ~/.local/share/wsp/config.yaml. Keys include \
             branch-prefix, workspaces-dir, gc.retention-days, git_config.* overrides, \
             and language integration toggles. Use `wsp config ls` to see all current values.",
        )
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
        None => run_list(matches, paths),
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
        .arg(
            Arg::new("key")
                .required(true)
                .add(ArgValueCandidates::new(completers::complete_config_keys)),
        )
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
}

pub fn unset_cmd() -> Command {
    Command::new("unset").about("Unset a config value").arg(
        Arg::new("key")
            .required(true)
            .add(ArgValueCandidates::new(completers::complete_config_keys)),
    )
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

    // git config: show effective values (defaults merged with overrides)
    let git_config = cfg.effective_git_config();
    for (key, value) in &git_config {
        entries.push(ConfigListEntry {
            key: format!("git_config.{}", key),
            value: value.clone(),
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
        });
    }

    // experimental: show gate and individual features (only when enabled)
    let exp = cfg.experimental.as_ref();
    let exp_enabled = exp.is_some_and(|e| e.enabled);
    entries.push(ConfigListEntry {
        key: "experimental".into(),
        value: exp_enabled.to_string(),
    });
    if exp_enabled {
        for feature in config::EXPERIMENTAL_FEATURES {
            let enabled = exp
                .and_then(|e| e.features.get(*feature))
                .copied()
                .unwrap_or(false);
            entries.push(ConfigListEntry {
                key: format!("experimental.{}", feature),
                value: enabled.to_string(),
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
                exp.features.insert(feature.clone(), enabled);
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
        "shell-prompt" | "shell-tmux-title" => {
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
            ("experimental.shell-tmux-title", "true"),
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
}

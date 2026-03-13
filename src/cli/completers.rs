// Test completions from the command line with:
//   _CLAP_IFS=$'\n' _CLAP_COMPLETE_INDEX=<N> COMPLETE=zsh target/release/wsp -- wsp <words...>
// where N is the 0-based index of the word to complete.
use clap_complete::engine::CompletionCandidate;

use crate::config::{Config, Paths};
use crate::giturl;
use crate::template;
use crate::workspace;

pub fn complete_templates() -> Vec<CompletionCandidate> {
    let Ok(paths) = Paths::resolve() else {
        return Vec::new();
    };
    let Ok(names) = template::list(&paths.templates_dir) else {
        return Vec::new();
    };
    names.into_iter().map(CompletionCandidate::new).collect()
}

pub fn complete_repos() -> Vec<CompletionCandidate> {
    let Ok(paths) = Paths::resolve() else {
        return Vec::new();
    };
    let Ok(cfg) = Config::load_from(&paths.config_path) else {
        return Vec::new();
    };
    repos_to_candidates(cfg.repos.keys().cloned().collect())
}

/// Complete only repos in the current workspace (for `ws repo rm`).
pub fn complete_workspace_repos() -> Vec<CompletionCandidate> {
    let Ok(cwd) = std::env::current_dir() else {
        return Vec::new();
    };
    let Ok(ws_dir) = workspace::detect(&cwd) else {
        return Vec::new();
    };
    let Ok(meta) = workspace::load_metadata(&ws_dir) else {
        return Vec::new();
    };
    repos_to_candidates(meta.repos.keys().cloned().collect())
}

/// Complete repos in a named template (for `template repo rm`).
pub fn complete_template_repos() -> Vec<CompletionCandidate> {
    let Some(name) = template_name_from_args() else {
        return Vec::new();
    };
    let Ok(paths) = Paths::resolve() else {
        return Vec::new();
    };
    let Ok(tmpl) = template::load(&paths.templates_dir, &name) else {
        return Vec::new();
    };
    let identities: Vec<String> = tmpl
        .repos
        .iter()
        .filter_map(|r| giturl::parse(&r.url).ok().map(|p| p.identity()))
        .collect();
    repos_to_candidates(identities)
}

/// Complete valid template config key prefixes.
pub fn complete_template_config_keys() -> Vec<CompletionCandidate> {
    vec![
        CompletionCandidate::new("sync-strategy"),
        CompletionCandidate::new("lang."),
        CompletionCandidate::new("git."),
    ]
}

/// Complete config keys for `wsp config get/set/unset`.
pub fn complete_config_keys() -> Vec<CompletionCandidate> {
    let mut keys: Vec<CompletionCandidate> = vec![
        CompletionCandidate::new("branch-prefix"),
        CompletionCandidate::new("workspaces-dir"),
        CompletionCandidate::new("sync-strategy"),
        CompletionCandidate::new("agent-md"),
        CompletionCandidate::new("gc.retention-days"),
        CompletionCandidate::new("shell.tmux"),
        CompletionCandidate::new("shell.prompt"),
    ];

    // lang.<name> keys
    for name in crate::lang::integration_names() {
        keys.push(CompletionCandidate::new(format!("lang.{}", name)));
    }

    // git.* — show defaults as suggestions
    for key in crate::config::Config::default_git_config().keys() {
        keys.push(CompletionCandidate::new(format!("git.{}", key)));
    }

    keys
}

/// Complete config values for `wsp config set` based on the key being set.
pub fn complete_config_values() -> Vec<CompletionCandidate> {
    // Inspect prior args to find the key
    let args: Vec<String> = std::env::args().collect();
    // Pattern: wsp config set <key> <value>
    let key = args
        .iter()
        .position(|a| a == "set")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str());

    match key {
        Some("sync-strategy") => vec![
            CompletionCandidate::new("rebase"),
            CompletionCandidate::new("merge"),
        ],
        Some("agent-md" | "shell.prompt") => bool_candidates(),
        Some("shell.tmux") => crate::config::SHELL_TMUX_VALUES
            .iter()
            .map(|v| CompletionCandidate::new(*v))
            .collect(),
        Some(k) if k.starts_with("lang.") => bool_candidates(),
        _ => Vec::new(),
    }
}

fn bool_candidates() -> Vec<CompletionCandidate> {
    vec![
        CompletionCandidate::new("true"),
        CompletionCandidate::new("false"),
    ]
}

pub fn complete_workspaces() -> Vec<CompletionCandidate> {
    let Ok(paths) = Paths::resolve() else {
        return Vec::new();
    };
    let Ok(names) = workspace::list_all(&paths.workspaces_dir) else {
        return Vec::new();
    };
    names.into_iter().map(CompletionCandidate::new).collect()
}

fn repos_to_candidates(identities: Vec<String>) -> Vec<CompletionCandidate> {
    let shortnames = giturl::shortnames(&identities);
    shortnames
        .into_iter()
        .map(|(identity, short)| CompletionCandidate::new(short).help(Some(identity.into())))
        .collect()
}

/// Extract the template name from `["template", "repo"|"config"|"agent-md", "add"|"rm"|"set"|"get"|"unset", <name>]`.
fn template_name_from_args() -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    let pos = args.iter().position(|a| a == "template")?;
    // template <sub-noun> <verb> <name>
    args.get(pos + 3).filter(|a| !a.starts_with('-')).cloned()
}

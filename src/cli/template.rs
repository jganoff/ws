use std::fs;
use std::io::{Read as _, Write};

use anyhow::Result;
use clap::{Arg, ArgMatches, Command};
use clap_complete::engine::ArgValueCandidates;

use crate::config::{self, Paths};
use crate::filelock;
use crate::giturl;
use crate::output::{
    ConfigGetOutput, MutationOutput, Output, TemplateListEntry, TemplateListOutput,
    TemplateShowOutput,
};
use crate::template as tmpl;

use super::completers;

pub fn cmd() -> Command {
    Command::new("template")
        .about("Manage workspace templates")
        .long_about(
            "Manage workspace templates.\n\n\
             Templates define reusable workspace configurations: a set of repos, optional \
             config overrides, and optional AGENTS.md content for AI coding assistants. \
             Create workspaces from templates with `wsp new -t <name>`.",
        )
        .subcommand_required(true)
        .subcommand(new_cmd())
        .subcommand(list_cmd())
        .subcommand(show_cmd())
        .subcommand(rm_cmd())
        .subcommand(export_cmd())
        .subcommand(repo_cmd())
        .subcommand(config_cmd())
        .subcommand(agent_md_cmd())
}

pub fn dispatch(matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    // Auto-migrate any existing groups to templates
    if let Ok(cfg) = config::Config::load_from(&paths.config_path)
        && !cfg.groups.is_empty()
    {
        let _ = tmpl::migrate_all_groups(&paths.templates_dir, &cfg);
    }

    match matches.subcommand() {
        Some(("new", m)) => run_new(m, paths),
        Some(("ls", m)) => run_list(m, paths),
        Some(("show", m)) => run_show(m, paths),
        Some(("rm", m)) => run_rm(m, paths),
        Some(("export", m)) => run_export(m, paths),
        Some(("repo", m)) => dispatch_repo(m, paths),
        Some(("config", m)) => dispatch_config(m, paths),
        Some(("agent-md", m)) => dispatch_agent_md(m, paths),
        _ => unreachable!(),
    }
}

fn new_cmd() -> Command {
    Command::new("new")
        .about("Create a new template")
        .arg(Arg::new("name").required(true))
        .arg(
            Arg::new("repos")
                .num_args(1..)
                .help("Repo URLs for the template"),
        )
        .arg(
            Arg::new("from-workspace")
                .short('w')
                .long("workspace")
                .help("Create from an existing workspace")
                .add(ArgValueCandidates::new(completers::complete_workspaces)),
        )
        .arg(
            Arg::new("file")
                .short('f')
                .long("file")
                .help("Create from a template file (.yaml)")
                .value_hint(clap::ValueHint::FilePath),
        )
        .group(
            clap::ArgGroup::new("source")
                .args(["repos", "from-workspace", "file"])
                .required(true),
        )
}

fn list_cmd() -> Command {
    Command::new("ls")
        .visible_alias("list")
        .about("List all templates [read-only]")
}

fn show_cmd() -> Command {
    Command::new("show")
        .about("Show template contents [read-only]")
        .arg(
            Arg::new("name")
                .required(true)
                .add(ArgValueCandidates::new(completers::complete_templates)),
        )
}

fn rm_cmd() -> Command {
    Command::new("rm")
        .visible_alias("remove")
        .about("Remove a template")
        .arg(
            Arg::new("name")
                .required(true)
                .add(ArgValueCandidates::new(completers::complete_templates)),
        )
}

fn export_cmd() -> Command {
    Command::new("export")
        .about("Export a template to a file or stdout [read-only]")
        .arg(
            Arg::new("name")
                .required(true)
                .add(ArgValueCandidates::new(completers::complete_templates)),
        )
        .arg(
            Arg::new("stdout")
                .long("stdout")
                .action(clap::ArgAction::SetTrue)
                .help("Print to stdout instead of writing a file"),
        )
}

fn run_new(matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    let name = matches.get_one::<String>("name").unwrap();
    let from_workspace = matches.get_one::<String>("from-workspace");
    let from_file = matches.get_one::<String>("file");

    if tmpl::exists(&paths.templates_dir, name) {
        anyhow::bail!("template {:?} already exists", name);
    }

    let template = if let Some(ws_name) = from_workspace {
        tmpl::from_workspace(paths, ws_name)?
    } else if let Some(file_path) = from_file {
        tmpl::load_from_file(std::path::Path::new(file_path))?
    } else {
        // Safe to unwrap: clap ArgGroup ensures repos, --workspace, or --file is present
        let repo_urls: Vec<String> = matches
            .get_many::<String>("repos")
            .unwrap()
            .cloned()
            .collect();

        // Validate all URLs are parseable
        for url in &repo_urls {
            giturl::parse(url).map_err(|e| anyhow::anyhow!("invalid repo URL {:?}: {}", url, e))?;
        }

        tmpl::Template {
            repos: repo_urls
                .into_iter()
                .map(|url| tmpl::TemplateRepo { url })
                .collect(),
            config: None,
            agent_md: None,
        }
    };

    template.print_customizations();

    let repo_count = template.repos.len();
    tmpl::save(&paths.templates_dir, name, &template)?;

    Ok(Output::Mutation(MutationOutput::new(format!(
        "Created template {:?} with {} repos",
        name, repo_count
    ))))
}

fn run_list(_matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    let names = tmpl::list(&paths.templates_dir)?;

    let mut templates = Vec::new();
    for name in &names {
        match tmpl::load(&paths.templates_dir, name) {
            Ok(t) => templates.push(TemplateListEntry {
                name: name.clone(),
                repo_count: t.repos.len(),
            }),
            Err(e) => {
                eprintln!("warning: skipping template {:?}: {}", name, e);
            }
        }
    }

    Ok(Output::TemplateList(TemplateListOutput { templates }))
}

fn run_show(matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    let name = matches.get_one::<String>("name").unwrap();
    let t = tmpl::load(&paths.templates_dir, name)?;

    let repos: Vec<crate::output::TemplateShowRepo> = t
        .repos
        .iter()
        .map(|r| {
            let identity = giturl::parse(&r.url)
                .map(|p| p.identity())
                .unwrap_or_default();
            crate::output::TemplateShowRepo {
                url: r.url.clone(),
                identity,
            }
        })
        .collect();

    Ok(Output::TemplateShow(TemplateShowOutput {
        name: name.clone(),
        repos,
    }))
}

fn run_rm(matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    let name = matches.get_one::<String>("name").unwrap().clone();

    tmpl::delete(&paths.templates_dir, &name)?;

    Ok(Output::Mutation(MutationOutput::new(format!(
        "Removed template {:?}",
        name
    ))))
}

fn run_export(matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    let name = matches.get_one::<String>("name").unwrap();
    let to_stdout = matches.get_flag("stdout");

    let t = tmpl::load(&paths.templates_dir, name)?;

    // Report what's being exported
    if !to_stdout {
        eprintln!("Exporting template {:?} ({} repos):", name, t.repos.len());
        t.print_customizations();
        eprintln!("  note: custom skills (.claude/skills/) are not included in exports");
    }

    let yaml = tmpl::to_yaml(&t)?;

    if to_stdout {
        print!("{}", yaml);
        Ok(Output::None)
    } else {
        let filename = format!("{}.wsp.yaml", name);
        let dest = std::env::current_dir()?.join(&filename);
        if dest.exists() {
            anyhow::bail!("{:?} already exists", filename);
        }
        let mut f = fs::File::create(&dest)?;
        f.write_all(yaml.as_bytes())?;
        Ok(Output::Mutation(MutationOutput::new(format!(
            "Exported template to {}",
            dest.display()
        ))))
    }
}

// ---------------------------------------------------------------------------
// template repo add/rm
// ---------------------------------------------------------------------------

fn repo_cmd() -> Command {
    Command::new("repo")
        .about("Add or remove repos in a template")
        .long_about(
            "Add or remove repos in an existing template.\n\n\
             Mirrors `wsp repo add/rm` but operates on a stored template instead of \
             a workspace. `repo add` is idempotent — repos already present are skipped \
             with a warning.",
        )
        .subcommand_required(true)
        .subcommand(
            Command::new("add")
                .about("Add repos to a template")
                .arg(
                    Arg::new("name")
                        .required(true)
                        .add(ArgValueCandidates::new(completers::complete_templates)),
                )
                .arg(
                    Arg::new("repos")
                        .required(true)
                        .num_args(1..)
                        .help("Repo URLs or shortnames to add")
                        .add(ArgValueCandidates::new(completers::complete_repos)),
                ),
        )
        .subcommand(
            Command::new("rm")
                .visible_alias("remove")
                .about("Remove repos from a template")
                .arg(
                    Arg::new("name")
                        .required(true)
                        .add(ArgValueCandidates::new(completers::complete_templates)),
                )
                .arg(
                    Arg::new("repos")
                        .required(true)
                        .num_args(1..)
                        .help("Repo URLs, identities, or shortnames to remove")
                        .add(ArgValueCandidates::new(completers::complete_template_repos)),
                ),
        )
}

fn dispatch_repo(matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    match matches.subcommand() {
        Some(("add", m)) => run_repo_add(m, paths),
        Some(("rm", m)) => run_repo_rm(m, paths),
        _ => unreachable!(),
    }
}

fn run_repo_add(matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    let name = matches.get_one::<String>("name").unwrap();
    let repos: Vec<String> = matches
        .get_many::<String>("repos")
        .unwrap()
        .cloned()
        .collect();

    let template = filelock::with_template(&paths.templates_dir, name, |tmpl| {
        let skipped = tmpl::add_repos(tmpl, repos)?;
        for url in &skipped {
            eprintln!("warning: repo {:?} already in template, skipping", url);
        }
        Ok(())
    })?;

    let show = template_show_output(name, &template);
    Ok(Output::TemplateShow(show))
}

fn run_repo_rm(matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    let name = matches.get_one::<String>("name").unwrap();
    let repos: Vec<String> = matches
        .get_many::<String>("repos")
        .unwrap()
        .cloned()
        .collect();

    let template = filelock::with_template(&paths.templates_dir, name, |tmpl| {
        tmpl::remove_repos(tmpl, repos)?;
        if tmpl.repos.is_empty() {
            anyhow::bail!("cannot remove all repos from template — use `wsp template rm` instead");
        }
        Ok(())
    })?;

    let show = template_show_output(name, &template);
    Ok(Output::TemplateShow(show))
}

// ---------------------------------------------------------------------------
// template config set/get/unset
// ---------------------------------------------------------------------------

fn config_cmd() -> Command {
    Command::new("config")
        .about("Manage template config overrides")
        .long_about(
            "Manage template-scoped config overrides.\n\n\
             Template config overrides global config when a workspace is created from the \
             template. Valid keys: language-integrations.<name>, sync-strategy, \
             git-config.<key>.",
        )
        .subcommand_required(true)
        .subcommand(
            Command::new("set")
                .about("Set a template config override")
                .arg(
                    Arg::new("name")
                        .required(true)
                        .add(ArgValueCandidates::new(completers::complete_templates)),
                )
                .arg(Arg::new("key").required(true).add(ArgValueCandidates::new(
                    completers::complete_template_config_keys,
                )))
                .arg(Arg::new("value").required(true)),
        )
        .subcommand(
            Command::new("get")
                .about("Get a template config value [read-only]")
                .arg(
                    Arg::new("name")
                        .required(true)
                        .add(ArgValueCandidates::new(completers::complete_templates)),
                )
                .arg(Arg::new("key").required(true).add(ArgValueCandidates::new(
                    completers::complete_template_config_keys,
                ))),
        )
        .subcommand(
            Command::new("unset")
                .about("Unset a template config override")
                .arg(
                    Arg::new("name")
                        .required(true)
                        .add(ArgValueCandidates::new(completers::complete_templates)),
                )
                .arg(Arg::new("key").required(true).add(ArgValueCandidates::new(
                    completers::complete_template_config_keys,
                ))),
        )
}

fn dispatch_config(matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    match matches.subcommand() {
        Some(("set", m)) => run_config_set(m, paths),
        Some(("get", m)) => run_config_get(m, paths),
        Some(("unset", m)) => run_config_unset(m, paths),
        _ => unreachable!(),
    }
}

fn run_config_set(matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    let name = matches.get_one::<String>("name").unwrap();
    let key = matches.get_one::<String>("key").unwrap().clone();
    let value = matches.get_one::<String>("value").unwrap().clone();

    filelock::with_template(&paths.templates_dir, name, |tmpl| {
        tmpl::set_config(tmpl, &key, &value)
    })?;

    Ok(Output::Mutation(MutationOutput::new(format!(
        "template {:?}: {} = {}",
        name, key, value
    ))))
}

fn run_config_get(matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    let name = matches.get_one::<String>("name").unwrap();
    let key = matches.get_one::<String>("key").unwrap();

    // Read-only: no lock needed
    let template = tmpl::load(&paths.templates_dir, name)?;
    let value = tmpl::get_config(&template, key)?;

    Ok(Output::ConfigGet(ConfigGetOutput {
        key: key.clone(),
        value,
    }))
}

fn run_config_unset(matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    let name = matches.get_one::<String>("name").unwrap();
    let key = matches.get_one::<String>("key").unwrap().clone();

    filelock::with_template(&paths.templates_dir, name, |tmpl| {
        tmpl::unset_config(tmpl, &key)
    })?;

    Ok(Output::Mutation(MutationOutput::new(format!(
        "template {:?}: {} unset",
        name, key
    ))))
}

// ---------------------------------------------------------------------------
// template agent-md set/unset
// ---------------------------------------------------------------------------

fn agent_md_cmd() -> Command {
    Command::new("agent-md")
        .about("Manage template AGENTS.md content")
        .long_about(
            "Manage template AGENTS.md content.\n\n\
             Set custom AGENTS.md content that will be included in workspaces created from \
             this template. Use `-` as the path to read from stdin.",
        )
        .subcommand_required(true)
        .subcommand(
            Command::new("set")
                .about("Set AGENTS.md content from a file (use - for stdin)")
                .arg(
                    Arg::new("name")
                        .required(true)
                        .add(ArgValueCandidates::new(completers::complete_templates)),
                )
                .arg(
                    Arg::new("path")
                        .required(true)
                        .help("File path (or - for stdin)")
                        .value_hint(clap::ValueHint::FilePath),
                ),
        )
        .subcommand(
            Command::new("unset")
                .about("Clear AGENTS.md content from a template")
                .arg(
                    Arg::new("name")
                        .required(true)
                        .add(ArgValueCandidates::new(completers::complete_templates)),
                ),
        )
}

fn dispatch_agent_md(matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    match matches.subcommand() {
        Some(("set", m)) => run_agent_md_set(m, paths),
        Some(("unset", m)) => run_agent_md_unset(m, paths),
        _ => unreachable!(),
    }
}

fn run_agent_md_set(matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    let name = matches.get_one::<String>("name").unwrap();
    let path = matches.get_one::<String>("path").unwrap();

    // Read content before acquiring lock — don't hold lock during I/O
    let content = if path == "-" {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        buf
    } else {
        fs::read_to_string(path).map_err(|e| anyhow::anyhow!("reading {:?}: {}", path, e))?
    };

    if content.trim().is_empty() {
        anyhow::bail!("agent-md content is empty — use `agent-md unset` to clear");
    }

    // Guard against accidentally loading huge files
    const MAX_AGENT_MD_BYTES: usize = 1_048_576; // 1 MiB
    if content.len() > MAX_AGENT_MD_BYTES {
        anyhow::bail!(
            "agent-md content is {} bytes, exceeds 1 MiB limit",
            content.len()
        );
    }

    // Validate no wsp markers
    if content.contains(crate::agentmd::MARKER_BEGIN)
        || content.contains(crate::agentmd::MARKER_END)
    {
        anyhow::bail!("agent_md content cannot contain wsp markers (<!-- wsp:begin/end -->)");
    }

    filelock::with_template(&paths.templates_dir, name, |tmpl| {
        tmpl.agent_md = Some(content);
        Ok(())
    })?;

    Ok(Output::Mutation(MutationOutput::new(format!(
        "template {:?}: agent-md set",
        name
    ))))
}

fn run_agent_md_unset(matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    let name = matches.get_one::<String>("name").unwrap();

    filelock::with_template(&paths.templates_dir, name, |tmpl| {
        tmpl.agent_md = None;
        Ok(())
    })?;

    Ok(Output::Mutation(MutationOutput::new(format!(
        "template {:?}: agent-md unset",
        name
    ))))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn template_show_output(name: &str, template: &tmpl::Template) -> TemplateShowOutput {
    let repos = template
        .repos
        .iter()
        .map(|r| {
            let identity = giturl::parse(&r.url)
                .map(|p| p.identity())
                .unwrap_or_default();
            crate::output::TemplateShowRepo {
                url: r.url.clone(),
                identity,
            }
        })
        .collect();

    TemplateShowOutput {
        name: name.to_string(),
        repos,
    }
}

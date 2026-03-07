use anyhow::Result;
use clap::{Arg, ArgMatches, Command};
use clap_complete::engine::ArgValueCandidates;

use crate::config::Paths;
use crate::giturl;
use crate::output::{
    MutationOutput, Output, TemplateListEntry, TemplateListOutput, TemplateShowOutput,
};
use crate::template as tmpl;

use super::completers;

pub fn cmd() -> Command {
    Command::new("template")
        .about("Manage workspace templates")
        .subcommand_required(true)
        .subcommand(new_cmd())
        .subcommand(list_cmd())
        .subcommand(show_cmd())
        .subcommand(rm_cmd())
}

pub fn dispatch(matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    match matches.subcommand() {
        Some(("new", m)) => run_new(m, paths),
        Some(("ls", m)) => run_list(m, paths),
        Some(("show", m)) => run_show(m, paths),
        Some(("rm", m)) => run_rm(m, paths),
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
                .long("from-workspace")
                .help("Derive template from an existing workspace")
                .add(ArgValueCandidates::new(completers::complete_workspaces)),
        )
        .group(
            clap::ArgGroup::new("source")
                .args(["repos", "from-workspace"])
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

fn run_new(matches: &ArgMatches, paths: &Paths) -> Result<Output> {
    let name = matches.get_one::<String>("name").unwrap();
    let from_workspace = matches.get_one::<String>("from-workspace");

    if tmpl::exists(&paths.templates_dir, name) {
        anyhow::bail!("template {:?} already exists", name);
    }

    let template = if let Some(ws_name) = from_workspace {
        tmpl::from_workspace(paths, ws_name)?
    } else {
        // Safe to unwrap: clap ArgGroup ensures either repos or --from-workspace is present
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
        }
    };

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

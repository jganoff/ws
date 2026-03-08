// ---------------------------------------------------------------------------
// Manpage generation (codegen only)
// ---------------------------------------------------------------------------

#[cfg(feature = "codegen")]
use anyhow::Result;

#[cfg(feature = "codegen")]
use clap::{Arg, ArgMatches, Command};

#[cfg(feature = "codegen")]
use crate::config::Paths;

#[cfg(feature = "codegen")]
use crate::output::Output;

#[cfg(feature = "codegen")]
pub fn generate_man_cmd() -> Command {
    Command::new("generate-man")
        .about("Generate manpages from CLI introspection (dev only)")
        .arg(
            Arg::new("out-dir")
                .required(true)
                .help("Directory to write manpages to"),
        )
}

#[cfg(feature = "codegen")]
pub fn run_generate_man(matches: &ArgMatches, _paths: &Paths) -> Result<Output> {
    let out_dir = matches.get_one::<String>("out-dir").unwrap();
    std::fs::create_dir_all(out_dir)?;

    let cli = super::build_cli();
    generate_manpages(&cli, out_dir, &[])?;

    Ok(Output::None)
}

#[cfg(feature = "codegen")]
fn generate_manpages(cmd: &Command, out_dir: &str, prefix: &[&str]) -> Result<()> {
    use std::io::Write;

    let name = cmd.get_name();

    // Skip hidden commands (setup, generate, generate-man).
    // Aliases are not surfaced by get_subcommands() in clap 4, so no dedup needed.
    if cmd.is_hide_set() {
        return Ok(());
    }

    // Build the manpage name: wsp, wsp-new, wsp-repo-add, etc.
    let man_name = if prefix.is_empty() {
        name.to_string()
    } else {
        let mut parts: Vec<&str> = prefix.to_vec();
        parts.push(name);
        parts.join("-")
    };

    let mut buf = Vec::new();
    clap_mangen::Man::new(cmd.clone())
        .title(&man_name)
        .render(&mut buf)?;

    let path = std::path::Path::new(out_dir).join(format!("{}.1", man_name));
    let mut file = std::fs::File::create(&path)?;
    file.write_all(&buf)?;

    // Build prefix for children
    let child_prefix: Vec<&str> = if prefix.is_empty() {
        vec![name]
    } else {
        let mut p = prefix.to_vec();
        p.push(name);
        p
    };

    for sub in cmd.get_subcommands() {
        generate_manpages(sub, out_dir, &child_prefix)?;
    }

    Ok(())
}

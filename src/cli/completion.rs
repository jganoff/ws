use std::io::Write;

use anyhow::{Result, bail};
use clap::ArgMatches;
use clap_complete::generate;
use clap_complete::shells::Zsh;

use super::build_cli;

pub fn run(matches: &ArgMatches) -> Result<()> {
    let shell = matches.get_one::<String>("shell").unwrap();
    match shell.as_str() {
        "zsh" => generate_zsh(&mut std::io::stdout()),
        _ => bail!("unsupported shell: {} (supported: zsh)", shell),
    }
}

fn generate_zsh(w: &mut dyn Write) -> Result<()> {
    let bin = std::env::current_exe()
        .map_err(|e| anyhow::anyhow!("cannot determine executable path: {}", e))?;
    let bin_str = bin.display().to_string();

    let cases = build_cases();

    // Write wrapper function
    write!(
        w,
        "# ws shell integration \u{2014} source with: eval \"$(ws completion zsh)\"\n\
         \n\
         ws() {{\n\
         \x20 local ws_bin={}\n\
         \n\
         \x20 case \"$1\" in\n",
        bin_str
    )?;

    for case in &cases {
        write!(
            w,
            "    {})\n\
             \x20     {}\n\
             \x20     ;;\n",
            case.pattern, case.body
        )?;
    }

    write!(
        w,
        "    *)\n\
         \x20     command \"$ws_bin\" \"$@\"\n\
         \x20     ;;\n\
         \x20 esac\n\
         }}\n\
         \n"
    )?;

    // Generate clap completions
    let mut app = build_cli();
    generate(Zsh, &mut app, "ws", w);

    Ok(())
}

struct ZshCase {
    pattern: String,
    body: String,
}

fn build_cases() -> Vec<ZshCase> {
    vec![
        ZshCase {
            pattern: "new".to_string(),
            body: build_cd_into_body("new"),
        },
        ZshCase {
            pattern: "remove".to_string(),
            body: build_cd_out_body("remove"),
        },
        ZshCase {
            pattern: "rm".to_string(),
            body: build_cd_out_body("remove"),
        },
    ]
}

fn build_cd_into_body(cmd_name: &str) -> String {
    format!(
        "shift\n\
         \x20     command \"$ws_bin\" {} \"$@\" || return\n\
         \x20     local ws_dir=\"$HOME/dev/workspaces/$1\"\n\
         \x20     cd \"$ws_dir\"",
        cmd_name
    )
}

fn build_cd_out_body(cmd_name: &str) -> String {
    format!(
        "shift\n\
         \x20     if [[ -n \"$1\" ]]; then\n\
         \x20       local ws_dir=\"$HOME/dev/workspaces/$1\"\n\
         \x20       if [[ \"$PWD\" = \"$ws_dir\"* ]]; then\n\
         \x20         cd \"$HOME/dev/workspaces\" || cd \"$HOME\"\n\
         \x20       fi\n\
         \x20       command \"$ws_bin\" {} \"$@\"\n\
         \x20     else\n\
         \x20       command \"$ws_bin\" {} \"$@\" || return\n\
         \x20       if [[ ! -d \"$PWD\" ]]; then\n\
         \x20         cd \"$HOME/dev/workspaces\" || cd \"$HOME\"\n\
         \x20       fi\n\
         \x20     fi",
        cmd_name, cmd_name
    )
}

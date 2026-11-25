//! `ciabatta secrets` — the client side of the remote cache's secrets (see
//! [`crate::remote_cache::secrets`]).
//!
//! A group's values reach a run one of two ways. Here, `ciabatta secrets use
//! <group>` puts them in the terminal's environment, where a run finds them as
//! it finds anything else exported there. On an executor, the job names the
//! group (it's in `CIABATTA_SECRETS` when the job is offloaded) and the
//! executor fetches the values itself — so they never travel in a job.

use std::collections::BTreeMap;
use std::io::{IsTerminal, Read, Write};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::cli::SecretsCommand;
use crate::remote_cache::client::Client;
use crate::remote_cache::secrets::{Group, valid_group, valid_name};

/// The variable naming the group a terminal (or a job) has in use.
pub const GROUP_VAR: &str = "CIABATTA_SECRETS";

#[derive(Deserialize)]
struct Groups {
    groups: Vec<Group>,
}

#[derive(Deserialize)]
struct Values {
    secrets: BTreeMap<String, String>,
}

fn group_path(project: &str, group: &str) -> String {
    format!("/api/projects/{}/secrets/{}", project, group)
}

/// The names of the secrets in `group` — none, when there's no such group.
pub async fn names(client: &Client, project: &str, group: &str) -> Result<Vec<String>> {
    let listed: Groups = client
        .get_json(&format!("/api/projects/{}/secrets", project))
        .await?;
    Ok(listed
        .groups
        .into_iter()
        .find(|g| g.name == group)
        .map(|g| g.secrets.into_iter().map(|s| s.name).collect())
        .unwrap_or_default())
}

pub async fn command(subcommand: SecretsCommand) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let remote = crate::oven::offload::Remote::discover(&cwd)
        .context("Secrets are kept by the workspace's remote cache, and none was found")?;
    let (client, project) = (&remote.client, remote.project.as_str());
    match subcommand {
        SecretsCommand::List { group } => {
            let listed: Groups = client
                .get_json(&format!("/api/projects/{}/secrets", project))
                .await?;
            let groups: Vec<&Group> = listed
                .groups
                .iter()
                .filter(|g| group.as_ref().is_none_or(|want| g.name == *want))
                .collect();
            if groups.is_empty() {
                match group {
                    Some(group) => println!("No secrets group '{group}' on {}.", remote.url),
                    None => println!(
                        "No secrets on {} yet. Add one with `ciabatta secrets set <group> <NAME>`.",
                        remote.url
                    ),
                }
                return Ok(());
            }
            for g in groups {
                println!("{} ({} secret(s))", g.name, g.secrets.len());
                for s in &g.secrets {
                    let by = s
                        .updated_by
                        .as_deref()
                        .map(|b| format!(" by {b}"))
                        .unwrap_or_default();
                    println!("  {:<40} set {}{by}", s.name, s.updated_at);
                }
            }
        }
        SecretsCommand::Set { group, name, value } => {
            check(&group, &name)?;
            let value = match value {
                Some(value) => value,
                None => read_value(&name)?,
            };
            let _: serde_json::Value = client
                .put_json(
                    &format!("{}/{}", group_path(project, &group), name),
                    &serde_json::json!({ "value": value }),
                )
                .await?;
            println!("Set {name} in '{group}'.");
        }
        SecretsCommand::Rm { group, name } => {
            check(&group, &name)?;
            let _: serde_json::Value = client
                .delete_json(&format!("{}/{}", group_path(project, &group), name))
                .await?;
            println!("Removed {name} from '{group}'.");
        }
        SecretsCommand::Use { group, shell } => {
            if !valid_group(&group) {
                bail!("'{group}' can't name a secrets group.");
            }
            let github_env = std::env::var("GITHUB_ENV").ok().filter(|p| !p.is_empty());
            let shell = shell.unwrap_or_else(|| match github_env {
                Some(_) => "github".to_string(),
                None => "sh".to_string(),
            });
            let syntax = Syntax::parse(&shell)?;
            // Printing every value onto a screen is never what was meant: the
            // output is for a shell to run.
            if syntax != Syntax::Github && std::io::stdout().is_terminal() {
                bail!(
                    "This prints the secrets for your shell to run, so it won't print them to \
                     the screen. Run it as:\n\n    {}\n",
                    syntax.usage(&group)
                );
            }
            let values: Values = client.get_json(&group_path(project, &group)).await?;
            let mut vars: Vec<(String, String)> = values.secrets.into_iter().collect();
            vars.push((GROUP_VAR.to_string(), group.clone()));
            match syntax {
                Syntax::Github => {
                    let path = github_env.context(
                        "--shell github writes to $GITHUB_ENV, and it isn't set — this isn't a \
                         GitHub Actions step",
                    )?;
                    let mut out = std::io::stdout().lock();
                    for (name, value) in &vars {
                        if name == GROUP_VAR {
                            continue;
                        }
                        for line in value.lines().filter(|l| !l.trim().is_empty()) {
                            writeln!(out, "::add-mask::{line}")?;
                        }
                    }
                    let mut file = std::fs::OpenOptions::new()
                        .append(true)
                        .create(true)
                        .open(&path)
                        .with_context(|| format!("Failed to open $GITHUB_ENV ({path})"))?;
                    file.write_all(github_env_lines(&vars).as_bytes())?;
                }
                syntax => print!("{}", syntax.render(&vars)),
            }
            eprintln!(
                "Using {} secret(s) from '{group}'.",
                vars.len().saturating_sub(1)
            );
        }
    }
    Ok(())
}

fn check(group: &str, name: &str) -> Result<()> {
    if !valid_group(group) {
        bail!("'{group}' can't name a secrets group: use letters, digits, '-' and '_'.");
    }
    if !valid_name(name) {
        bail!(
            "'{name}' can't name a secret: it becomes an environment variable, so use letters, \
             digits and '_', not starting with a digit."
        );
    }
    Ok(())
}

/// The value, from stdin when it's piped, or asked for without echoing it.
fn read_value(name: &str) -> Result<String> {
    if !std::io::stdin().is_terminal() {
        let mut value = String::new();
        std::io::stdin().read_to_string(&mut value)?;
        // A trailing newline is the pipe's, not the secret's.
        let value = value.strip_suffix('\n').unwrap_or(&value);
        let value = value.strip_suffix('\r').unwrap_or(value);
        return Ok(value.to_string());
    }
    eprint!("Value for {name} (not shown): ");
    std::io::stderr().flush()?;
    let value = read_hidden()?;
    eprintln!();
    if value.is_empty() {
        bail!("No value given, so nothing was set.");
    }
    Ok(value)
}

/// A line from the terminal, with echo off.
fn read_hidden() -> Result<String> {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, read};
    crossterm::terminal::enable_raw_mode()?;
    let mut value = String::new();
    let result = loop {
        match read() {
            Ok(Event::Key(KeyEvent {
                code,
                modifiers,
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            })) => match code {
                KeyCode::Enter => break Ok(()),
                KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                    break Err(anyhow::anyhow!("Cancelled."));
                }
                KeyCode::Backspace => {
                    value.pop();
                }
                KeyCode::Char(c) => value.push(c),
                _ => {}
            },
            Ok(Event::Paste(text)) => value.push_str(&text),
            Ok(_) => {}
            Err(e) => break Err(e.into()),
        }
    };
    crossterm::terminal::disable_raw_mode()?;
    result.map(|()| value)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Syntax {
    Sh,
    Fish,
    PowerShell,
    Github,
}

impl Syntax {
    fn parse(shell: &str) -> Result<Self> {
        Ok(match shell.trim().to_ascii_lowercase().as_str() {
            "sh" | "bash" | "zsh" | "dash" | "ksh" | "posix" => Syntax::Sh,
            "fish" => Syntax::Fish,
            "powershell" | "pwsh" | "ps" => Syntax::PowerShell,
            "github" | "gha" | "github-actions" => Syntax::Github,
            other => bail!("Unknown --shell '{other}' (expected sh, fish, powershell or github)"),
        })
    }

    fn usage(self, group: &str) -> String {
        match self {
            Syntax::Sh => format!("eval \"$(ciabatta secrets use {group})\""),
            Syntax::Fish => format!("ciabatta secrets use {group} --shell fish | source"),
            Syntax::PowerShell => format!(
                "ciabatta secrets use {group} --shell powershell | Out-String | Invoke-Expression"
            ),
            Syntax::Github => format!("ciabatta secrets use {group} --shell github"),
        }
    }

    fn render(self, vars: &[(String, String)]) -> String {
        let mut out = String::new();
        for (name, value) in vars {
            out.push_str(&match self {
                Syntax::Sh => format!("export {name}='{}'\n", value.replace('\'', r"'\''")),
                Syntax::Fish => format!(
                    "set -gx {name} '{}'\n",
                    value.replace('\\', r"\\").replace('\'', r"\'")
                ),
                Syntax::PowerShell => {
                    format!("$env:{name} = '{}'\n", value.replace('\'', "''"))
                }
                Syntax::Github => unreachable!("written to $GITHUB_ENV, not printed"),
            });
        }
        out
    }
}

/// `$GITHUB_ENV` lines: the heredoc form for every value, so one with a
/// newline in it can't end early or set a second variable.
fn github_env_lines(vars: &[(String, String)]) -> String {
    let mut out = String::new();
    for (name, value) in vars {
        let mut delimiter = "CIABATTA_EOF".to_string();
        while value.contains(&delimiter) {
            delimiter.push('_');
        }
        out.push_str(&format!("{name}<<{delimiter}\n{value}\n{delimiter}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> Vec<(String, String)> {
        vec![
            ("PLAIN".to_string(), "abc".to_string()),
            ("QUOTED".to_string(), "it's $HOME".to_string()),
        ]
    }

    #[test]
    fn sh_quoting_keeps_a_value_literal() {
        let out = Syntax::Sh.render(&vars());
        assert_eq!(out, "export PLAIN='abc'\nexport QUOTED='it'\\''s $HOME'\n");
        // And a real shell agrees.
        let printed = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("{out}printf %s \"$QUOTED\""))
            .output();
        if let Ok(printed) = printed {
            assert_eq!(String::from_utf8_lossy(&printed.stdout), "it's $HOME");
        }
    }

    #[test]
    fn powershell_and_fish_quote_their_own_way() {
        assert!(
            Syntax::PowerShell
                .render(&vars())
                .contains("$env:QUOTED = 'it''s $HOME'")
        );
        assert!(
            Syntax::Fish
                .render(&vars())
                .contains(r"set -gx QUOTED 'it\'s $HOME'")
        );
    }

    #[test]
    fn github_env_survives_a_value_holding_the_delimiter() {
        let out = github_env_lines(&[(
            "TRICKY".to_string(),
            "line\nCIABATTA_EOF\nEVIL=1".to_string(),
        )]);
        assert!(out.starts_with("TRICKY<<CIABATTA_EOF_\n"), "{out}");
        assert!(out.ends_with("\nCIABATTA_EOF_\n"), "{out}");
    }
}

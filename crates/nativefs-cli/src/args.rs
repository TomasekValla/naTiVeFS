//! Command line parsing (hand written: the option set is tiny and the AI mode must never be surprised).

use std::path::PathBuf;

use crate::expiry;
use crate::settings::ThemeMode;

#[derive(Debug, PartialEq)]
pub enum Command {
    /// Full-screen interface.
    Tui { theme: Option<ThemeMode> },
    /// One-shot mode for AI agents.
    Agent(AgentArgs),
    Help,
    AiHelp,
    Version,
}

#[derive(Debug, PartialEq)]
pub struct AgentArgs {
    pub paths: Vec<PathBuf>,
    pub expire_min: u32,
    pub json: bool,
    pub confirm: Option<String>,
}

pub const HELP: &str = "\
naTiVeFS-cli - terminal client for TVFS (also: nativefscli, nativecli)

Usage:
  naTiVeFS-cli [--theme dark|light|auto]    full-screen interface (log in, upload, recent uploads, settings)

For AI agents (never opens the interface; needs an existing login):
  naTiVeFS-cli --direct-no-cli-ui --choose-file-directly <path> [<path>...] [--expire <time>] [--json]
  naTiVeFS-cli --direct-no-cli-ui --choose-file-directly <path> ... --confirm <CODE>

  --expire <time>   5m 30m 1h 6h 12h 1d 2d 3d 5d 7d 10d 14d (default 7d)
  --json            machine-readable output
  --ai-help         onboarding text for AI agents (alias: help ai)

  --help            this text
  --version         version
";

fn is_flag(s: &str) -> bool {
    s.starts_with("--")
}

pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Command, String> {
    let argv: Vec<String> = args.into_iter().collect();
    if argv.is_empty() {
        return Ok(Command::Tui { theme: None });
    }
    if argv.len() == 2 && argv[0] == "help" && argv[1] == "ai" {
        return Ok(Command::AiHelp);
    }
    let mut direct = false;
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut expire: Option<String> = None;
    let mut json = false;
    let mut confirm: Option<String> = None;
    let mut theme: Option<ThemeMode> = None;
    let mut choose = false;
    let mut i = 0;
    while i < argv.len() {
        let a = argv[i].clone();
        let (name, inline) = match a.split_once('=') {
            Some((n, v)) if is_flag(n) => (n.to_owned(), Some(v.to_owned())),
            _ => (a.clone(), None),
        };
        // the value of an option: inline (`--x=v`) or the next argument
        let value = |what: &str, i: &mut usize| -> Result<String, String> {
            if let Some(v) = inline.clone() {
                return Ok(v);
            }
            *i += 1;
            argv.get(*i).filter(|v| !is_flag(v)).cloned().ok_or_else(|| format!("{name} needs {what}"))
        };
        match name.as_str() {
            "--help" | "-h" => return Ok(Command::Help),
            "--version" | "-V" => return Ok(Command::Version),
            "--ai-help" => return Ok(Command::AiHelp),
            "--direct-no-cli-ui" => direct = true,
            "--json" => json = true,
            "--choose-file-directly" => {
                choose = true;
                if let Some(v) = inline {
                    paths.push(PathBuf::from(v));
                }
                while let Some(next) = argv.get(i + 1).filter(|v| !is_flag(v)) {
                    paths.push(PathBuf::from(next));
                    i += 1;
                }
            }
            "--expire" => expire = Some(value("a time such as 7d", &mut i)?),
            "--confirm" => confirm = Some(value("the code printed by the first step", &mut i)?),
            "--theme" => {
                let v = value("dark, light or auto", &mut i)?;
                theme = Some(ThemeMode::parse(&v).ok_or_else(|| format!("unknown theme \"{v}\" (dark, light or auto)"))?);
            }
            _ => return Err(format!("unknown argument \"{a}\" (try --help)")),
        }
        i += 1;
    }
    if direct || choose || expire.is_some() || confirm.is_some() || json {
        if !direct {
            return Err("these options need --direct-no-cli-ui (the AI agent mode)".into());
        }
        if paths.is_empty() {
            return Err("--choose-file-directly needs at least one file or folder".into());
        }
        let expire_min = match expire {
            None => tvfs_core::DEFAULT_EXPIRATION_MIN,
            Some(e) => expiry::parse(&e).ok_or_else(|| format!("unknown expiry \"{e}\" (use one of: {})", expiry::labels()))?,
        };
        return Ok(Command::Agent(AgentArgs { paths, expire_min, json, confirm }));
    }
    Ok(Command::Tui { theme })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<Command, String> {
        parse(s.split_whitespace().map(str::to_owned))
    }

    #[test]
    fn no_arguments_is_the_interface() {
        assert_eq!(p("").unwrap(), Command::Tui { theme: None });
        assert_eq!(p("--theme light").unwrap(), Command::Tui { theme: Some(ThemeMode::Light) });
    }

    #[test]
    fn agent_step_one_and_two() {
        let Command::Agent(a) = p("--direct-no-cli-ui --choose-file-directly a.txt b.png --expire 1d --json").unwrap() else { panic!() };
        assert_eq!(a.paths, vec![PathBuf::from("a.txt"), PathBuf::from("b.png")]);
        assert_eq!((a.expire_min, a.json, a.confirm), (1440, true, None));
        let Command::Agent(a) = p("--direct-no-cli-ui --choose-file-directly a.txt --confirm ABCD-EFGH").unwrap() else { panic!() };
        assert_eq!(a.confirm.as_deref(), Some("ABCD-EFGH"));
        assert_eq!(a.expire_min, tvfs_core::DEFAULT_EXPIRATION_MIN);
    }

    #[test]
    fn options_before_the_files_and_equals_form() {
        let Command::Agent(a) = p("--direct-no-cli-ui --expire=14d --choose-file-directly x.zip").unwrap() else { panic!() };
        assert_eq!((a.paths.len(), a.expire_min), (1, 20160));
    }

    #[test]
    fn mistakes_are_errors() {
        assert!(p("--choose-file-directly a.txt").is_err());
        assert!(p("--direct-no-cli-ui").is_err());
        assert!(p("--direct-no-cli-ui --choose-file-directly a --expire 9d").is_err());
        assert!(p("--direct-no-cli-ui --choose-file-directly a --confirm").is_err());
        assert!(p("--bogus").is_err());
        assert!(p("--theme pink").is_err());
    }

    #[test]
    fn help_forms() {
        assert_eq!(p("--help").unwrap(), Command::Help);
        assert_eq!(p("--version").unwrap(), Command::Version);
        assert_eq!(p("--ai-help").unwrap(), Command::AiHelp);
        assert_eq!(p("help ai").unwrap(), Command::AiHelp);
    }
}

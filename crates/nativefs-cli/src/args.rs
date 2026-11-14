//! Command line parsing (hand written: the option set is tiny and the AI mode must never be surprised).

use std::path::PathBuf;

use crate::expiry;
use crate::settings::ThemeMode;

#[derive(Debug, PartialEq)]
pub enum Command {
    /// Full-screen interface; with `paths` it opens straight on the upload options of those files.
    Tui { theme: Option<ThemeMode>, paths: Vec<PathBuf> },
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
    /// Random file names (the same as the interface's "Anonymize file names").
    pub anonymize: bool,
    /// `--users-only` = Some(true), `--public` = Some(false), neither = the shared config decides.
    pub users_only: Option<bool>,
    /// Leave out files TVFS cannot take instead of failing the whole command.
    pub skip_unsupported: bool,
    /// Folder uploads also take dot files / dot folders and secret-looking files.
    pub include_hidden: bool,
    /// Upload the audio files among the given files as voice notes.
    pub voice_note: bool,
    /// Delete this own upload (by file id) instead of uploading.
    pub delete: Option<String>,
    /// Allow `--delete` for an id that is not among the recent uploads.
    pub force_unknown_id: bool,
}

pub const HELP: &str = "\
naTiVeFS-cli - terminal client for TVFS (also: nativefscli, nativecli)

Usage:
  naTiVeFS-cli [--theme dark|light|auto]    full-screen interface (log in, upload, recent uploads, settings); --theme is for this run only
  naTiVeFS-cli <path> [<path>...]           full-screen interface, opened on the upload options of these files/folders

For AI agents (never opens the interface; needs an existing login). Every upload and every delete takes two
steps: the first prints what will happen plus a one-time code, the second repeats the command with --confirm <CODE>.
  naTiVeFS-cli --direct-no-cli-ui --choose-file-directly <path> [<path>...] [options] [--json]
  naTiVeFS-cli --direct-no-cli-ui --choose-file-directly <path> ... [options] --confirm <CODE>
  naTiVeFS-cli --direct-no-cli-ui --delete <file_id> [--force-unknown-id] [--json]    (then again with --confirm <CODE>)

Upload options:
  --expire <time>     5m 30m 1h 6h 12h 1d 2d 3d 5d 7d 10d 14d (default 7d)
  --anonymize         random file names (the extension is kept)
  --users-only        only logged-in TVFS users can open the link (overrides the setting); at most 200 files per share
  --public            public link (overrides the TVFS Users Only setting); without either, the setting decides
  --skip-unsupported  skip files TVFS cannot take (no extension, over 10 GB) instead of failing; they are listed as skipped
  --include-hidden    folders: also take dot files / dot folders and secret-looking files (*.pem, *.key, id_rsa*,
                      .env*, *.p12, *.pfx, credentials*, ...), which are left out and listed as skipped by default
  --voice-note        upload the audio files as voice notes (public uploads only; needs at least one audio file)
  --json              machine-readable output (includes the file_id of every upload); errors are JSON on stdout too
  --ai-help           onboarding text for AI agents (alias: help ai)
  --                  everything after it is a file or folder path (for names that start with a dash)

--delete: --force-unknown-id allows an id that is not among the recent uploads (otherwise it is refused).
The confirmation code is bound to the exact files (paths, sizes, modification times), the options and the logged-in
account. stdin is not supported: write the content to a temporary file first.

  --help              this text
  --version           version
";

fn is_flag(s: &str) -> bool {
    s.starts_with("--")
}

/// Store a single-value option; giving it twice is an error (the second would silently win otherwise).
fn once(slot: &mut Option<String>, name: &str, v: String) -> Result<(), String> {
    if slot.replace(v).is_some() {
        return Err(format!("{name} was given more than once"));
    }
    Ok(())
}

pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Command, String> {
    let argv: Vec<String> = args.into_iter().collect();
    if argv.is_empty() {
        return Ok(Command::Tui { theme: None, paths: Vec::new() });
    }
    if argv.len() == 2 && argv[0] == "help" && argv[1] == "ai" {
        return Ok(Command::AiHelp);
    }
    if argv.len() == 1 && argv[0] == "help" {
        return Ok(Command::Help);
    }
    let mut direct = false;
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut expire: Option<String> = None;
    let mut json = false;
    let mut confirm: Option<String> = None;
    let mut theme_raw: Option<String> = None;
    let mut choose = false;
    let mut positional: Vec<PathBuf> = Vec::new();
    let (mut anonymize, mut users_only, mut public, mut skip_unsupported) = (false, false, false, false);
    let (mut include_hidden, mut voice_note, mut force_unknown_id) = (false, false, false);
    let mut delete: Option<String> = None;
    let mut i = 0;
    while i < argv.len() {
        let a = argv[i].clone();
        if a == "--" {
            // everything after the terminator is a path
            let rest = argv[i + 1..].iter().map(PathBuf::from);
            if choose {
                paths.extend(rest);
            } else {
                positional.extend(rest);
            }
            break;
        }
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
            "--anonymize" => anonymize = true,
            "--users-only" => users_only = true,
            "--public" => public = true,
            "--skip-unsupported" => skip_unsupported = true,
            "--include-hidden" => include_hidden = true,
            "--voice-note" => voice_note = true,
            "--force-unknown-id" => force_unknown_id = true,
            "--delete" => {
                let v = value("a file id", &mut i)?;
                once(&mut delete, "--delete", v)?;
            }
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
            "--expire" => {
                let v = value("a time such as 7d", &mut i)?;
                once(&mut expire, "--expire", v)?;
            }
            "--confirm" => {
                let v = value("the code printed by the first step", &mut i)?;
                once(&mut confirm, "--confirm", v)?;
            }
            "--theme" => {
                let v = value("dark, light or auto", &mut i)?;
                once(&mut theme_raw, "--theme", v)?;
            }
            // a plain path (no dash): opens the interface on the upload options
            _ if !a.starts_with('-') => positional.push(PathBuf::from(&a)),
            _ => return Err(format!("unknown argument \"{a}\" (try --help; a path that starts with a dash goes after --)")),
        }
        i += 1;
    }
    let theme = match &theme_raw {
        Some(v) => Some(ThemeMode::parse(v).ok_or_else(|| format!("unknown theme \"{v}\" (dark, light or auto)"))?),
        None => None,
    };
    if users_only && public {
        return Err("--users-only and --public exclude each other".into());
    }
    let upload_only = anonymize || users_only || public || skip_unsupported || include_hidden || voice_note || expire.is_some();
    if direct || choose || upload_only || confirm.is_some() || json || delete.is_some() || force_unknown_id {
        if !direct {
            return Err("these options need --direct-no-cli-ui (the AI agent mode)".into());
        }
        if theme.is_some() {
            return Err("--theme only applies to the full-screen interface, not to --direct-no-cli-ui".into());
        }
        if let Some(first) = positional.first() {
            return Err(format!("unexpected argument \"{}\": in the agent mode, list files after --choose-file-directly", first.display()));
        }
        if let Some(id) = delete {
            if choose {
                return Err("--delete cannot be combined with --choose-file-directly".into());
            }
            if upload_only {
                return Err("--delete cannot be combined with the upload options (--expire, --anonymize, --users-only, --public, --skip-unsupported, --include-hidden, --voice-note)".into());
            }
            let id = id.trim().to_owned();
            if id.is_empty() {
                return Err("--delete needs a file id".into());
            }
            return Ok(Command::Agent(AgentArgs {
                paths,
                expire_min: tvfs_core::DEFAULT_EXPIRATION_MIN,
                json,
                confirm,
                anonymize: false,
                users_only: None,
                skip_unsupported: false,
                include_hidden: false,
                voice_note: false,
                delete: Some(id),
                force_unknown_id,
            }));
        }
        if force_unknown_id {
            return Err("--force-unknown-id only goes with --delete".into());
        }
        if paths.is_empty() {
            return Err("--choose-file-directly needs at least one file or folder".into());
        }
        if voice_note && users_only {
            return Err("--voice-note is for public uploads only (it cannot be combined with --users-only)".into());
        }
        let expire_min = match expire {
            None => tvfs_core::DEFAULT_EXPIRATION_MIN,
            Some(e) => expiry::parse(&e).ok_or_else(|| format!("unknown expiry \"{e}\" (use one of: {})", expiry::labels()))?,
        };
        let users_only = if users_only {
            Some(true)
        } else if public {
            Some(false)
        } else {
            None
        };
        return Ok(Command::Agent(AgentArgs {
            paths,
            expire_min,
            json,
            confirm,
            anonymize,
            users_only,
            skip_unsupported,
            include_hidden,
            voice_note,
            delete: None,
            force_unknown_id: false,
        }));
    }
    Ok(Command::Tui { theme, paths: positional })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<Command, String> {
        parse(s.split_whitespace().map(str::to_owned))
    }

    fn agent(s: &str) -> AgentArgs {
        let Command::Agent(a) = p(s).unwrap() else { panic!("not agent mode: {s}") };
        a
    }

    #[test]
    fn no_arguments_is_the_interface() {
        assert_eq!(p("").unwrap(), Command::Tui { theme: None, paths: vec![] });
        assert_eq!(p("--theme light").unwrap(), Command::Tui { theme: Some(ThemeMode::Light), paths: vec![] });
    }

    #[test]
    fn agent_step_one_and_two() {
        let a = agent("--direct-no-cli-ui --choose-file-directly a.txt b.png --expire 1d --json");
        assert_eq!(a.paths, vec![PathBuf::from("a.txt"), PathBuf::from("b.png")]);
        assert_eq!((a.expire_min, a.json, a.confirm), (1440, true, None));
        let a = agent("--direct-no-cli-ui --choose-file-directly a.txt --confirm ABCD-EFGH");
        assert_eq!(a.confirm.as_deref(), Some("ABCD-EFGH"));
        assert_eq!(a.expire_min, tvfs_core::DEFAULT_EXPIRATION_MIN);
    }

    #[test]
    fn options_before_the_files_and_equals_form() {
        let a = agent("--direct-no-cli-ui --expire=14d --choose-file-directly x.zip");
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
    fn new_upload_flags() {
        let a = agent("--direct-no-cli-ui --choose-file-directly a.txt --anonymize --users-only --skip-unsupported");
        assert_eq!((a.anonymize, a.users_only, a.skip_unsupported, a.delete), (true, Some(true), true, None));
        let a = agent("--direct-no-cli-ui --public --choose-file-directly a.txt");
        assert_eq!((a.anonymize, a.users_only, a.skip_unsupported), (false, Some(false), false));
        assert_eq!(agent("--direct-no-cli-ui --choose-file-directly a.txt").users_only, None);
    }

    #[test]
    fn hidden_and_voice_note_flags() {
        let a = agent("--direct-no-cli-ui --choose-file-directly dir --include-hidden --voice-note");
        assert_eq!((a.include_hidden, a.voice_note), (true, true));
        let a = agent("--direct-no-cli-ui --choose-file-directly dir");
        assert_eq!((a.include_hidden, a.voice_note), (false, false));
        assert!(p("--direct-no-cli-ui --choose-file-directly a.ogg --voice-note --users-only").is_err());
        assert!(p("--include-hidden").is_err());
        assert!(p("--direct-no-cli-ui --delete x --voice-note").is_err());
    }

    #[test]
    fn users_only_and_public_exclude_each_other() {
        let e = p("--direct-no-cli-ui --choose-file-directly a --users-only --public").unwrap_err();
        assert!(e.contains("exclude each other"), "{e}");
        // the upload flags need the agent mode
        assert!(p("--users-only a.txt").is_err());
        assert!(p("--anonymize").is_err());
        assert!(p("--skip-unsupported").is_err());
    }

    #[test]
    fn delete_forms_and_errors() {
        let a = agent("--direct-no-cli-ui --delete abc123 --json");
        assert_eq!((a.delete.as_deref(), a.json, a.paths.len()), (Some("abc123"), true, 0));
        let a = agent("--direct-no-cli-ui --delete=abc123 --confirm ABCD-EFGH");
        assert_eq!((a.delete.as_deref(), a.confirm.as_deref()), (Some("abc123"), Some("ABCD-EFGH")));
        assert!(agent("--direct-no-cli-ui --delete abc --force-unknown-id").force_unknown_id);
        assert!(p("--direct-no-cli-ui --choose-file-directly a.txt --force-unknown-id").is_err());
        assert!(p("--direct-no-cli-ui --delete").is_err());
        assert!(p("--delete abc").is_err());
        assert!(p("--direct-no-cli-ui --delete abc --choose-file-directly a.txt").is_err());
        assert!(p("--direct-no-cli-ui --choose-file-directly a.txt --delete abc").is_err());
        assert!(p("--direct-no-cli-ui --delete abc --expire 1d").is_err());
        assert!(p("--direct-no-cli-ui --delete abc --anonymize").is_err());
    }

    #[test]
    fn plain_paths_open_the_interface() {
        let Command::Tui { theme, paths } = p("a.txt b.png").unwrap() else { panic!() };
        assert_eq!((theme, paths), (None, vec![PathBuf::from("a.txt"), PathBuf::from("b.png")]));
        let Command::Tui { theme, paths } = p("--theme dark docs").unwrap() else { panic!() };
        assert_eq!((theme, paths.len()), (Some(ThemeMode::Dark), 1));
        // unknown flags still fail, and paths do not mix with the agent mode
        assert!(p("a.txt --bogus").is_err());
        assert!(p("-x a.txt").is_err());
        assert!(p("--direct-no-cli-ui a.txt").is_err());
        assert!(p("a.txt --json").is_err());
    }

    #[test]
    fn double_dash_takes_dash_leading_paths() {
        let a = agent("--direct-no-cli-ui --choose-file-directly a.txt -- -notes.txt --weird.md");
        assert_eq!(a.paths, vec![PathBuf::from("a.txt"), PathBuf::from("-notes.txt"), PathBuf::from("--weird.md")]);
        let Command::Tui { paths, .. } = p("-- -notes.txt").unwrap() else { panic!() };
        assert_eq!(paths, vec![PathBuf::from("-notes.txt")]);
    }

    #[test]
    fn repeated_single_value_options_and_theme_in_agent_mode_are_errors() {
        assert!(p("--direct-no-cli-ui --choose-file-directly a --expire 1d --expire 2d").unwrap_err().contains("more than once"));
        assert!(p("--direct-no-cli-ui --choose-file-directly a --confirm X --confirm Y").is_err());
        assert!(p("--theme dark --theme light").is_err());
        assert!(p("--direct-no-cli-ui --choose-file-directly a --theme dark").is_err());
    }

    #[test]
    fn help_forms() {
        assert_eq!(p("--help").unwrap(), Command::Help);
        assert_eq!(p("--version").unwrap(), Command::Version);
        assert_eq!(p("--ai-help").unwrap(), Command::AiHelp);
        assert_eq!(p("help ai").unwrap(), Command::AiHelp);
        assert_eq!(p("help").unwrap(), Command::Help);
    }

    /// Every flag the parser accepts is described in both help texts (they were maintained by hand and drifted).
    #[test]
    fn every_flag_is_in_both_help_texts() {
        for flag in [
            "--direct-no-cli-ui", "--choose-file-directly", "--expire", "--anonymize", "--users-only", "--public", "--skip-unsupported", "--include-hidden",
            "--voice-note", "--json", "--confirm", "--delete", "--force-unknown-id", "--ai-help", "--theme",
        ] {
            assert!(HELP.contains(flag), "HELP lacks {flag}");
            if flag != "--theme" && flag != "--ai-help" {
                assert!(tvfs_core::ai_help::AI_AGENT_HELP.contains(flag), "AI help lacks {flag}");
            }
        }
    }
}

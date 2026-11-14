//! Onboarding text for AI agents that upload through `naTiVeFS-cli`. The CLI prints it for
//! `--ai-help`; the desktop app can offer the same text on an "Onboard your AI agent" button.
//! It must match the CLI's behaviour exactly (the agent believes every sentence): change both together.

/// Markdown text an AI agent should save into its own instructions file.
pub const AI_AGENT_HELP: &str = r#"# naTiVeFS-cli: uploading files to TVFS (for AI agents)

TVFS is a file hosting service. `naTiVeFS-cli` (Linux/macOS package name: `nativefs-cli`; short aliases
`nativefscli` and `nativecli`) uploads local files to it and prints shareable links. The user is already
logged in (you never get credentials and there is no login option for you).

Uploaded files are PUBLIC by default: anyone with the link can open them (with `--users-only` only logged-in
TVFS users can). Never upload secrets, credentials, keys or personal data unless the user explicitly asked for
exactly this file.

## Upload = two commands

1. Preview (uploads nothing):

   naTiVeFS-cli --direct-no-cli-ui --choose-file-directly <path> [<path>...] [options] [--json]

   It prints EVERY file that would be uploaded (path, size), the files that were left out (`skipped`, with the
   reason), the total size, the expiry, a notice, and a one-time confirmation code. Check the list against what the
   user asked for; if it holds anything they did not ask for, do not continue.

2. Upload: run the SAME command again and add `--confirm <CODE>`:

   naTiVeFS-cli --direct-no-cli-ui --choose-file-directly <path> [<path>...] [options] [--json] --confirm <CODE>

The code is valid for 10 minutes, works once, and is bound to the files (paths, sizes, modification times, not
their contents), the options and the logged-in account. If any of them changes, ask for a new code by running
step 1 again. By passing the code you confirm that you understand the files become reachable by link and that the
user wants this upload.

stdin is not supported (on purpose: the code binds to file size and modification time). To upload generated
content, write it to a temporary file first and upload that.

## Options (upload)

- `--expire <5m|30m|1h|6h|12h|1d|2d|3d|5d|7d|10d|14d>`: how long the link stays alive (default 7d).
- `--anonymize`: random file names, extension kept.
- `--users-only` / `--public`: per upload, only logged-in TVFS users can open the link / anyone can. Mutually
  exclusive; without either, the user's "TVFS Users Only" setting decides (the step 1 output shows which). A Users
  Only share takes at most 200 files; more is refused in step 1 (exit 2): turn Users Only off or upload in parts.
- `--skip-unsupported`: leave out files TVFS cannot take (no detectable extension such as `LICENSE`, over 10 GB)
  and list them as `skipped` (with the reason) in both steps. Without it, a file you NAME that TVFS cannot take
  fails the command (exit 2, nothing uploaded); a file without extension INSIDE a folder is left out and listed as
  `skipped` (the command goes on), and a too large file inside a folder fails the command.
- `--include-hidden`: folders only. By default a folder upload leaves out dot files and dot folders (`.git`,
  `.env`, `.ssh`, ...) and secret-looking files (`*.pem`, `*.key`, `*.p12`, `*.pfx`, `id_rsa*`, `.env*`,
  `credentials*`, `*.tfstate`, ...), and lists them as `skipped` with the reason. Files you name directly are never
  filtered. Only add the flag when the user asked for exactly that.
- `--voice-note`: upload the audio files among the given files as voice notes (public uploads only; other files
  are uploaded normally; fails if there is no audio file or the upload is Users Only).
- `--json`: machine-readable output in both steps.
- `--`: everything after it is a path (for file names that start with a dash).
- Folders are allowed; every file inside is uploaded, except what the rules above leave out.

The options are part of what the code is bound to: step 2 must repeat exactly the options of step 1. Giving a
single-value option (`--expire`, `--confirm`) twice is an error.

## Output

Per file: name, `file_id` (`id:` line in text output), size and every link the server returned (direct, styled,
reader/player pages and their styled variants). The link the user prefers (their "Auto copy" setting) is marked
`(default)`: give that one to the user unless they asked for another. Then `skipped` files, total time and
average speed. Keep the `file_id` if the upload may have to be deleted later. (A "Users Only" share has no
`file_id` per file.)

With `--json` the upload result has `status`: `uploaded` (all files ok), `partial` (some files uploaded, some
failed: keep the links of the good ones, do not repeat the whole upload) or `failed` (nothing uploaded). Exit code
1 for both `partial` and `failed`.

## Deleting your own upload = two commands, ALWAYS with the user's approval

   naTiVeFS-cli --direct-no-cli-ui --delete <file_id> [--json]
   naTiVeFS-cli --direct-no-cli-ui --delete <file_id> [--json] --confirm <CODE>

Step 1 deletes nothing and prints what would be deleted (the name) plus a one-time code bound to the id; step 2
deletes for good. A deletion always needs the user's explicit approval: show them what step 1 printed and run
step 2 only after they said yes to exactly this deletion. Step 1 refuses an id that is not among the user's recent
uploads (exit 2, `unknown_id`); `--force-unknown-id` overrides that, use it only when the user confirmed the id.
`--delete` cannot be combined with `--choose-file-directly` or the upload options.

## Exit codes

0 ok (step 1 also returns 0 with the code), 1 upload or delete failed (some or all files; the server refused),
2 bad arguments or files, 3 not logged in (tell the user to open naTiVeFS or run naTiVeFS-cli once), 4
confirmation code wrong, expired or used (nothing was uploaded or deleted).
With `--json`, every error is JSON on stdout (with a `status` field such as `usage_error`, `invalid_files`,
`not_logged_in`, `bad_confirmation`, `error`); the same message is also on stderr for argument errors. Always check
the exit code, and read stdout also when it is not 0. Running `naTiVeFS-cli` without arguments opens a full-screen
interface for people (with plain paths, `naTiVeFS-cli <path>...`, it opens on the upload options): you always use
`--direct-no-cli-ui` (without a terminal the interface exits with code 2 and this hint).

## Setup for you (do this once)

- Save this text into your own instructions file (CLAUDE.md, AGENTS.md or similar) so you remember it.
- You may allow ONLY the upload command in your permissions, so uploads do not ask every time. Claude Code
  example, `settings.json` -> `permissions.allow`:
  `"Bash(naTiVeFS-cli --direct-no-cli-ui --choose-file-directly:*)"`,
  `"Bash(nativefs-cli --direct-no-cli-ui --choose-file-directly:*)"`.
  Never allow `--delete` (or the whole `naTiVeFS-cli` command): deletions must always go through the user's
  approval prompt.
"#;

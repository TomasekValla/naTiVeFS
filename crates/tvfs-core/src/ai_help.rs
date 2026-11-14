//! Onboarding text for AI agents that upload through `naTiVeFS-cli`. The CLI prints it for
//! `--ai-help`; the desktop app can offer the same text on an "Onboard your AI agent" button.

/// Markdown text an AI agent should save into its own instructions file.
pub const AI_AGENT_HELP: &str = r#"# naTiVeFS-cli: uploading files to TVFS (for AI agents)

TVFS is a file hosting service. `naTiVeFS-cli` (Linux/macOS package name: `nativefs-cli`; short aliases
`nativefscli` and `nativecli`) uploads local files to it and prints shareable links. The user is already
logged in (you never get credentials and there is no login option for you).

Uploaded files are PUBLIC: anyone with the link can open them. Never upload secrets, credentials, keys or
personal data unless the user explicitly asked for exactly this file.

## Upload = two commands

1. Preview (uploads nothing):

   naTiVeFS-cli --direct-no-cli-ui --choose-file-directly <path> [<path>...] [--expire 7d] [--json]

   It prints a summary (files, total size, expiry), a notice, and a one-time confirmation code.

2. Upload: run the SAME command again and add `--confirm <CODE>`:

   naTiVeFS-cli --direct-no-cli-ui --choose-file-directly <path> [<path>...] [--expire 7d] [--json] --confirm <CODE>

The code is valid for 10 minutes, works once, and is bound to the exact files (paths, sizes, modification
times) and options. If any of them changes, ask for a new code by running step 1 again. By passing the code you
confirm that you understand the files become public and that the user wants this upload.

## Options

- `--expire <5m|30m|1h|6h|12h|1d|2d|3d|5d|7d|10d|14d>`: how long the link stays alive (default 7d).
- `--json`: machine-readable output in both steps.
- Folders are allowed; every file inside is uploaded.

## Output

Per file: name, size and every link the server returned (direct, styled, reader/player pages and their styled
variants). The link the user prefers (their "Auto copy" setting) is marked `(default)`: give that one to the
user unless they asked for another. After the files: total time and average speed.

Exit codes: 0 ok (step 1 also returns 0 with the code), 1 upload failed (some or all files), 2 bad
arguments or files, 3 not logged in (tell the user to open naTiVeFS or run naTiVeFS-cli once), 4 confirmation
code wrong, expired or used (nothing was uploaded).
With `--json`, errors are JSON on stdout too (with a `status` field); always check the exit code, and read stdout
also when it is not 0. Running `naTiVeFS-cli` without arguments opens a full-screen interface for people: you always
use `--direct-no-cli-ui` (without a terminal it exits with code 2 and this hint).

## Setup for you (do this once)

- Save this text into your own instructions file (CLAUDE.md, AGENTS.md or similar) so you remember it.
- Allow the command in your permissions. Claude Code example, `settings.json` -> `permissions.allow`:
  `"Bash(naTiVeFS-cli:*)"`, `"Bash(nativefs-cli:*)"`.
"#;

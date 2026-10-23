# CLAUDE.md

Guidance for Claude Code when working in this repository.

## What this is

A desktop client for the [Agent Client Protocol](https://agentclientprotocol.com) (ACP),
written in Rust. It drives coding agents such as Claude Code and Codex through their
official ACP adapters. macOS is the primary target.

Three crates in one Cargo workspace (edition 2024, resolver 3):

| Crate | Role |
| --- | --- |
| `crates/acp-core` | Runs one ACP agent connection on its own thread and exposes it as two channels (`Command` in, `Event` out). Holds several sessions per connection, each as its own task. Front ends never touch the ACP runtime directly. |
| `crates/acp-cli` | Minimal terminal front end over `acp-core`: one session, one prompt per stdin line. Useful for checking core changes without the GUI. |
| `crates/acp-gui` | The real app: a gpui window with a session sidebar beside a chat, built on `gpui-kit` (a pinned gpui snapshot plus gpui-component). |

acp-core and acp-cli are a single file each (`lib.rs`, `main.rs`); keep them so until one
clearly needs splitting. acp-gui is split by what each part keeps or draws:

| File in `crates/acp-gui/src/` | Holds |
| --- | --- |
| `main.rs` | Starting the app: the agents to run, the window, key bindings, actions, the extra icons, profiling. |
| `config.rs` | What is kept on disk between runs: `AppSettings`, the starred models, each agent's last settings, the kept sessions with each one's transcript and last context report, the folders sessions were started in, the Stuff board's notes and the images pasted into them. |
| `debug.rs` | `ACP_GUI_DEBUG`: switches that turn a live reply's effects off one by one, and a log of each transcript frame. |
| `git.rs` | What git says of a session's folder: the branch and the others it can switch to, a linked worktree, what the branch changed since its base, the files not committed; and switching it to another branch. Runs `git` in two steps (the branches first), and never on macOS while `/usr/bin/git` is only the Command Line Tools' installer. |
| `hotkey.rs` | The system-wide shortcut that brings the window to the front: what it can't be, holding it with macOS through Carbon, and what a press does. |
| `launch.rs` | What an app opened from Finder lacks (macOS only): PATH from the user's login shell, so `npx` is found, and the home folder instead of `/` for new sessions. |
| `session.rs` | `Session`: one conversation's transcript rows and settings, and how each `SessionEvent` lands on it. |
| `session/pacing.rs` | The agent's text in a live turn, held back and shown at an even pace, and where it may stop so half-arrived markdown never shows. |
| `session/tool_call.rs` | A tool call as its row keeps it: its content made ready to draw once (a command's lines, a file's numbered lines, a diff), how long it ran, and the agent's permission request on it with its answer. |
| `session/context.rs` | What fills a session's context window: the agent's last count, each turn's tokens added up, and Claude's `/context` report read into categories and their items. |
| `session/pull_requests.rs` | The pull requests a session opened, found in its tool calls' output, and how each stands (state, checks, reviews, files, diff) as the user's own `gh` tells it. |
| `session/kept.rs` | A session's rows as kept on disk: the form each is written in, when they are due (the whole after a replay that changed them, a turn's at its end), the kept rows standing in while the agent loads the session, and its replay taking their place, lined up with them. |
| `session/files.rs` | The PDFs, spreadsheets and tables a session's turns made: its folder watched (FSEvents on macOS) from a prompt until its turn ends, and the full paths its commands name. |
| `sound.rs` | The effort meter's ticks and knock, made as WAV data and played through NSSound while macOS's interface sounds are on (macOS only; silent elsewhere). |
| `chrome.rs` | The window around the content: the macOS title bar row, the traffic lights, the blur, the light or dark theme. |
| `chat_view.rs` | `ChatView`: the agents, the sessions and what happens to them, and the window's layout (`Render`). |
| `chat_view/transcript.rs` | How each row of a transcript is drawn. |
| `chat_view/tool_call.rs` | A tool call's row (icon, title, state, time) and the box it opens to; calls in a row grouped under one header that folds them; the permission card in a call's place, with its keys. |
| `chat_view/scroller.rs` | The transcript's list: following its newest text, the button back to the newest, and how far a finger may push it past either end. |
| `chat_view/sidebar.rs` | The Sessions and Stuff tabs, the session cards, renaming, the delete prompt, picking several cards (⌘-click, ⇧-click, a card's Select) in one list at a time and the bar that archives or deletes them, the Archived list, the draggable edge. |
| `chat_view/stuff.rs` | The Stuff list: the note in front to write into, and the board of notes stuck from it, the same for every session. |
| `chat_view/stuff/attachments.rs` | Images and files pasted into a note: taken from the clipboard, drawn as tiles on the note, its card, the prompt a card is let go on and the row it is sent as, taken off the note or the prompt with their ×. |
| `chat_view/stuff/drag.rs` | A card dragged into the chat: pulled at by its tape until the tape tears in two, then into the prompt, sent from the transcript (into the prompt while the agent replies) or back; the zones it lights and its way there; once in the chat, its place on the board closing and the note gone. |
| `chat_view/info_panel.rs` | The session info panel at the chat's right, under the title row: its button, the draggable edge, and the Context window section with its bar and categories. |
| `chat_view/info_panel/pull_requests.rs` | The panel's Pull requests section, when it asks `gh`, and a file's diff in the panel's place. |
| `chat_view/info_panel/files.rs` | The panel's Files section, and starting and stopping each turn's watch of the session's folder. |
| `chat_view/settings_page.rs` | The settings, which take the window while open. |
| `chat_view/folder_line.rs` | The line under the prompt box: the session's folder, a button to the folder menu until its first prompt, the system's folder panel the menu's last row opens (AppKit's, opened at that folder, on macOS), the recent folders the menu lists, what git says of the folder (the branch a button to the branch menu while the agent isn't replying), switching branches, and when git is asked again. |
| `chat_view/usage_line.rs` | The plan usage line below the folder line: which of the agent's limits show, how each reads, and the clock that redraws it when one starts over. |
| `chat_view/pickers.rs` | What the buttons below the prompt and their menus share: `MenuLook`, sizes, `picker_button`. |
| `chat_view/pickers/` | One file each for the model panel, the effort meter with its slider, the mode menu, and the folder and branch menus under the prompt box; `effort/wheel.rs` is how the meter reads a scroll (wheel notches, trackpad pixels, momentum, scrolls begun elsewhere). |

`ChatView`'s fields stay private: its parts sit under `chat_view/`, and a child module can
read its parent's private fields. What another module needs is `pub(crate)`, and no more.
A new screen or menu goes in a file of its own beside the others.

## Running and checking

```bash
cargo run -p acp-gui                # Claude and Codex, picked between from the model menu
cargo run -p acp-gui -- claude     # or: codex, or "<any shell-style agent command>"
cargo run -p acp-cli -- claude
cargo run --release -p acp-gui --features profile   # see "Profiling" below
cargo build --profile dist -p acp-gui               # what a release ships
cargo test --workspace
cargo clippy --workspace --all-targets
cargo fmt --all
```

- `claude` and `codex` resolve to pinned `npx` adapter commands in `acp-core::agent_for`.
  Bump those pins deliberately; they tie the adapter to the agent CLI it was tested with.
  For now they point at commits on Lemon2ee's adapter forks, which add the rate limits push
  ahead of upstream; go back to the npm releases once upstream ships it.
- Build output goes to `target.nosync/` (see `.cargo/config.toml`): `~/Documents` syncs to
  iCloud, which skips `.nosync` names. Do not change the target dir.
- Tests are few, and only of plain functions, which need no window or agent: acp-gui's
  pacing, tool-call content, effort meter, usage line, git output, folder line, sidebar
  selection, Stuff rules and kept transcripts, and acp-core's making of a prompt's content from its files. "Verified" means
  `cargo build`, `cargo test`, `cargo clippy` and `cargo fmt` are clean, plus a run of the
  affected front end when the change is user-visible.
- `dist` (workspace `Cargo.toml`) is `release` with fat LTO and one codegen unit: 20.6 MB
  instead of 30.8 MB on macOS arm64, but a clean build takes minutes and no rebuild is
  incremental. Keep it for builds that get shipped; its output is `target.nosync/dist/`.
- `profile` is off by default, so clippy also needs a run with `-p acp-gui --features profile`
  when a change touches it.
- Do not start `ACPClient.app` or run `.claude/hooks/run-latest-app.sh` unless the user asks.
  Several sessions launching their own builds kept replacing each other's running app.
  If a check needs the running app, say what could not be checked instead.

## Profiling

acp-gui's `profile` feature turns on three things. Run it with `--release`; debug frame times
mean little.

- Cmd+Option+F (also in the app menu) cycles gpui's frame-time overlay: hidden, the last
  draw time, then the slowest 1% and 10%, the worst, and a frame count.
- acp-core's request timings print to stderr: `initialize` (from starting the agent
  process), `session/new`, `session/load`, `session/set_config_option`, `session/close`,
  `session/delete`, and `session/prompt` with its first update, update count and time
  after a stop.
  `session/request_permission` is the time the user took, which `session/prompt` includes.
- Tracy sees all of it plus gpui's draw, present and input zones and a frame mark per
  present. The viewer must be Tracy 0.14, the version the locked tracy-client-sys 0.29 builds,
  and it only records while connected (`ondemand`, localhost only).

To find which of a live reply's effects makes it look wrong, `ACP_GUI_DEBUG` turns them
off one at a time and logs each transcript frame to stderr, in any build, e.g.
`ACP_GUI_DEBUG=no-pace,log cargo run --release -p acp-gui 2> frames.log`. The words are
`no-pace`, `no-fade`, `no-enter`, `no-thinking`, `no-follow` and `log` (see `debug.rs`).

In debug builds, gpui-component already binds Cmd+Option+I to gpui's element inspector.

## Releasing

Pushing a `v*` tag runs `.github/workflows/release.yml`: it builds the `dist` profile on an
Apple Silicon runner, wraps it in `ACPClient.app` and attaches
`ACPClient-<version>-macos-arm64.zip` to a GitHub release of that tag. The version comes from
the tag, not from `Cargo.toml`, and becomes the bundle's version, which macOS wants as plain
numbers (`v0.2.0`, not `v0.2.0-beta`). The app is signed ad hoc, not notarized, so a
downloaded copy needs allowing once in System Settings > Privacy & Security.

## Architecture rules

- **Core owns the protocol; front ends own display.** Anything that talks to
  `agent_client_protocol` lives in `acp-core`. The CLI and GUI see only `Command`,
  `Prompt`, `Event`, `SessionEvent`, `Setting`, `PermissionRequest`, `ToolStatus`,
  `RateLimitWindow` and `Harness` (the agents the client can start by name).
- **One connection, many sessions.** `SessionKey` is chosen by the front end so it can show
  a session before the agent has created it. Each session reads its commands from one
  queue in order, so a cancel never overtakes the prompt it is meant to stop.
- **Never take down the connection for one session.** Send prompts by hand (see
  `run_turn`) rather than through helpers whose failure ends every session.
- **Timings go to `tracing`, not `Event`.** acp-core sends how long each request took as
  info events with `_ms` fields; nothing is recorded unless a front end subscribes.
- **Credentials are never handled here.** `auth_required` surfaces as a message that points
  at the agent's own login command.
- **New `SessionEvent` variants must be handled in both front ends.** The CLI may ignore
  one explicitly, with a comment saying why.
- **GUI state is per session.** Drafts, transcripts, settings and permission cards live on
  `Session`, not on `ChatView`, so switching sessions loses nothing.
- **macOS specifics stay behind `OWN_TITLE_BAR` / `cfg(target_os = "macos")`.** The app
  draws its own title bar row, moves the window itself, and reads the traffic lights'
  size from AppKit. Keep non-macOS builds compiling.

## Code style

- Rust 2024 defaults; `cargo fmt` is the formatter and clippy runs on save in Zed.
- Doc comments and inline comments explain *why* and what the reader could not see from
  the code: a pinned version, an AppKit quirk, a macOS version difference. Keep them in
  the existing plain, concrete voice; no restating the code.
- Prefer small named constants for layout numbers (`SIDEBAR_WIDTH`, `MENU_ROW_HEIGHT`)
  over literals inside render code.
- When adding a `gpui-kit` component, check whether the library already ships the icon
  or behaviour before adding assets or custom code; `AppAssets` is only for what it lacks.

## Git conventions

- Commit subject: `<crate>: <what it does now, in plain imperative words>`, lower-case,
  no trailing period, e.g. `acp-gui: keep the caret at the end of a restored draft`.
  A change outside a single crate (several crates, or workspace files such as
  `Cargo.toml`, `.zed/`, `CLAUDE.md`) drops the prefix and starts with a capitalised
  verb, e.g. `Add Zed and cargo project config`.
- Commit body: one or two sentences on the observable behaviour and the reason, not a
  list of edits. Mention the numbers that matter (sizes, colours, versions).
- Branches: `feat/...`, `fix/...`; work lands on `main` through a PR.
- Commit or push only when asked.

## Pull requests

- **Title** follows the commit subject rule: `<crate>: <what the PR does, in plain words>`,
  lower-case after the prefix, no trailing period, e.g.
  `acp-gui: follow the system's light or dark appearance`. A PR outside a single crate
  drops the prefix and starts with a capitalised verb, as commits do (`Fix the draft
  caret and stale permission cards`, `Add a CLAUDE.md for Claude Code`). No ticket
  numbers or `feat:`/`fix:` tags in the title.
- **Body**: what the user sees differently, why, and how it was checked. Short.
- **Turn on Auto-fix for every PR you open.** Right after `gh pr create`, bind the PR in
  the desktop app (`get_status`, then `bind_pr` if it is not reported) and enable
  Auto-fix with `set_monitor`. The app then wakes this session on CI failures, merge
  conflicts and review comments; never poll CI yourself.
- **Every PR is reviewed by the `@sourcery-ai` bot.** Expect its review a few minutes
  after the PR opens. Treat each comment as a suggestion to weigh, not an order: fix
  what is right, reply briefly on the thread where it is wrong or out of scope, and
  resolve the thread either way. Do not merge while a sourcery thread is unanswered.

## Editor

`.zed/` holds tasks (run each front end with `claude` or `codex`, clippy, fmt), debug
configs, and rust-analyzer settings that keep its check build out of cargo's lock.
Mirror any new routine command there.

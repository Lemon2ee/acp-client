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

Each crate is a single file today: `lib.rs` for core, `main.rs` for the two front ends.
Keep it that way until a file clearly needs splitting.

## Running and checking

```bash
cargo run -p acp-gui                # Claude and Codex, picked between from the model menu
cargo run -p acp-gui -- claude     # or: codex, or "<any shell-style agent command>"
cargo run -p acp-cli -- claude
cargo run --release -p acp-gui --features profile   # see "Profiling" below
cargo clippy --workspace --all-targets
cargo fmt --all
```

- `claude` and `codex` resolve to pinned `npx` adapter commands in `acp-core::agent_for`.
  Bump those pins deliberately; they tie the adapter to the agent CLI it was tested with.
- Build output goes to `target.nosync/` (see `.cargo/config.toml`): `~/Documents` syncs to
  iCloud, which skips `.nosync` names. Do not change the target dir.
- There are no tests yet. "Verified" means `cargo build`, `cargo clippy` and `cargo fmt`
  are clean, plus a run of the affected front end when the change is user-visible.
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
  process), `session/new`, `session/set_config_option`, and `session/prompt` with its first
  update, update count and time after a stop. `session/request_permission` is the time the
  user took, which `session/prompt` includes.
- Tracy sees all of it plus gpui's draw, present and input zones and a frame mark per
  present. The viewer must be Tracy 0.14, the version the locked tracy-client-sys 0.29 builds,
  and it only records while connected (`ondemand`, localhost only).

In debug builds, gpui-component already binds Cmd+Option+I to gpui's element inspector.

## Architecture rules

- **Core owns the protocol; front ends own display.** Anything that talks to
  `agent_client_protocol` lives in `acp-core`. The CLI and GUI see only `Command`,
  `Event`, `SessionEvent`, `Setting`, `PermissionRequest` and `Harness` (the agents
  the client can start by name).
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

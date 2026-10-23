//! Minimal ACP client: `acp-cli [claude | codex | "<agent command>"]`, one prompt per stdin line.

use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::process::ExitCode;
use std::time::{Duration, SystemTime};

use acp_core::{
    Command, ContextUsage, Event, PermissionRequest, RateLimitWindow, Receiver, SessionEvent,
    SessionKey,
};

/// The CLI talks to a single session.
const SESSION: SessionKey = SessionKey(0);

fn main() -> ExitCode {
    let name = std::env::args().nth(1).unwrap_or_else(|| "claude".into());
    let agent = match acp_core::agent_for(&name) {
        Ok(agent) => agent,
        Err(err) => {
            eprintln!("bad agent command: {err}");
            return ExitCode::from(2);
        }
    };
    let cwd = std::env::current_dir().expect("no current directory");
    eprintln!("starting {name} in {} ...", cwd.display());
    let handle = acp_core::spawn(agent, cwd);
    let _ = handle.commands.send_blocking(Command::NewSession(SESSION));
    let mut lines = io::stdin().lock().lines();

    let mut alive = pump(&handle.events, &mut lines);
    while alive {
        print!("\n> ");
        io::stdout().flush().ok();
        let Some(Ok(line)) = lines.next() else { break };
        if line.trim().is_empty() {
            continue;
        }
        if handle
            .commands
            .send_blocking(Command::Prompt(SESSION, line))
            .is_err()
        {
            break;
        }
        alive = pump(&handle.events, &mut lines);
    }

    // Closing the command channel ends the session; wait so the agent process is torn down.
    drop(handle.commands);
    let mut failed = !alive;
    while let Ok(event) = handle.events.recv_blocking() {
        match event {
            Event::Error(message) => {
                eprintln!("error: {message}");
                failed = true;
            }
            Event::Closed => break,
            _ => {}
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Prints events until the agent is ready for the next prompt. `false` means it is gone.
fn pump(events: &Receiver<Event>, lines: &mut impl Iterator<Item = io::Result<String>>) -> bool {
    let mut tools = HashMap::new();
    // Claude's adapter counts again with each piece of a reply, so only the last count
    // shows, as the turn ends.
    let mut context = None;
    let mut took = None;
    while let Ok(event) = events.recv_blocking() {
        match event {
            Event::Connected { agent } => println!("connected: {agent}"),
            Event::SessionReady(_) => return true,
            Event::SessionFailed(_, message) => {
                eprintln!("error: {message}");
                return false;
            }
            Event::Session(_, update) => match update {
                SessionEvent::AgentText(text) => print!("{text}"),
                SessionEvent::AgentThought(text) => print!("\x1b[2m{text}\x1b[0m"),
                SessionEvent::ToolCall {
                    id, title, status, ..
                } => {
                    println!("\n[tool] {title} ({status})");
                    tools.insert(id, (title, Some(status)));
                }
                SessionEvent::ToolCallUpdate {
                    id, title, status, ..
                } => {
                    let tool = tools
                        .entry(id)
                        .or_insert_with(|| ("tool".to_string(), None));
                    if let Some(title) = title {
                        tool.0 = title;
                    }
                    // A permission request repeats the call as it stands, so only a change
                    // of status is printed.
                    if let Some(status) = status.filter(|status| tool.1 != Some(*status)) {
                        tool.1 = Some(status);
                        println!("[tool] {} ({status})", tool.0);
                    }
                }
                SessionEvent::Plan(entries) => {
                    println!("\n[plan]");
                    for entry in entries {
                        println!("  {entry}");
                    }
                }
                SessionEvent::Permission(request) => ask(request, lines),
                SessionEvent::ContextUsage(usage) => context = Some(usage),
                SessionEvent::TurnUsage(usage) => took = Some(usage),
                SessionEvent::TurnEnded { stop_reason } => {
                    println!("\n[turn ended: {stop_reason}]");
                    if let Some(usage) = took {
                        let (input, cached, output) =
                            (usage.input, usage.cached_read, usage.output);
                        println!("[tokens: {input} in, {cached} cached, {output} out]");
                    }
                    if let Some(ContextUsage { used, size }) = context {
                        println!("[context: {used} of {size} tokens]");
                    }
                    return true;
                }
                SessionEvent::TurnFailed(message) => {
                    eprintln!("\nerror: {message}");
                    return true;
                }
                SessionEvent::ModeChanged(mode) => println!("\n[mode: {mode}]"),
                // Comes a few seconds after a turn, so it shows as the next one starts.
                SessionEvent::Title(title) => println!("\n[title: {title}]"),
                // The CLI keeps no sessions between runs, so it never loads one, and
                // the agent replays no messages to it.
                SessionEvent::Created(_) | SessionEvent::UserText(_) => {}
                // The CLI never changes the session's settings, so it doesn't list them.
                SessionEvent::Settings(_)
                | SessionEvent::SettingChanged { .. }
                | SessionEvent::SettingFailed { .. } => {}
                // The CLI never asks what fills the context window.
                SessionEvent::ContextReport(_) | SessionEvent::ContextReadable(_) => {}
            },
            Event::RateLimits(windows) => print_rate_limits(&windows),
            Event::Error(message) => eprintln!("\nerror: {message}"),
            Event::Closed => return false,
        }
        io::stdout().flush().ok();
    }
    false
}

/// The agent sends every window again each time one changes.
fn print_rate_limits(windows: &[RateLimitWindow]) {
    if windows.is_empty() {
        println!("\n[plan limits: none for this login]");
        return;
    }
    println!("\n[plan limits]");
    for window in windows {
        let resets = window
            .resets_at
            .and_then(|at| at.duration_since(SystemTime::now()).ok())
            .map(|left| format!(", resets in {}", rough_duration(left)))
            .unwrap_or_default();
        println!(
            "  {}: {:.0}% used{resets}",
            window.label, window.used_percent
        );
    }
}

/// `3d 4h`, `2h 05m`, `12m`.
fn rough_duration(duration: Duration) -> String {
    let minutes = duration.as_secs() / 60;
    let (days, hours, minutes) = (minutes / (24 * 60), minutes / 60 % 24, minutes % 60);
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else {
        format!("{minutes}m")
    }
}

fn ask(mut request: PermissionRequest, lines: &mut impl Iterator<Item = io::Result<String>>) {
    println!("\n[permission] {}", request.title);
    for (index, option) in request.options.iter().enumerate() {
        println!("  {}. {}", index + 1, option.name);
    }
    print!("choose a number (anything else rejects): ");
    io::stdout().flush().ok();
    let choice = lines
        .next()
        .and_then(Result::ok)
        .and_then(|line| line.trim().parse::<usize>().ok())
        .and_then(|number| request.options.get(number.checked_sub(1)?))
        .map(|option| option.id.clone());
    request.answer(choice);
}

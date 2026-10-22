//! Minimal ACP client: `acp-cli [claude | codex | "<agent command>"]`, one prompt per stdin line.

use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::process::ExitCode;

use acp_core::{Command, Event, PermissionRequest, Receiver, SessionEvent, SessionKey};

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
    let mut tool_titles = HashMap::new();
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
                SessionEvent::ToolCall { id, title, status } => {
                    println!("\n[tool] {title} ({status})");
                    tool_titles.insert(id, title);
                }
                SessionEvent::ToolCallUpdate { id, title, status } => {
                    if let Some(title) = title {
                        tool_titles.insert(id.clone(), title);
                    }
                    if let Some(status) = status {
                        let title = tool_titles.get(&id).map_or("tool", String::as_str);
                        println!("[tool] {title} ({status})");
                    }
                }
                SessionEvent::Plan(entries) => {
                    println!("\n[plan]");
                    for entry in entries {
                        println!("  {entry}");
                    }
                }
                SessionEvent::Permission(request) => ask(request, lines),
                SessionEvent::TurnEnded { stop_reason } => {
                    println!("\n[turn ended: {stop_reason}]");
                    return true;
                }
                SessionEvent::TurnFailed(message) => {
                    eprintln!("\nerror: {message}");
                    return true;
                }
                SessionEvent::ModeChanged(mode) => println!("\n[mode: {mode}]"),
                // The CLI never changes the session's settings, so it doesn't list them.
                SessionEvent::Settings(_)
                | SessionEvent::SettingChanged { .. }
                | SessionEvent::SettingFailed { .. } => {}
            },
            Event::Error(message) => eprintln!("\nerror: {message}"),
            Event::Closed => return false,
        }
        io::stdout().flush().ok();
    }
    false
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

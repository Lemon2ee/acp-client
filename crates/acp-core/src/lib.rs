//! Runs one ACP agent session on its own thread and exposes it as two channels,
//! so a front end (CLI, gpui window) never touches the ACP runtime directly.

use std::path::{Path, PathBuf};
use std::thread;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    AuthMethod, CancelNotification, ContentBlock, ErrorCode, Implementation, InitializeRequest,
    PermissionOptionKind, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionNotification, SessionUpdate,
};
use agent_client_protocol::util::MatchDispatch;
pub use agent_client_protocol::{AcpAgent, Error};
use agent_client_protocol::{ActiveSession, Agent, Client, ConnectionTo, Dispatch, SessionMessage};
use futures::channel::oneshot;
pub use smol::channel::{Receiver, Sender};

/// What the front end asks the agent to do.
pub enum Command {
    Prompt(String),
    /// Stop the turn in flight; ignored when idle.
    Cancel,
}

/// What the agent reports, flattened for display.
pub enum Event {
    /// Initialized and a session exists.
    Ready {
        agent: String,
    },
    AgentText(String),
    AgentThought(String),
    ToolCall {
        id: String,
        title: String,
        status: String,
    },
    ToolCallUpdate {
        id: String,
        title: Option<String>,
        status: Option<String>,
    },
    Plan(Vec<String>),
    Permission(PermissionRequest),
    TurnEnded {
        stop_reason: String,
    },
    /// The connection failed (missing login, agent crashed, ...).
    Error(String),
    /// No more events will follow.
    Closed,
}

pub struct PermissionRequest {
    pub title: String,
    pub options: Vec<PermissionChoice>,
    respond: Option<oneshot::Sender<Option<String>>>,
}

impl PermissionRequest {
    /// Answers with an option id, or `None` to reject; only the first answer counts.
    /// Dropping an unanswered request also rejects.
    pub fn answer(&mut self, choice: Option<String>) {
        if let Some(respond) = self.respond.take() {
            let _ = respond.send(choice);
        }
    }
}

pub struct PermissionChoice {
    pub id: String,
    pub name: String,
    pub allow: bool,
}

pub struct AgentHandle {
    pub commands: Sender<Command>,
    pub events: Receiver<Event>,
}

/// Pinned rather than `@latest`, so each adapter runs the CLI it bundles and was tested with
/// (claude-agent-acp pins Agent SDK 0.3.303 exactly; codex-acp takes @openai/codex 0.160.x).
const CLAUDE_ADAPTER: &str = "npx -y @agentclientprotocol/claude-agent-acp@0.94.0";
const CODEX_ADAPTER: &str = "npx -y @agentclientprotocol/codex-acp@2.2.2";

/// `claude` and `codex` map to the official ACP adapters; anything else is run as a shell-style command.
pub fn agent_for(name: &str) -> Result<AcpAgent, Error> {
    match name {
        "claude" => CLAUDE_ADAPTER,
        "codex" => CODEX_ADAPTER,
        command => command,
    }
    .parse()
}

/// Starts the agent process and its session on a background thread.
pub fn spawn(agent: AcpAgent, cwd: PathBuf) -> AgentHandle {
    let (command_tx, command_rx) = smol::channel::unbounded();
    let (event_tx, event_rx) = smol::channel::unbounded();
    thread::Builder::new()
        .name("acp-agent".into())
        .spawn(move || {
            if let Err(message) = smol::block_on(run(agent, &cwd, &command_rx, &event_tx)) {
                emit(&event_tx, Event::Error(message));
            }
            emit(&event_tx, Event::Closed);
        })
        .expect("failed to spawn the ACP thread");
    AgentHandle {
        commands: command_tx,
        events: event_rx,
    }
}

async fn run(
    agent: AcpAgent,
    cwd: &Path,
    commands: &Receiver<Command>,
    events: &Sender<Event>,
) -> Result<(), String> {
    let mut auth_methods = Vec::new();
    Client
        .builder()
        .name("acp-gui")
        .connect_with(agent, async |cx: ConnectionTo<Agent>| {
            let init = cx
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await?;
            auth_methods = init.auth_methods;
            let mut session = cx.build_session(cwd).block_task().start_session().await?;
            emit(
                events,
                Event::Ready {
                    agent: describe(init.agent_info.as_ref()),
                },
            );

            while let Ok(command) = commands.recv().await {
                if let Command::Prompt(text) = command {
                    session.send_prompt(text)?;
                    if !run_turn(&mut session, commands, events).await? {
                        break;
                    }
                }
            }
            Ok(())
        })
        .await
        .map_err(|err| describe_error(err, &auth_methods))
}

/// Pumps one prompt turn. Returns `false` when the front end has gone away.
async fn run_turn(
    session: &mut ActiveSession<'static, Agent>,
    commands: &Receiver<Command>,
    events: &Sender<Event>,
) -> Result<bool, Error> {
    enum Next {
        Update(Result<SessionMessage, Error>),
        Command(Option<Command>),
    }
    loop {
        // Both arms are cancel-safe: a dropped `recv`/`read_update` loses no message.
        let next = smol::future::or(async { Next::Update(session.read_update().await) }, async {
            Next::Command(commands.recv().await.ok())
        })
        .await;
        match next {
            Next::Update(update) => match update? {
                SessionMessage::StopReason(reason) => {
                    emit(
                        events,
                        Event::TurnEnded {
                            stop_reason: format!("{reason:?}"),
                        },
                    );
                    return Ok(true);
                }
                SessionMessage::SessionMessage(dispatch) => handle(dispatch, events).await?,
                _ => {}
            },
            Next::Command(Some(Command::Cancel)) => session
                .connection()
                .send_notification(CancelNotification::new(session.session_id().clone()))?,
            // Front ends keep the input disabled during a turn; a stray prompt is dropped.
            Next::Command(Some(Command::Prompt(_))) => {}
            Next::Command(None) => return Ok(false),
        }
    }
}

async fn handle(dispatch: Dispatch, events: &Sender<Event>) -> Result<(), Error> {
    MatchDispatch::new(dispatch)
        .if_notification(async |notification: SessionNotification| {
            if let Some(event) = flatten(notification.update) {
                emit(events, event);
            }
            Ok(())
        })
        .await
        .if_request(async |request: RequestPermissionRequest, responder| {
            let outcome = ask_permission(request, events).await;
            responder.respond(RequestPermissionResponse::new(outcome))
        })
        .await
        .otherwise(async |message| match message {
            // We advertise no fs/terminal capabilities, so anything else is unexpected.
            Dispatch::Request(_, responder) => {
                responder.respond_with_error(Error::method_not_found())
            }
            _ => Ok(()),
        })
        .await
}

async fn ask_permission(
    request: RequestPermissionRequest,
    events: &Sender<Event>,
) -> RequestPermissionOutcome {
    let (respond, choice) = oneshot::channel();
    let options = request
        .options
        .iter()
        .map(|option| PermissionChoice {
            id: option.option_id.0.to_string(),
            name: option.name.clone(),
            allow: matches!(
                option.kind,
                PermissionOptionKind::AllowOnce | PermissionOptionKind::AllowAlways
            ),
        })
        .collect();
    let title = request
        .tool_call
        .fields
        .title
        .clone()
        .unwrap_or_else(|| "tool call".into());
    emit(
        events,
        Event::Permission(PermissionRequest {
            title,
            options,
            respond: Some(respond),
        }),
    );

    let chosen = choice.await.ok().flatten();
    match request
        .options
        .into_iter()
        .find(|option| Some(option.option_id.0.as_ref()) == chosen.as_deref())
    {
        Some(option) => {
            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option.option_id))
        }
        None => RequestPermissionOutcome::Cancelled,
    }
}

fn flatten(update: SessionUpdate) -> Option<Event> {
    Some(match update {
        SessionUpdate::AgentMessageChunk(chunk) => Event::AgentText(text_of(&chunk.content)),
        SessionUpdate::AgentThoughtChunk(chunk) => Event::AgentThought(text_of(&chunk.content)),
        SessionUpdate::ToolCall(call) => Event::ToolCall {
            id: call.tool_call_id.0.to_string(),
            title: call.title,
            status: format!("{:?}", call.status),
        },
        SessionUpdate::ToolCallUpdate(update) => Event::ToolCallUpdate {
            id: update.tool_call_id.0.to_string(),
            title: update.fields.title,
            status: update.fields.status.map(|status| format!("{status:?}")),
        },
        SessionUpdate::Plan(plan) => Event::Plan(
            plan.entries
                .into_iter()
                .map(|entry| format!("[{:?}] {}", entry.status, entry.content))
                .collect(),
        ),
        _ => return None,
    })
}

fn text_of(block: &ContentBlock) -> String {
    match block {
        ContentBlock::Text(text) => text.text.clone(),
        ContentBlock::Image(image) => format!("[image: {}]", image.mime_type),
        ContentBlock::ResourceLink(link) => link.uri.clone(),
        _ => "[unsupported content]".into(),
    }
}

fn describe(info: Option<&Implementation>) -> String {
    match info {
        Some(info) => format!(
            "{} {}",
            info.title.as_deref().unwrap_or(&info.name),
            info.version
        ),
        None => "unknown agent".into(),
    }
}

/// A missing or expired login surfaces as `auth_required` (on `session/new` or a prompt).
/// We never handle credentials ourselves, so point at the agent's own login instead.
fn describe_error(err: Error, auth_methods: &[AuthMethod]) -> String {
    if err.code != ErrorCode::AuthRequired {
        return err.to_string();
    }
    let names: Vec<&str> = auth_methods
        .iter()
        .map(|method| match method {
            AuthMethod::Agent(agent) => agent.name.as_str(),
            AuthMethod::Terminal(terminal) => terminal.name.as_str(),
            _ => "unknown",
        })
        .collect();
    format!(
        "not logged in, or the login expired. Log in with the agent's own CLI \
         (`claude` then /login, or `codex login`), then restart. Agent offers: {}",
        names.join(", ")
    )
}

fn emit(events: &Sender<Event>, event: Event) {
    // A closed channel means the front end quit; nothing left to tell.
    let _ = events.send_blocking(event);
}

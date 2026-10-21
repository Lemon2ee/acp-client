//! Runs one ACP agent connection on its own thread and exposes it as two channels,
//! so a front end (CLI, gpui window) never touches the ACP runtime directly.
//! The connection can hold several sessions, each running as its own task.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::thread;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    AuthMethod, CancelNotification, ContentBlock, ErrorCode, Implementation, InitializeRequest,
    PermissionOptionKind, PromptRequest, PromptResponse, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, SelectedPermissionOutcome, SessionId,
    SessionNotification, SessionUpdate, StopReason,
};
use agent_client_protocol::util::{MatchDispatch, internal_error};
pub use agent_client_protocol::{AcpAgent, Error};
use agent_client_protocol::{ActiveSession, Agent, Client, ConnectionTo, Dispatch, SessionMessage};
use futures::channel::oneshot;
pub use smol::channel::{Receiver, Sender};

/// Names a session on the connection. The front end picks it, so it can show the
/// session before the agent has created it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SessionKey(pub u64);

/// What the front end asks the agent to do.
pub enum Command {
    /// Answered with `SessionReady` or `SessionFailed`.
    NewSession(SessionKey),
    Prompt(SessionKey, String),
    /// Stop the session's turn in flight; ignored when idle.
    Cancel(SessionKey),
}

/// What the agent reports, flattened for display.
pub enum Event {
    /// Initialized; sessions can be created.
    Connected {
        agent: String,
    },
    SessionReady(SessionKey),
    SessionFailed(SessionKey, String),
    Session(SessionKey, SessionEvent),
    /// The connection failed (missing login, agent crashed, ...); every session is gone.
    Error(String),
    /// No more events will follow.
    Closed,
}

/// Something that happened inside one session.
pub enum SessionEvent {
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
    /// The prompt failed; the session stays usable.
    TurnFailed(String),
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

/// Starts the agent process on a background thread; create sessions with `Command::NewSession`.
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
    let mut login_options = Vec::new();
    Client
        .builder()
        .name("acp-gui")
        .connect_with(agent, async |cx: ConnectionTo<Agent>| {
            let init = cx
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await?;
            login_options = init.auth_methods.iter().map(login_option).collect();
            emit(
                events,
                Event::Connected {
                    agent: describe(init.agent_info.as_ref()),
                },
            );

            // Sessions run as tasks on this thread, so turns in different sessions overlap.
            let tasks = smol::LocalExecutor::new();
            let mut sessions: HashMap<SessionKey, (SessionId, Sender<String>)> = HashMap::new();
            tasks
                .run(async {
                    while let Some(command) = next_command(commands, &cx).await? {
                        match command {
                            Command::NewSession(key) => {
                                match cx.build_session(cwd).block_task().start_session().await {
                                    Ok(session) => {
                                        let (prompt_tx, prompt_rx) = smol::channel::unbounded();
                                        sessions
                                            .insert(key, (session.session_id().clone(), prompt_tx));
                                        tasks
                                            .spawn(run_session(
                                                key,
                                                session,
                                                prompt_rx,
                                                events.clone(),
                                                login_options.clone(),
                                            ))
                                            .detach();
                                        emit(events, Event::SessionReady(key));
                                    }
                                    Err(err) => emit(
                                        events,
                                        Event::SessionFailed(
                                            key,
                                            describe_error(err, &login_options),
                                        ),
                                    ),
                                }
                            }
                            Command::Prompt(key, text) => {
                                if let Some((_, prompts)) = sessions.get(&key) {
                                    let _ = prompts.try_send(text);
                                }
                            }
                            Command::Cancel(key) => {
                                if let Some((id, _)) = sessions.get(&key) {
                                    cx.send_notification(CancelNotification::new(id.clone()))?;
                                }
                            }
                        }
                    }
                    Ok(())
                })
                .await
        })
        .await
        .map_err(|err| describe_error(err, &login_options))
}

/// The next command, `None` once the front end has gone away, or an error once the agent has.
async fn next_command(
    commands: &Receiver<Command>,
    cx: &ConnectionTo<Agent>,
) -> Result<Option<Command>, Error> {
    enum Next {
        Command(Option<Command>),
        AgentGone,
    }
    let next = smol::future::or(async { Next::Command(commands.recv().await.ok()) }, async {
        cx.incoming_closed().await;
        Next::AgentGone
    })
    .await;
    match next {
        Next::Command(command) => Ok(command),
        Next::AgentGone => Err(internal_error("the agent process exited")),
    }
}

/// Runs the session's prompts one at a time, as they arrive.
async fn run_session(
    key: SessionKey,
    mut session: ActiveSession<'static, Agent>,
    prompts: Receiver<String>,
    events: Sender<Event>,
    login_options: Vec<String>,
) {
    while let Ok(text) = prompts.recv().await {
        let outcome = match run_turn(key, &mut session, text, &events).await {
            Ok(reason) => SessionEvent::TurnEnded {
                stop_reason: format!("{reason:?}"),
            },
            Err(err) => SessionEvent::TurnFailed(describe_error(err, &login_options)),
        };
        emit(&events, Event::Session(key, outcome));
    }
}

/// Sends one prompt and forwards the session's updates until the agent answers it.
/// The prompt is sent by hand instead of with `ActiveSession::send_prompt`, whose
/// failure would end the whole connection along with every other session.
async fn run_turn(
    key: SessionKey,
    session: &mut ActiveSession<'static, Agent>,
    text: String,
    events: &Sender<Event>,
) -> Result<StopReason, Error> {
    enum Next {
        Update(Result<SessionMessage, Error>),
        Answered(Result<Result<PromptResponse, Error>, oneshot::Canceled>),
    }
    let (answer_tx, mut answer) = oneshot::channel();
    session
        .connection()
        .prepare_request(PromptRequest::new(
            session.session_id().clone(),
            vec![text.into()],
        ))
        .on_receiving_result(async move |result| {
            let _ = answer_tx.send(result);
            Ok(())
        })?;
    loop {
        // `or` polls the session first, and updates sent before the answer are already
        // queued when it arrives, so none are left behind.
        let next = smol::future::or(async { Next::Update(session.read_update().await) }, async {
            Next::Answered((&mut answer).await)
        })
        .await;
        match next {
            Next::Update(update) => {
                if let SessionMessage::SessionMessage(dispatch) = update? {
                    handle(dispatch, key, events).await?;
                }
            }
            Next::Answered(answer) => {
                return answer
                    .map_err(|_| internal_error("the prompt was dropped"))?
                    .map(|response| response.stop_reason);
            }
        }
    }
}

async fn handle(dispatch: Dispatch, key: SessionKey, events: &Sender<Event>) -> Result<(), Error> {
    MatchDispatch::new(dispatch)
        .if_notification(async |notification: SessionNotification| {
            if let Some(event) = flatten(notification.update) {
                emit(events, Event::Session(key, event));
            }
            Ok(())
        })
        .await
        .if_request(async |request: RequestPermissionRequest, responder| {
            let outcome = ask_permission(request, key, events).await;
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
    key: SessionKey,
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
        Event::Session(
            key,
            SessionEvent::Permission(PermissionRequest {
                title,
                options,
                respond: Some(respond),
            }),
        ),
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

fn flatten(update: SessionUpdate) -> Option<SessionEvent> {
    Some(match update {
        SessionUpdate::AgentMessageChunk(chunk) => SessionEvent::AgentText(text_of(&chunk.content)),
        SessionUpdate::AgentThoughtChunk(chunk) => {
            SessionEvent::AgentThought(text_of(&chunk.content))
        }
        SessionUpdate::ToolCall(call) => SessionEvent::ToolCall {
            id: call.tool_call_id.0.to_string(),
            title: call.title,
            status: format!("{:?}", call.status),
        },
        SessionUpdate::ToolCallUpdate(update) => SessionEvent::ToolCallUpdate {
            id: update.tool_call_id.0.to_string(),
            title: update.fields.title,
            status: update.fields.status.map(|status| format!("{status:?}")),
        },
        SessionUpdate::Plan(plan) => SessionEvent::Plan(
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

fn login_option(method: &AuthMethod) -> String {
    match method {
        AuthMethod::Agent(agent) => agent.name.clone(),
        AuthMethod::Terminal(terminal) => terminal.name.clone(),
        _ => "unknown".into(),
    }
}

/// A missing or expired login surfaces as `auth_required` (on `session/new` or a prompt).
/// We never handle credentials ourselves, so point at the agent's own login instead.
fn describe_error(err: Error, login_options: &[String]) -> String {
    if err.code != ErrorCode::AuthRequired {
        return err.to_string();
    }
    format!(
        "not logged in, or the login expired. Log in with the agent's own CLI \
         (`claude` then /login, or `codex login`), then try again. Agent offers: {}",
        login_options.join(", ")
    )
}

fn emit(events: &Sender<Event>, event: Event) {
    // A closed channel means the front end quit; nothing left to tell.
    let _ = events.send_blocking(event);
}

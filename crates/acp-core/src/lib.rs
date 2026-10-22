//! Runs one ACP agent connection on its own thread and exposes it as two channels,
//! so a front end (CLI, gpui window) never touches the ACP runtime directly.
//! The connection can hold several sessions, each running as its own task.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::thread;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    AuthMethod, BooleanConfigOptionCapabilities, CancelNotification, ClientCapabilities,
    ClientSessionCapabilities, ContentBlock, ErrorCode, Implementation, InitializeRequest,
    PermissionOption, PermissionOptionKind, PromptRequest, PromptResponse,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionConfigKind, SessionConfigOption, SessionConfigOptionCategory,
    SessionConfigOptionValue, SessionConfigOptionsCapabilities, SessionConfigSelectOptions,
    SessionNotification, SessionUpdate, SetSessionConfigOptionRequest, StopReason,
};
use agent_client_protocol::util::{MatchDispatch, internal_error};
pub use agent_client_protocol::{AcpAgent, Error};
use agent_client_protocol::{
    ActiveSession, Agent, Client, ConnectionTo, Dispatch, Responder, SessionMessage,
};
use futures::channel::oneshot;
pub use smol::channel::{Receiver, Sender};

/// Names a session on the connection. The front end picks it, so it can show the
/// session before the agent has created it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SessionKey(pub u64);

/// What the front end asks the agent to do.
pub enum Command {
    /// Answered with `SessionReady` or `SessionFailed`. A key already in use keeps its
    /// session, and the repeated command is ignored.
    NewSession(SessionKey),
    /// Runs after the session's earlier prompts. If the agent failed to create the
    /// session, it is created again first.
    Prompt(SessionKey, String),
    /// Stop the session's turn in flight; ignored when idle.
    Cancel(SessionKey),
    /// Sets one of the session's settings, by id, during a turn or not. Answered with
    /// the session's `Settings`, or `SettingFailed`.
    ChangeSetting(SessionKey, String, SettingValue),
}

/// What the agent reports, flattened for display.
pub enum Event {
    /// Initialized; sessions can be created.
    Connected {
        agent: String,
    },
    SessionReady(SessionKey),
    /// The agent could not create the session; its next prompt tries again.
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
    /// Every setting the agent offers for the session, as they stand now. Comes once
    /// the session exists, and again whenever they change.
    Settings(Vec<Setting>),
    /// The agent moved the session to another mode, by the id of its choice in the
    /// `Mode` setting.
    ModeChanged(String),
    /// The agent refused a `ChangeSetting`; the setting keeps its value.
    SettingFailed(String),
}

/// Something the agent lets the user choose for one session, such as its model.
#[derive(Clone)]
pub struct Setting {
    pub id: String,
    pub name: String,
    pub kind: SettingKind,
    pub value: SettingValue,
    /// What a `Choice` setting can be set to; none for a `Switch`.
    pub choices: Vec<SettingChoice>,
}

impl Setting {
    /// The choice the setting is set to, when it is one of its choices.
    pub fn chosen(&self) -> Option<&SettingChoice> {
        match &self.value {
            SettingValue::Choice(id) => self.choices.iter().find(|choice| choice.id == *id),
            SettingValue::Switch(_) => None,
        }
    }
}

/// What a setting is for, where the agent says.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SettingKind {
    Model,
    Mode,
    /// How hard the model thinks, such as its reasoning effort.
    ThoughtLevel,
    /// Some other setting of the model, such as a fast mode.
    ModelConfig,
    Other,
}

/// A setting's value.
#[derive(Clone, PartialEq, Eq)]
pub enum SettingValue {
    /// The id of one of the setting's choices.
    Choice(String),
    /// Whether a setting that is on or off is on.
    Switch(bool),
}

#[derive(Clone)]
pub struct SettingChoice {
    pub id: String,
    pub name: String,
    /// What the choice does, when the agent says.
    pub description: Option<String>,
}

pub struct PermissionRequest {
    pub title: String,
    pub options: Vec<PermissionChoice>,
    respond: Option<oneshot::Sender<Option<String>>>,
}

impl PermissionRequest {
    /// Answers with an option id, or `None` to reject; only the first answer counts.
    /// Dropping an unanswered request also rejects. Returns false when the answer is
    /// not taken: it is not the first, or the turn was already stopped or over.
    pub fn answer(&mut self, choice: Option<String>) -> bool {
        self.respond
            .take()
            .is_some_and(|respond| respond.send(choice).is_ok())
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

/// A command for one session. Each session reads these from one queue in order,
/// so a cancel never overtakes the prompt it is meant to stop.
enum SessionCommand {
    Prompt(String),
    Cancel,
    /// A setting's id and its new value.
    ChangeSetting(String, SettingValue),
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
            // Lets agents offer a setting that is on or off, such as a fast mode, as a
            // switch rather than a pick of two values.
            let capabilities = ClientCapabilities::new().session(
                ClientSessionCapabilities::new().config_options(
                    SessionConfigOptionsCapabilities::new()
                        .boolean(BooleanConfigOptionCapabilities::new()),
                ),
            );
            let init = cx
                .send_request(
                    InitializeRequest::new(ProtocolVersion::V1).client_capabilities(capabilities),
                )
                .block_task()
                .await?;
            login_options = init.auth_methods.iter().map(login_option).collect();
            emit(
                events,
                Event::Connected {
                    agent: describe(init.agent_info.as_ref()),
                },
            );

            // Each session runs as a task on this thread: turns in different sessions
            // overlap, and a slow `session/new` never holds up other sessions' commands.
            let tasks = smol::LocalExecutor::new();
            let mut sessions: HashMap<SessionKey, Sender<SessionCommand>> = HashMap::new();
            tasks
                .run(async {
                    while let Some(command) = next_command(commands, &cx).await? {
                        let (key, command) = match command {
                            Command::NewSession(key) => {
                                if let Entry::Vacant(entry) = sessions.entry(key) {
                                    let (command_tx, command_rx) = smol::channel::unbounded();
                                    entry.insert(command_tx);
                                    tasks
                                        .spawn(run_session(
                                            key,
                                            cx.clone(),
                                            cwd.to_path_buf(),
                                            command_rx,
                                            events.clone(),
                                            login_options.clone(),
                                        ))
                                        .detach();
                                }
                                continue;
                            }
                            Command::Prompt(key, text) => (key, SessionCommand::Prompt(text)),
                            Command::Cancel(key) => (key, SessionCommand::Cancel),
                            Command::ChangeSetting(key, setting, value) => {
                                (key, SessionCommand::ChangeSetting(setting, value))
                            }
                        };
                        if let Some(session) = sessions.get(&key) {
                            let _ = session.try_send(command);
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

/// Creates the session, then runs its prompts one at a time, as they arrive.
/// A session the agent failed to create is created again by its next prompt.
async fn run_session(
    key: SessionKey,
    cx: ConnectionTo<Agent>,
    cwd: PathBuf,
    commands: Receiver<SessionCommand>,
    events: Sender<Event>,
    login_options: Vec<String>,
) {
    // The prompt to run once the session exists, when a prompt is creating it again.
    let mut retry = None;
    loop {
        // `run_until` sends `session/new` from this task, so a refusal comes back here;
        // `start_session` sends it from a connection task, where an error ends the
        // whole connection.
        let created = cx
            .build_session(&cwd)
            .block_task()
            .run_until(async |mut session| {
                let settings = settings_of(session.config_options().unwrap_or_default());
                emit(
                    &events,
                    Event::Session(key, SessionEvent::Settings(settings)),
                );
                let waiting: VecDeque<String> = retry.take().into_iter().collect();
                if waiting.is_empty() {
                    emit(&events, Event::SessionReady(key));
                }
                serve(
                    key,
                    &mut session,
                    waiting,
                    &commands,
                    &events,
                    &login_options,
                )
                .await;
                Ok(())
            })
            .await;
        // `serve` returns only once the front end has gone.
        let Err(err) = created else { return };
        let message = describe_error(err, &login_options);
        let failed = match retry {
            None => Event::SessionFailed(key, message),
            Some(_) => Event::Session(key, SessionEvent::TurnFailed(message)),
        };
        emit(&events, failed);
        retry = loop {
            match commands.recv().await {
                Ok(SessionCommand::Prompt(text)) => break Some(text),
                // No session, so nothing is running to stop, and no settings to change.
                Ok(SessionCommand::Cancel | SessionCommand::ChangeSetting(..)) => {}
                Err(_) => return,
            }
        };
    }
}

/// Runs the session's prompts one at a time, as they arrive, until the front end has gone.
/// Prompts sent during a turn wait in `waiting` for their own.
async fn serve(
    key: SessionKey,
    session: &mut ActiveSession<'_, Agent>,
    mut waiting: VecDeque<String>,
    commands: &Receiver<SessionCommand>,
    events: &Sender<Event>,
    login_options: &[String],
) {
    loop {
        let text = match waiting.pop_front() {
            Some(text) => text,
            None => match commands.recv().await {
                Ok(SessionCommand::Prompt(text)) => text,
                // Nothing is running, so there is nothing to stop.
                Ok(SessionCommand::Cancel) => continue,
                Ok(SessionCommand::ChangeSetting(setting, value)) => {
                    // Only a closed connection fails this, which ends the session anyway.
                    let _ = change_setting(key, session, setting, value, events);
                    continue;
                }
                Err(_) => return,
            },
        };
        let outcome = match run_turn(key, session, text, commands, &mut waiting, events).await {
            Ok(reason) => SessionEvent::TurnEnded {
                stop_reason: format!("{reason:?}"),
            },
            Err(err) => SessionEvent::TurnFailed(describe_error(err, login_options)),
        };
        emit(events, Event::Session(key, outcome));
    }
}

/// Sends one prompt and forwards the session's updates until the agent answers it.
/// The prompt is sent by hand instead of with `ActiveSession::send_prompt`, whose
/// failure would end the whole connection along with every other session.
async fn run_turn(
    key: SessionKey,
    session: &mut ActiveSession<'_, Agent>,
    text: String,
    commands: &Receiver<SessionCommand>,
    waiting: &mut VecDeque<String>,
    events: &Sender<Event>,
) -> Result<StopReason, Error> {
    enum Next {
        Update(Result<SessionMessage, Error>),
        Answered(Result<Result<PromptResponse, Error>, oneshot::Canceled>),
        Chosen(usize, Option<String>),
        Command(Option<SessionCommand>),
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
    // Permission requests are answered from this loop, so the turn keeps reading
    // updates and can still be cancelled while the user decides.
    let mut permissions: Vec<PendingPermission> = Vec::new();
    loop {
        let update = async { Next::Update(session.read_update().await) };
        let answered = async { Next::Answered((&mut answer).await) };
        let chosen = async {
            if permissions.is_empty() {
                return smol::future::pending().await;
            }
            let choices = permissions
                .iter_mut()
                .map(|permission| &mut permission.choice);
            let (choice, ix, _) = futures::future::select_all(choices).await;
            Next::Chosen(ix, choice.ok().flatten())
        };
        let command = async { Next::Command(commands.recv().await.ok()) };
        // `or` polls its left side first. Updates come before the answer, and updates
        // sent before the answer are already queued when it arrives, so none are left behind.
        let next = smol::future::or(
            update,
            smol::future::or(answered, smol::future::or(chosen, command)),
        )
        .await;
        match next {
            Next::Update(update) => {
                if let SessionMessage::SessionMessage(dispatch) = update? {
                    handle(dispatch, key, events, &mut permissions).await?;
                }
            }
            Next::Answered(answer) => {
                return answer
                    .map_err(|_| internal_error("the prompt was dropped"))?
                    .map(|response| response.stop_reason);
            }
            Next::Chosen(ix, choice) => permissions.remove(ix).answer(choice)?,
            Next::Command(Some(SessionCommand::Cancel)) => {
                let id = session.session_id().clone();
                session
                    .connection()
                    .send_notification(CancelNotification::new(id))?;
                // ACP has the client answer every open permission request as cancelled.
                for permission in permissions.drain(..) {
                    permission.answer(None)?;
                }
            }
            Next::Command(Some(SessionCommand::Prompt(text))) => waiting.push_back(text),
            Next::Command(Some(SessionCommand::ChangeSetting(setting, value))) => {
                change_setting(key, session, setting, value, events)?
            }
            Next::Command(None) => return Err(internal_error("the session was closed")),
        }
    }
}

/// Asks the agent to change one of the session's settings. Its answer, the settings
/// as they now stand, goes to the front end whenever it comes.
fn change_setting(
    key: SessionKey,
    session: &ActiveSession<'_, Agent>,
    setting: String,
    value: SettingValue,
    events: &Sender<Event>,
) -> Result<(), Error> {
    let events = events.clone();
    let value = match value {
        SettingValue::Choice(id) => SessionConfigOptionValue::value_id(id),
        SettingValue::Switch(on) => SessionConfigOptionValue::boolean(on),
    };
    let request =
        SetSessionConfigOptionRequest::new(session.session_id().clone(), setting.clone(), value);
    session
        .connection()
        .prepare_request(request)
        .on_receiving_result(async move |result| {
            let event = match result {
                Ok(response) => SessionEvent::Settings(settings_of(&response.config_options)),
                Err(err) => {
                    SessionEvent::SettingFailed(format!("couldn't change {setting}: {err}"))
                }
            };
            emit(&events, Event::Session(key, event));
            Ok(())
        })
}

async fn handle(
    dispatch: Dispatch,
    key: SessionKey,
    events: &Sender<Event>,
    permissions: &mut Vec<PendingPermission>,
) -> Result<(), Error> {
    MatchDispatch::new(dispatch)
        .if_notification(async |notification: SessionNotification| {
            if let Some(event) = flatten(notification.update) {
                emit(events, Event::Session(key, event));
            }
            Ok(())
        })
        .await
        .if_request(async |request: RequestPermissionRequest, responder| {
            permissions.push(ask_permission(request, responder, key, events));
            Ok(())
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

/// A permission request the user has not answered yet.
struct PendingPermission {
    options: Vec<PermissionOption>,
    responder: Responder<RequestPermissionResponse>,
    choice: oneshot::Receiver<Option<String>>,
}

impl PendingPermission {
    /// Answers the agent with the chosen option; `None`, or an id it did not offer, cancels.
    fn answer(self, chosen: Option<String>) -> Result<(), Error> {
        let outcome = match self
            .options
            .into_iter()
            .find(|option| Some(option.option_id.0.as_ref()) == chosen.as_deref())
        {
            Some(option) => {
                RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option.option_id))
            }
            None => RequestPermissionOutcome::Cancelled,
        };
        self.responder
            .respond(RequestPermissionResponse::new(outcome))
    }
}

/// Shows the request to the front end; the turn answers the agent once the user chooses.
fn ask_permission(
    request: RequestPermissionRequest,
    responder: Responder<RequestPermissionResponse>,
    key: SessionKey,
    events: &Sender<Event>,
) -> PendingPermission {
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
    PendingPermission {
        options: request.options,
        responder,
        choice,
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
        SessionUpdate::ConfigOptionUpdate(update) => {
            SessionEvent::Settings(settings_of(&update.config_options))
        }
        SessionUpdate::CurrentModeUpdate(update) => {
            SessionEvent::ModeChanged(update.current_mode_id.0.to_string())
        }
        _ => return None,
    })
}

/// The agent's session config options of the kinds we know: a pick of one of several
/// values, or on or off.
fn settings_of(options: &[SessionConfigOption]) -> Vec<Setting> {
    options
        .iter()
        .filter_map(|option| {
            let (value, choices) = match &option.kind {
                SessionConfigKind::Select(select) => {
                    let choices = match &select.options {
                        SessionConfigSelectOptions::Ungrouped(choices) => choices.iter().collect(),
                        SessionConfigSelectOptions::Grouped(groups) => groups
                            .iter()
                            .flat_map(|group| &group.options)
                            .collect::<Vec<_>>(),
                        _ => Vec::new(),
                    };
                    let choices = choices
                        .into_iter()
                        .map(|choice| SettingChoice {
                            id: choice.value.0.to_string(),
                            name: choice.name.clone(),
                            description: choice.description.clone(),
                        })
                        .collect();
                    (
                        SettingValue::Choice(select.current_value.0.to_string()),
                        choices,
                    )
                }
                SessionConfigKind::Boolean(boolean) => {
                    (SettingValue::Switch(boolean.current_value), Vec::new())
                }
                _ => return None,
            };
            Some(Setting {
                id: option.id.0.to_string(),
                name: option.name.clone(),
                kind: match option.category {
                    Some(SessionConfigOptionCategory::Model) => SettingKind::Model,
                    Some(SessionConfigOptionCategory::Mode) => SettingKind::Mode,
                    Some(SessionConfigOptionCategory::ThoughtLevel) => SettingKind::ThoughtLevel,
                    Some(SessionConfigOptionCategory::ModelConfig) => SettingKind::ModelConfig,
                    _ => SettingKind::Other,
                },
                value,
                choices,
            })
        })
        .collect()
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

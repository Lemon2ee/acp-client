//! Runs one ACP agent connection on its own thread and exposes it as two channels,
//! so a front end (CLI, gpui window) never touches the ACP runtime directly.
//! The connection can hold several sessions, each running as its own task.
//!
//! How long the agent takes to answer each request is sent to `tracing` as an info
//! event with millisecond fields. Nothing is recorded unless a front end subscribes,
//! as acp-gui does with its `profile` feature. They are events rather than spans
//! because the time goes to waiting on the agent, and Tracy draws a span of an async
//! task only while the task is being polled.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    AuthMethod, BooleanConfigOptionCapabilities, CancelNotification, ClientCapabilities,
    ClientSessionCapabilities, ContentBlock, ErrorCode, Implementation, InitializeRequest,
    PermissionOption, PermissionOptionKind, PromptRequest, PromptResponse,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionConfigKind, SessionConfigOption, SessionConfigOptionCategory,
    SessionConfigOptionValue, SessionConfigOptionsCapabilities, SessionConfigSelectOptions,
    SessionId, SessionNotification, SessionUpdate, SetSessionConfigOptionRequest,
    SetSessionConfigOptionResponse, StopReason,
};
use agent_client_protocol::util::{MatchDispatch, internal_error};
pub use agent_client_protocol::{AcpAgent, Error};
use agent_client_protocol::{
    ActiveSession, Agent, Client, ConnectionTo, Dispatch, Responder, SessionMessage,
};
use futures::channel::oneshot;
use serde::{Deserialize, Serialize};
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
    /// `SettingChanged`, or `SettingFailed`; a prompt sent after it waits for the answer.
    ChangeSetting(SessionKey, String, SettingValue),
    /// Drops the session: its task is cancelled, whether it is still creating the
    /// session or running a turn, so no more of its events come. A permission request
    /// the turn was waiting on is answered as cancelled; the agent is left to finish
    /// the turn by itself.
    CloseSession(SessionKey),
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
    /// the session exists, and again whenever the agent says they changed.
    Settings(Vec<Setting>),
    /// The agent answered a `ChangeSetting` of `setting`, with every setting as it now
    /// stands. Each change gets one answer, this or `SettingFailed`, in the order the
    /// changes were made.
    SettingChanged {
        setting: String,
        settings: Vec<Setting>,
    },
    /// The agent moved the session to another mode, by the id of its choice in the
    /// `Mode` setting.
    ModeChanged(String),
    /// The agent refused a `ChangeSetting`, or hadn't answered it by the time a prompt
    /// stopped waiting; the setting keeps its value.
    SettingFailed {
        setting: String,
        message: String,
    },
}

/// Something the agent lets the user choose for one session, such as its model.
/// Serializable so a front end can show the settings from its last run while the
/// agent creates a session.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SettingValue {
    /// The id of one of the setting's choices.
    Choice(String),
    /// Whether a setting that is on or off is on.
    Switch(bool),
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// An agent the client knows how to start: Claude Code or Codex, through its official
/// ACP adapter. A front end can run one of each and let the user pick between them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Harness {
    Claude,
    Codex,
}

impl Harness {
    pub const ALL: [Harness; 2] = [Harness::Claude, Harness::Codex];

    pub fn name(self) -> &'static str {
        match self {
            Harness::Claude => "Claude",
            Harness::Codex => "Codex",
        }
    }

    pub fn agent(self) -> AcpAgent {
        match self {
            Harness::Claude => CLAUDE_ADAPTER,
            Harness::Codex => CODEX_ADAPTER,
        }
        .parse()
        .expect("the adapter commands are well-formed")
    }
}

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
            // Closed first, so by the time the front end hears `Closed` a command it
            // sends is refused rather than lost.
            drop(command_rx);
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
    let started = Instant::now();
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
            let agent = describe(init.agent_info.as_ref());
            // From starting the process, so an `npx` install or a slow agent start shows.
            tracing::info!(%agent, took_ms = millis(started), "initialize");
            login_options = init.auth_methods.iter().map(login_option).collect();
            emit(events, Event::Connected { agent });

            // Each session runs as a task on this thread: turns in different sessions
            // overlap, and a slow `session/new` never holds up other sessions' commands.
            let tasks = smol::LocalExecutor::new();
            // Each session's queue and its task, which dropping cancels.
            let mut sessions: HashMap<SessionKey, (Sender<SessionCommand>, smol::Task<()>)> =
                HashMap::new();
            tasks
                .run(async {
                    while let Some(command) = next_command(commands, &cx).await? {
                        let (key, command) = match command {
                            Command::NewSession(key) => {
                                if let Entry::Vacant(entry) = sessions.entry(key) {
                                    let (command_tx, command_rx) = smol::channel::unbounded();
                                    let task = tasks.spawn(run_session(
                                        key,
                                        cx.clone(),
                                        cwd.to_path_buf(),
                                        command_rx,
                                        events.clone(),
                                        login_options.clone(),
                                    ));
                                    entry.insert((command_tx, task));
                                }
                                continue;
                            }
                            Command::Prompt(key, text) => (key, SessionCommand::Prompt(text)),
                            Command::Cancel(key) => (key, SessionCommand::Cancel),
                            Command::ChangeSetting(key, setting, value) => {
                                (key, SessionCommand::ChangeSetting(setting, value))
                            }
                            Command::CloseSession(key) => {
                                // Dropping the task cancels it where it is, so neither
                                // a `session/new` still out nor a queued prompt goes on.
                                sessions.remove(&key);
                                continue;
                            }
                        };
                        if let Some((session, _)) = sessions.get(&key) {
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
    // Setting changes made while there was no session to make them to, which the
    // session gets before the prompt that created it again.
    let mut changes_waiting = Vec::new();
    loop {
        // `run_until` sends `session/new` from this task, so a refusal comes back here;
        // `start_session` sends it from a connection task, where an error ends the
        // whole connection.
        let asked = Instant::now();
        let created = cx
            .build_session(&cwd)
            .block_task()
            .run_until(async |mut session| {
                tracing::info!(session = key.0, took_ms = millis(asked), "session/new");
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
                    std::mem::take(&mut changes_waiting),
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
        tracing::warn!(
            session = key.0,
            took_ms = millis(asked),
            error = %message,
            "session/new failed"
        );
        let failed = match retry {
            None => Event::SessionFailed(key, message),
            Some(_) => Event::Session(key, SessionEvent::TurnFailed(message)),
        };
        emit(&events, failed);
        retry = loop {
            match commands.recv().await {
                Ok(SessionCommand::Prompt(text)) => break Some(text),
                // No session, so nothing is running to stop.
                Ok(SessionCommand::Cancel) => {}
                Ok(SessionCommand::ChangeSetting(setting, value)) => {
                    changes_waiting.push((setting, value))
                }
                Err(_) => return,
            }
        };
    }
}

/// Runs the session's prompts one at a time, as they arrive, until the front end has gone.
/// Prompts sent during a turn wait in `waiting` for their own, and every prompt waits
/// for the agent to answer the setting changes sent before it, `first` included.
async fn serve(
    key: SessionKey,
    session: &mut ActiveSession<'_, Agent>,
    mut waiting: VecDeque<String>,
    first: Vec<(String, SettingValue)>,
    commands: &Receiver<SessionCommand>,
    events: &Sender<Event>,
    login_options: &[String],
) {
    let mut changes = Changes::default();
    for (setting, value) in first {
        // Only a closed connection fails this, which ends the session anyway.
        let _ = change_setting(key, session, setting, value, events, &mut changes);
    }
    loop {
        let text = match waiting.pop_front() {
            Some(text) => text,
            None => match commands.recv().await {
                Ok(SessionCommand::Prompt(text)) => text,
                // Nothing is running, so there is nothing to stop.
                Ok(SessionCommand::Cancel) => continue,
                Ok(SessionCommand::ChangeSetting(setting, value)) => {
                    // Only a closed connection fails this, which ends the session anyway.
                    let _ = change_setting(key, session, setting, value, events, &mut changes);
                    continue;
                }
                Err(_) => return,
            },
        };
        // The agent may still be applying a change, and a prompt it took first would
        // run with the setting as it was.
        let waited =
            wait_for_changes(key, session, &mut waiting, commands, events, &mut changes).await;
        match waited {
            Waited::Answered => {}
            // Stopped before it was sent, so it never runs.
            Waited::Stopped => {
                let stop_reason = format!("{:?}", StopReason::Cancelled);
                emit(
                    events,
                    Event::Session(key, SessionEvent::TurnEnded { stop_reason }),
                );
                continue;
            }
            Waited::Gone => return,
        }
        let outcome = match run_turn(
            key,
            session,
            text,
            commands,
            &mut waiting,
            &mut changes,
            events,
        )
        .await
        {
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
    changes: &mut Changes,
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
    // For the timing sent once the agent answers: when its first update came, how many
    // came, and when the user stopped the turn, if they did.
    let sent = Instant::now();
    let mut first_update_ms = None;
    let mut updates: u64 = 0;
    let mut stopped = None;
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
                    first_update_ms.get_or_insert_with(|| millis(sent));
                    updates += 1;
                    handle(dispatch, key, events, &mut permissions).await?;
                }
            }
            Next::Answered(answer) => {
                // `took_ms` includes the time spent on the user's permission answers,
                // which `session/request_permission` reports one by one.
                tracing::info!(
                    session = key.0,
                    took_ms = millis(sent),
                    first_update_ms,
                    updates,
                    after_stop_ms = stopped.map(millis),
                    "session/prompt"
                );
                return answer
                    .map_err(|_| internal_error("the prompt was dropped"))?
                    .map(|response| response.stop_reason);
            }
            Next::Chosen(ix, choice) => {
                let permission = permissions.remove(ix);
                tracing::info!(
                    session = key.0,
                    waited_ms = millis(permission.asked),
                    "session/request_permission"
                );
                permission.answer(choice)?
            }
            Next::Command(Some(SessionCommand::Cancel)) => {
                stopped.get_or_insert_with(Instant::now);
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
                change_setting(key, session, setting, value, events, changes)?
            }
            Next::Command(None) => return Err(internal_error("the session was closed")),
        }
    }
}

/// How long a prompt waits for the agent to answer the setting changes sent before it.
/// An agent that hasn't answered by then gets the prompt anyway.
const SETTING_ANSWER_TIMEOUT: Duration = Duration::from_secs(5);

/// The session's setting changes the agent hasn't answered yet.
#[derive(Default)]
struct Changes {
    queue: ChangeQueue,
    /// Told once every change sent so far is answered, as they go one at a time.
    last: Option<oneshot::Receiver<()>>,
}

/// The changes waiting for an answer, oldest first, shared with the callbacks that take
/// the answers. Only the first is out at a time: each goes once the agent has answered
/// the one before, so the agent applies them in the order they were made and the last
/// answer is the newest.
#[derive(Clone, Default)]
struct ChangeQueue(Arc<Mutex<Queue>>);

#[derive(Default)]
struct Queue {
    changes: VecDeque<Change>,
    next_id: u64,
}

struct Change {
    /// Tells this change's answer from a late one to a change given up on.
    id: u64,
    setting: String,
    value: SettingValue,
    /// Set by whoever sends it, so it goes out once.
    sent: bool,
    /// Told when the agent answers.
    answered: oneshot::Sender<()>,
}

impl ChangeQueue {
    fn lock(&self) -> MutexGuard<'_, Queue> {
        // Nothing panics while holding it, so it is never poisoned.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// How waiting for the setting changes before a prompt ended.
enum Waited {
    /// The agent answered them, or took too long and the front end was told so.
    Answered,
    /// The front end stopped the prompt.
    Stopped,
    /// The front end has gone.
    Gone,
}

/// Waits for the agent to answer the setting changes sent so far, still taking
/// commands meanwhile: a stop drops the prompt, and other prompts and changes queue up.
/// After `SETTING_ANSWER_TIMEOUT`, the changes the agent still owes are given up on and
/// reported failed.
async fn wait_for_changes(
    key: SessionKey,
    session: &ActiveSession<'_, Agent>,
    waiting: &mut VecDeque<String>,
    commands: &Receiver<SessionCommand>,
    events: &Sender<Event>,
    changes: &mut Changes,
) -> Waited {
    enum Next {
        Answered,
        TimedOut,
        Command(Option<SessionCommand>),
    }
    let mut deadline = smol::Timer::after(SETTING_ANSWER_TIMEOUT);
    while let Some(answer) = changes.last.as_mut() {
        let answered = async {
            let _ = answer.await;
            Next::Answered
        };
        let timed_out = async {
            (&mut deadline).await;
            Next::TimedOut
        };
        let command = async { Next::Command(commands.recv().await.ok()) };
        match smol::future::or(answered, smol::future::or(timed_out, command)).await {
            Next::Answered => changes.last = None,
            Next::TimedOut => {
                // None of them goes out from here on, and a late answer to the one out
                // leaves the queue alone, so the next change goes at once.
                let given_up: Vec<Change> = changes.queue.lock().changes.drain(..).collect();
                for change in given_up {
                    let message = format!(
                        "couldn't change {}: the agent didn't answer within {} s",
                        change.setting,
                        SETTING_ANSWER_TIMEOUT.as_secs()
                    );
                    let setting = change.setting;
                    let failed = SessionEvent::SettingFailed { setting, message };
                    emit(events, Event::Session(key, failed));
                }
                changes.last = None;
            }
            Next::Command(Some(SessionCommand::Cancel)) => return Waited::Stopped,
            Next::Command(Some(SessionCommand::Prompt(text))) => waiting.push_back(text),
            Next::Command(Some(SessionCommand::ChangeSetting(setting, value))) => {
                // Only a closed connection fails this, which ends the session anyway.
                let _ = change_setting(key, session, setting, value, events, changes);
            }
            Next::Command(None) => return Waited::Gone,
        }
    }
    Waited::Answered
}

/// Asks the agent to change one of the session's settings, once it has answered the
/// changes asked before, for the next prompt to wait on. The answer, the settings as
/// they then stand, goes to the front end.
fn change_setting(
    key: SessionKey,
    session: &ActiveSession<'_, Agent>,
    setting: String,
    value: SettingValue,
    events: &Sender<Event>,
    changes: &mut Changes,
) -> Result<(), Error> {
    let (answered, answer) = oneshot::channel();
    changes.last = Some(answer);
    {
        let mut queue = changes.queue.lock();
        let id = queue.next_id;
        queue.next_id += 1;
        queue.changes.push_back(Change {
            id,
            setting,
            value,
            sent: false,
            answered,
        });
    }
    send_change(
        key,
        session.connection().clone(),
        session.session_id().clone(),
        events.clone(),
        changes.queue.clone(),
    )
}

/// Sends the first of the session's waiting setting changes unless it is out already,
/// and once the agent answers it, the next.
fn send_change(
    key: SessionKey,
    connection: ConnectionTo<Agent>,
    session_id: SessionId,
    events: Sender<Event>,
    queue: ChangeQueue,
) -> Result<(), Error> {
    // Claimed under the lock, so a change that two callers reach goes out once.
    let first = queue.lock().changes.front_mut().and_then(|change| {
        (!change.sent).then(|| {
            change.sent = true;
            (change.id, change.setting.clone(), change.value.clone())
        })
    });
    let Some((id, setting, value)) = first else {
        return Ok(());
    };
    let value = match value {
        SettingValue::Choice(id) => SessionConfigOptionValue::value_id(id),
        SettingValue::Switch(on) => SessionConfigOptionValue::boolean(on),
    };
    let request = SetSessionConfigOptionRequest::new(session_id.clone(), setting.clone(), value);
    let asked = Instant::now();
    let callback = {
        let (connection, queue) = (connection.clone(), queue.clone());
        async move |result: Result<SetSessionConfigOptionResponse, Error>| {
            let took_ms = millis(asked);
            let event = match result {
                Ok(response) => {
                    tracing::info!(session = key.0, took_ms, %setting, "session/set_config_option");
                    SessionEvent::SettingChanged {
                        settings: settings_of(&response.config_options),
                        setting,
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        session = key.0,
                        took_ms,
                        %setting,
                        error = %err,
                        "session/set_config_option failed"
                    );
                    SessionEvent::SettingFailed {
                        message: format!("couldn't change {setting}: {err}"),
                        setting,
                    }
                }
            };
            emit(&events, Event::Session(key, event));
            // A late answer to a change given up on leaves the queue to the changes
            // after it, which went out without waiting.
            let answered = {
                let mut locked = queue.lock();
                let ours = locked.changes.front().is_some_and(|change| change.id == id);
                ours.then(|| locked.changes.pop_front()).flatten()
            };
            if let Some(change) = answered {
                let _ = change.answered.send(());
                // Only a closed connection fails this, and then no answer will come.
                let _ = send_change(key, connection, session_id, events, queue);
            }
            Ok(())
        }
    };
    let sent = connection
        .prepare_request(request)
        .on_receiving_result(callback);
    if sent.is_err() {
        // No answer will come to any of them.
        queue.lock().changes.clear();
    }
    sent
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

/// A permission request the user has not answered yet. Dropped unanswered, as when
/// its session is closed, it answers the agent with a cancellation.
struct PendingPermission {
    /// When the user was asked, to report how long the turn waited on them.
    asked: Instant,
    options: Vec<PermissionOption>,
    /// Taken by the answer.
    responder: Option<Responder<RequestPermissionResponse>>,
    choice: oneshot::Receiver<Option<String>>,
}

impl PendingPermission {
    /// Answers the agent with the chosen option; `None`, or an id it did not offer, cancels.
    fn answer(mut self, chosen: Option<String>) -> Result<(), Error> {
        let outcome = match self
            .options
            .iter()
            .find(|option| Some(option.option_id.0.as_ref()) == chosen.as_deref())
        {
            Some(option) => RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                option.option_id.clone(),
            )),
            None => RequestPermissionOutcome::Cancelled,
        };
        match self.responder.take() {
            Some(responder) => responder.respond(RequestPermissionResponse::new(outcome)),
            None => Ok(()),
        }
    }
}

impl Drop for PendingPermission {
    fn drop(&mut self) {
        if let Some(responder) = self.responder.take() {
            let cancelled = RequestPermissionOutcome::Cancelled;
            let _ = responder.respond(RequestPermissionResponse::new(cancelled));
        }
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
        asked: Instant::now(),
        options: request.options,
        responder: Some(responder),
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

/// Whole milliseconds since `since`, for the timings sent to `tracing`.
fn millis(since: Instant) -> u64 {
    since.elapsed().as_millis() as u64
}

fn emit(events: &Sender<Event>, event: Event) {
    // A closed channel means the front end quit; nothing left to tell.
    let _ = events.send_blocking(event);
}

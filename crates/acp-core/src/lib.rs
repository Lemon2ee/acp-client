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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use agent_client_protocol::schema::v1::{
    AuthMethod, AvailableCommand, BooleanConfigOptionCapabilities, CancelNotification,
    ClientCapabilities, ClientSessionCapabilities, CloseSessionRequest, ContentBlock,
    DeleteSessionRequest, ErrorCode, ImageContent, Implementation, InitializeRequest,
    PermissionOption, PermissionOptionKind, PromptRequest, PromptResponse,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse, ResourceLink,
    SelectedPermissionOutcome, SessionConfigKind, SessionConfigOption, SessionConfigOptionCategory,
    SessionConfigOptionValue, SessionConfigOptionsCapabilities, SessionConfigSelectOptions,
    SessionId, SessionNotification, SessionUpdate, SetSessionConfigOptionRequest,
    SetSessionConfigOptionResponse, StopReason, ToolCallContent, ToolCallLocation, ToolCallStatus,
    ToolKind as ProtocolToolKind, Usage,
};
use agent_client_protocol::schema::{MaybeUndefined, ProtocolVersion};
use agent_client_protocol::util::{MatchDispatch, internal_error};
pub use agent_client_protocol::{AcpAgent, Error};
use agent_client_protocol::{
    ActiveSession, Agent, Client, ConnectionTo, Dispatch, JsonRpcNotification, Responder,
    SessionMessage,
};
use base64::prelude::{BASE64_STANDARD, Engine as _};
use futures::FutureExt as _;
use futures::channel::oneshot;
use serde::{Deserialize, Serialize};
pub use smol::channel::{Receiver, Sender};
use url::Url;

/// Names a session on the connection. The front end picks it, so it can show the
/// session before the agent has created it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SessionKey(pub u64);

/// What the user sends as one prompt: their text, and the files they put with it, which
/// are read as the prompt goes out.
#[derive(Clone, Debug, Default)]
pub struct Prompt {
    pub text: String,
    pub attachments: Vec<Attachment>,
}

impl From<String> for Prompt {
    fn from(text: String) -> Self {
        Self {
            text,
            attachments: Vec::new(),
        }
    }
}

/// A file put with a prompt, by its path on this machine. An image goes to the agent as
/// its pixels where it can (see `attachment_block`); anything else goes as a link to the
/// file, which the agent opens with its own tools.
#[derive(Clone, Debug)]
pub enum Attachment {
    /// An image file, with its type, such as `image/png`.
    Image { path: PathBuf, mime: String },
    /// Any other file, or a folder.
    File { path: PathBuf },
}

/// What the front end asks the agent to do.
pub enum Command {
    /// Creates the session now, ahead of its first prompt. Answered with `SessionReady`
    /// or `SessionFailed`. A key already in use keeps its session, and the repeated
    /// command is ignored.
    NewSession(SessionKey),
    /// Loads a session the agent created in an earlier run, by the id
    /// `SessionEvent::Created` gave, in the folder it was created in. Its transcript
    /// comes back as session events, then `SessionReady`, or `SessionFailed` after which
    /// its next prompt tries again. A key already in use keeps its session.
    LoadSession(SessionKey, String, PathBuf),
    /// Runs after the session's earlier prompts. A session not created yet, or that the
    /// agent failed to create, is created first; if that fails, so does the prompt.
    Prompt(SessionKey, Prompt),
    /// Stop the session's turn in flight; ignored when idle.
    Cancel(SessionKey),
    /// Asks the agent what fills the session's context window, with its `/context`
    /// command. Answered with `ContextReport`, never in the transcript, and no `TurnEnded`
    /// follows. It runs once no prompt is waiting, and a prompt sent while it runs stops
    /// it, so it never holds one up. A session the agent hasn't created yet ignores it.
    ReadContext(SessionKey),
    /// Sets one of the session's settings, by id, during a turn or not. Answered with
    /// `SettingChanged`, or `SettingFailed`; a prompt sent after it waits for the answer.
    /// A session not created yet gets the change once its first prompt creates it.
    ChangeSetting(SessionKey, String, SettingValue),
    /// Drops the session: commands still queued for it never run, a turn in flight is
    /// stopped and its open permission requests answered as cancelled, and the agent is
    /// asked to close it where the agent can, once it exists if the agent is still
    /// creating it. No more of its events come.
    CloseSession(SessionKey),
    /// Closes the session as `CloseSession` does, then asks the agent to delete what it
    /// keeps of the session with this id, where the agent can: Claude's adapter deletes
    /// its transcript, and Codex's archives its thread. The id may be of a session from
    /// an earlier run that this one never loaded. A failure comes as an `Event::Error`.
    DeleteSession(SessionKey, String),
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
    /// How much of its plan's usage limits the agent's login has used, each time that
    /// changes: every window, in the order the agent shows them. Empty when the login has
    /// no plan limits, such as an API key. Only an agent that pushes
    /// `_auth/rate_limits_update` sends it; until one comes, the limits are unknown.
    RateLimits(Vec<RateLimitWindow>),
    /// The connection failed (missing login, agent crashed, ...); every session is gone.
    Error(String),
    /// No more events will follow.
    Closed,
}

/// Something that happened inside one session.
pub enum SessionEvent {
    /// The agent has the session under this id, which `Command::LoadSession` takes to
    /// load it in a later run. Comes first, each time the agent creates or loads it.
    Created(String),
    /// Part of a message the user sent, as the agent replays a session it loads. A
    /// prompt sent in this run doesn't come back this way.
    UserText(String),
    AgentText(String),
    AgentThought(String),
    ToolCall {
        id: String,
        title: String,
        kind: ToolKind,
        status: ToolStatus,
        content: Vec<ToolContent>,
        locations: Vec<ToolLocation>,
    },
    /// What changed in a tool call: each field the agent sent replaces the one before,
    /// the content and the locations as a whole.
    ToolCallUpdate {
        id: String,
        title: Option<String>,
        kind: Option<ToolKind>,
        status: Option<ToolStatus>,
        content: Option<Vec<ToolContent>>,
        locations: Option<Vec<ToolLocation>>,
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
    /// The agent's title for the session. Claude's and Codex's adapters have a small model
    /// make one a few seconds after the first turn, so it can come between turns, and the
    /// same title can come again.
    Title(String),
    /// How much of its context window the session takes up, each time the agent counts:
    /// Claude's adapter as it replies, at the end of each turn and after compacting;
    /// Codex's after each request to the model. Neither sends it as it loads a session.
    ContextUsage(ContextUsage),
    /// The tokens the turn took, just before its `TurnEnded`, from an agent that counts
    /// them, as both adapters do.
    TurnUsage(TurnUsage),
    /// What fills the context window, as the agent's Markdown, answering `ReadContext`.
    /// `None` when the agent offers no `/context` command, or its answer failed or was
    /// stopped by a prompt.
    ContextReport(Option<String>),
    /// Whether `ReadContext` can say what fills the window: the agent's list of the
    /// session's commands has `/context` in it. Comes when that changes, and the agent
    /// can send a new list at any time; until one comes, it can't.
    ContextReadable(bool),
    /// The agent refused a `ChangeSetting`, or hadn't answered it by the time a prompt
    /// stopped waiting; the setting keeps its value.
    SettingFailed {
        setting: String,
        message: String,
    },
}

/// How full a session's context window is, in tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextUsage {
    /// What the agent last sent the model.
    pub used: u64,
    /// The most the model takes.
    pub size: u64,
}

/// The tokens one turn took, as the agent answered its prompt. A count the agent left
/// out is 0.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnUsage {
    /// Input the model read afresh: both adapters count the cached part apart.
    pub input: u64,
    pub cached_read: u64,
    pub cached_write: u64,
    pub output: u64,
    /// Of the output, how much was thinking, where the agent says (Codex's does).
    pub thought: u64,
}

/// Where a tool call stands. Shown as ACP names it: `Pending`, `InProgress`, `Completed`,
/// `Failed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

impl ToolStatus {
    /// The call is over, whether it worked or not.
    pub fn finished(self) -> bool {
        matches!(self, ToolStatus::Completed | ToolStatus::Failed)
    }
}

impl std::fmt::Display for ToolStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl From<ToolCallStatus> for ToolStatus {
    fn from(status: ToolCallStatus) -> Self {
        match status {
            ToolCallStatus::Pending => ToolStatus::Pending,
            ToolCallStatus::InProgress => ToolStatus::InProgress,
            ToolCallStatus::Completed => ToolStatus::Completed,
            ToolCallStatus::Failed => ToolStatus::Failed,
            // ACP may add statuses; one this version doesn't know is taken as still running.
            _ => ToolStatus::InProgress,
        }
    }
}

/// What a tool call does, as the agent sorts it, for a front end to pick its icon and
/// how to show it. Named as ACP names them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    SwitchMode,
    Other,
}

impl From<ProtocolToolKind> for ToolKind {
    fn from(kind: ProtocolToolKind) -> Self {
        match kind {
            ProtocolToolKind::Read => ToolKind::Read,
            ProtocolToolKind::Edit => ToolKind::Edit,
            ProtocolToolKind::Delete => ToolKind::Delete,
            ProtocolToolKind::Move => ToolKind::Move,
            ProtocolToolKind::Search => ToolKind::Search,
            ProtocolToolKind::Execute => ToolKind::Execute,
            ProtocolToolKind::Think => ToolKind::Think,
            ProtocolToolKind::Fetch => ToolKind::Fetch,
            ProtocolToolKind::SwitchMode => ToolKind::SwitchMode,
            // ACP may add kinds; one this version doesn't know is shown as any other tool.
            _ => ToolKind::Other,
        }
    }
}

/// Something a tool call shows: what it produced, or the change it makes to a file.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ToolContent {
    /// Markdown. Claude's adapter sends a command's output and a file it read as a fenced
    /// code block, the file's lines numbered.
    Text(String),
    /// A change to one file: its text before, none for a new file, and after. Agents may
    /// send only the part that changes rather than the whole file: Claude's adapter sends
    /// an edit's snippet, and once it is done one diff for each hunk, whose first line is
    /// the `line` of the location at the same place.
    Diff {
        path: PathBuf,
        old: Option<String>,
        new: String,
    },
}

/// A file a tool call works on, and the line it works at where the agent says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolLocation {
    pub path: PathBuf,
    pub line: Option<u32>,
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
    /// The id of the tool call it asks about. The call's fields as the request shows them
    /// come just before it, as a `ToolCallUpdate`: the agent may show more there, such as
    /// the change an edit would make, and may never have sent the call as a `ToolCall`.
    pub tool_call: String,
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
    /// The answer stands for later calls like this one too, not just this one.
    pub always: bool,
}

/// One of the usage limits of the agent's login, such as the 5-hour or the weekly window
/// of a Claude or ChatGPT plan, and how much of it is used.
#[derive(Clone, Debug, PartialEq)]
pub struct RateLimitWindow {
    /// Stable for the same window across updates: `five_hour`, `seven_day:Fable`,
    /// `codex:primary`.
    pub id: String,
    /// The agent's own name for it: "5-hour limit", "Weekly · Fable", "Codex 5h limit".
    pub label: String,
    /// Share of the window used, 0 to 100.
    pub used_percent: f64,
    /// When the window starts over, where the agent says.
    pub resets_at: Option<SystemTime>,
    /// How long the window is, where the agent says: 300 for 5 hours, 10080 for a week.
    pub window_minutes: Option<u32>,
    /// Set when only some models count against the window: the plan's name for them,
    /// such as "Fable". It is not a model id.
    pub model: Option<String>,
}

pub struct AgentHandle {
    pub commands: Sender<Command>,
    pub events: Receiver<Event>,
}

/// A command for one session. Each session reads these from one queue in order,
/// so a cancel never overtakes the prompt it is meant to stop.
enum SessionCommand {
    Prompt(Prompt),
    Cancel,
    /// A setting's id and its new value.
    ChangeSetting(String, SettingValue),
    ReadContext,
}

/// One turn of a session: a prompt, or a `ReadContext`.
enum Turn {
    Prompt(Prompt),
    /// `/context`, whose answer goes to the front end as a `ContextReport` instead of
    /// the transcript.
    ReadContext,
}

/// What waits for a session's turn in flight to end.
struct Waiting {
    /// Prompts, oldest first.
    prompts: VecDeque<Prompt>,
    /// A `ReadContext`, which runs once no prompt waits.
    read_context: bool,
}

impl Waiting {
    fn next(&mut self) -> Option<Turn> {
        match self.prompts.pop_front() {
            Some(prompt) => Some(Turn::Prompt(prompt)),
            None => std::mem::take(&mut self.read_context).then_some(Turn::ReadContext),
        }
    }
}

/// Claude Code's command that says what fills the context window. claude-agent-acp
/// 0.94.0 runs it without the model (its `LOCAL_ONLY_COMMANDS`), so it costs nothing and
/// leaves the conversation as it was, and leaves it out when it replays a session.
const CONTEXT_COMMAND: &str = "context";

/// What a session's task needs to know of its agent, from `initialize`.
#[derive(Default)]
struct AgentInfo {
    /// The ways the agent offers to log in, for the message when the login is missing.
    login_options: Vec<String>,
    /// The agent takes `session/close`.
    closes: bool,
    /// The agent takes `session/load`.
    loads: bool,
    /// The agent takes `session/delete`.
    deletes: bool,
    /// The agent takes images in a prompt as pixels.
    images: bool,
}

/// A session's task and the queue it reads its commands from.
struct SessionTask {
    queue: Sender<SessionCommand>,
    /// The queue's own end, to empty it when the session is closed.
    unread: Receiver<SessionCommand>,
    task: smol::Task<()>,
}

impl SessionTask {
    /// Has the task close the session: the commands it hasn't read yet are dropped, and
    /// it sees its queue closed at the next one it waits for. The task returned ends once
    /// it has told the agent, and must be detached or waited for: dropping it would cancel
    /// it where it is, and a `session/new` still out would leave the agent a session
    /// nobody can close.
    fn close(self) -> smol::Task<()> {
        while self.unread.try_recv().is_ok() {}
        self.queue.close();
        self.task
    }
}

/// A session's events to the front end. None goes once the front end has closed the
/// session, whichever task or callback would send it, so a late answer from the agent
/// needs no check of its own.
#[derive(Clone)]
struct SessionEvents {
    key: SessionKey,
    events: Sender<Event>,
    /// The session's command queue, which `SessionTask::close` closes.
    commands: Receiver<SessionCommand>,
    /// The agent's last list of the session's commands has `/context` in it.
    offers_context: Arc<AtomicBool>,
}

impl SessionEvents {
    /// The front end has closed the session.
    fn closed(&self) -> bool {
        self.commands.is_closed()
    }

    fn send(&self, event: Event) {
        if !self.closed() {
            emit(&self.events, event);
        }
    }

    fn emit(&self, event: SessionEvent) {
        self.send(Event::Session(self.key, event));
    }

    /// Takes the agent's list of the session's commands, which it sends again whenever
    /// they change.
    fn note_commands(&self, commands: &[AvailableCommand]) {
        let offers = commands
            .iter()
            .any(|command| command.name == CONTEXT_COMMAND);
        if self.offers_context.swap(offers, Ordering::Relaxed) != offers {
            self.emit(SessionEvent::ContextReadable(offers));
        }
    }

    fn offers_context(&self) -> bool {
        self.offers_context.load(Ordering::Relaxed)
    }
}

/// Pinned rather than `@latest`, so each adapter runs the CLI it bundles and was tested with
/// (claude-agent-acp pins Agent SDK 0.3.303 exactly; codex-acp takes @openai/codex 0.160.x).
///
/// For now both come from Lemon2ee's forks: the `preview/rate-limits` branch adds the
/// `_auth/rate_limits_update` push (see `RateLimitsUpdate`), whose upstream PRs are not
/// merged yet. Each is upstream main past 0.94.0 and 2.2.2, with the same Agent SDK and
/// @openai/codex, and the branch has its built `dist/` committed, as the packages build
/// nothing when installed. They run from GitHub's tarball of the commit: npm 10 fails on
/// `npx github:…` with "GitFetcher requires an Arborist constructor". Go back to the npm
/// releases once upstream ships the push.
const CLAUDE_ADAPTER: &str = "npx -y https://codeload.github.com/Lemon2ee/claude-agent-acp/tar.gz/bb11951ef4c2edf27ce50145e3994208336f87ec";
const CODEX_ADAPTER: &str = "npx -y https://codeload.github.com/Lemon2ee/codex-acp/tar.gz/079052f7c1f0c535a18849274cf48c1d9a21414b";

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
    let mut info = AgentInfo::default();
    let to_front_end = events.clone();
    Client
        .builder()
        .name("acp-gui")
        // Not part of ACP: the adapter forks `CLAUDE_ADAPTER` and `CODEX_ADAPTER` run push
        // it for the connection, not for a session, as the limits belong to the login.
        .on_receive_notification(
            async move |update: RateLimitsUpdate, _cx| {
                emit(&to_front_end, Event::RateLimits(update.windows()));
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
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
            info = AgentInfo {
                login_options: init.auth_methods.iter().map(login_option).collect(),
                closes: init.agent_capabilities.session_capabilities.close.is_some(),
                loads: init.agent_capabilities.load_session,
                deletes: init
                    .agent_capabilities
                    .session_capabilities
                    .delete
                    .is_some(),
                images: init.agent_capabilities.prompt_capabilities.image,
            };
            emit(events, Event::Connected { agent });

            // Each session runs as a task on this thread: turns in different sessions
            // overlap, and a slow `session/new` never holds up other sessions' commands.
            let tasks = smol::LocalExecutor::new();
            let mut sessions: HashMap<SessionKey, SessionTask> = HashMap::new();
            let start = |key, open| {
                let (queue, commands) = smol::channel::unbounded();
                let to_front_end = SessionEvents {
                    key,
                    events: events.clone(),
                    commands: commands.clone(),
                    offers_context: Arc::default(),
                };
                SessionTask {
                    queue,
                    unread: commands.clone(),
                    task: tasks.spawn(run_session(
                        cx.clone(),
                        cwd.to_path_buf(),
                        commands,
                        to_front_end,
                        &info,
                        open,
                    )),
                }
            };
            tasks
                .run(async {
                    while let Some(command) = next_command(commands, &cx).await? {
                        let (key, command) = match command {
                            Command::NewSession(key) => {
                                if let Entry::Vacant(entry) = sessions.entry(key) {
                                    entry.insert(start(key, Open::New { now: true }));
                                }
                                continue;
                            }
                            Command::LoadSession(key, id, cwd) => {
                                if let Entry::Vacant(entry) = sessions.entry(key) {
                                    entry.insert(start(key, Open::Load(id.into(), cwd)));
                                }
                                continue;
                            }
                            Command::Prompt(key, prompt) => (key, SessionCommand::Prompt(prompt)),
                            Command::Cancel(key) => (key, SessionCommand::Cancel),
                            Command::ReadContext(key) => (key, SessionCommand::ReadContext),
                            Command::ChangeSetting(key, setting, value) => {
                                (key, SessionCommand::ChangeSetting(setting, value))
                            }
                            Command::CloseSession(key) => {
                                if let Some(session) = sessions.remove(&key) {
                                    session.close().detach();
                                }
                                continue;
                            }
                            Command::DeleteSession(key, id) => {
                                let closing = sessions.remove(&key).map(SessionTask::close);
                                let (cx, events, deletes) =
                                    (cx.clone(), events.clone(), info.deletes);
                                // After the session's task has asked the agent to close it,
                                // so the delete doesn't overtake the close.
                                tasks
                                    .spawn(async move {
                                        if let Some(closing) = closing {
                                            closing.await;
                                        }
                                        delete(&cx, key, id.into(), deletes, events);
                                    })
                                    .detach();
                                continue;
                            }
                        };
                        let session = match sessions.entry(key) {
                            Entry::Occupied(entry) => entry.into_mut(),
                            // No session, so nothing is running to stop, nor a context to read.
                            Entry::Vacant(_)
                                if matches!(
                                    command,
                                    SessionCommand::Cancel | SessionCommand::ReadContext
                                ) =>
                            {
                                continue;
                            }
                            // Waits for its first prompt to create the session.
                            Entry::Vacant(entry) => {
                                entry.insert(start(key, Open::New { now: false }))
                            }
                        };
                        let _ = session.queue.try_send(command);
                    }
                    Ok(())
                })
                .await
        })
        .await
        .map_err(|err| describe_error(err, &info.login_options))
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

/// How a session's task has the agent make its session.
enum Open {
    /// With `session/new`, at once, or once its first prompt comes.
    New { now: bool },
    /// With `session/load`, at once: one the agent created in an earlier run, by its
    /// id, in the folder it was created in.
    Load(SessionId, PathBuf),
}

impl Open {
    /// The request that makes the session, for the timings.
    fn request(&self) -> &'static str {
        match self {
            Open::New { .. } => "session/new",
            Open::Load(..) => "session/load",
        }
    }
}

/// Has the agent make the session as `open` says, then runs its prompts one at a time,
/// as they arrive, until the front end closes it. A session the agent failed to make is
/// made again, the same way, by its next prompt.
async fn run_session(
    cx: ConnectionTo<Agent>,
    cwd: PathBuf,
    commands: Receiver<SessionCommand>,
    events: SessionEvents,
    agent: &AgentInfo,
    open: Open,
) {
    let key = events.key;
    // The prompt to run once the session exists, when a prompt is creating it.
    let mut retry = None;
    // Setting changes made while there was no session to make them to, which the
    // session gets before the prompt that creates it.
    let mut changes_waiting = Vec::new();
    if let Open::New { now: false } = open {
        let Some(prompt) = creating_prompt(&commands, &mut changes_waiting).await else {
            return;
        };
        retry = Some(prompt);
    }
    loop {
        let asked = Instant::now();
        let opened = match &open {
            // `run_until` sends `session/new` from this task, so a refusal comes back
            // here; `SessionBuilder::start_session` sends it from a connection task,
            // where an error ends the whole connection.
            Open::New { .. } => {
                cx.build_session(&cwd)
                    .block_task()
                    .run_until(async |mut session| {
                        tracing::info!(session = key.0, took_ms = millis(asked), "session/new");
                        let (prompt, changes) = (retry.take(), &mut changes_waiting);
                        run_opened(&mut session, prompt, changes, &commands, &events, agent).await;
                        Ok(())
                    })
                    .await
            }
            Open::Load(..) if !agent.loads => Err(Error::new(
                ErrorCode::MethodNotFound.into(),
                "the agent can't load sessions from earlier runs",
            )),
            // Unlike `SessionBuilder::start_session`, this sends `session/load` from this
            // task.
            Open::Load(id, folder) => {
                match cx
                    .load_session(id.clone(), folder)
                    .block_task()
                    .start_session()
                    .await
                {
                    Ok(loaded) => {
                        tracing::info!(session = key.0, took_ms = millis(asked), "session/load");
                        let mut session = loaded.into_session();
                        replay(&mut session, &events).await;
                        let (prompt, changes) = (retry.take(), &mut changes_waiting);
                        run_opened(&mut session, prompt, changes, &commands, &events, agent).await;
                        Ok(())
                    }
                    Err(err) => Err(err),
                }
            }
        };
        // `run_opened` returns only once the front end has closed the session.
        let Err(err) = opened else { return };
        let message = describe_error(err, &agent.login_options);
        tracing::warn!(
            session = key.0,
            took_ms = millis(asked),
            error = %message,
            "{} failed",
            open.request()
        );
        let failed = match retry {
            None => Event::SessionFailed(key, message),
            Some(_) => Event::Session(key, SessionEvent::TurnFailed(message)),
        };
        events.send(failed);
        // Returns at once if the session was closed while the agent was refusing it.
        let Some(prompt) = creating_prompt(&commands, &mut changes_waiting).await else {
            return;
        };
        retry = Some(prompt);
    }
}

/// Runs the session the agent has just created or loaded until the front end closes
/// it, then has the agent close it. `prompt` is the one that had it made, if one did,
/// and `changes_waiting` the setting changes made while it didn't exist.
async fn run_opened(
    session: &mut ActiveSession<'_, Agent>,
    prompt: Option<Prompt>,
    changes_waiting: &mut Vec<(String, SettingValue)>,
    commands: &Receiver<SessionCommand>,
    events: &SessionEvents,
    agent: &AgentInfo,
) {
    let key = events.key;
    if commands.is_closed() {
        // Closed while the agent was making it.
        close(key, session, agent.closes);
        return;
    }
    events.emit(SessionEvent::Created(session.session_id().0.to_string()));
    let settings = settings_of(session.config_options().unwrap_or_default());
    // A level the front end picked before the session existed goes out below.
    let reported = reported_thought_level(&settings)
        .filter(|(id, _)| !changes_waiting.iter().any(|(setting, _)| setting == id));
    events.emit(SessionEvent::Settings(settings));
    let waiting = Waiting {
        prompts: prompt.into_iter().collect(),
        read_context: false,
    };
    if waiting.prompts.is_empty() {
        events.send(Event::SessionReady(key));
    }
    // Only a closed connection fails these, which ends the session anyway.
    let mut changes = Changes::default();
    if let Some((setting, value)) = reported {
        let by = AskedBy::Core;
        let _ = queue_change(session, setting, value, by, events, &mut changes);
    }
    for (setting, value) in std::mem::take(changes_waiting) {
        let _ = change_setting(session, setting, value, events, &mut changes);
    }
    serve(session, waiting, changes, commands, events, agent).await;
    close(key, session, agent.closes);
}

/// Sends on the transcript the agent replayed as it loaded the session. The replay
/// comes before the answer to `session/load`, so by now it has all come in.
async fn replay(session: &mut ActiveSession<'_, Agent>, events: &SessionEvents) {
    // None is expected while loading; any that came is answered as cancelled as this drops.
    let mut permissions = Vec::new();
    while let Some(update) = session.read_update().now_or_never() {
        // Only a closed connection fails these, which ends the session anyway.
        let Ok(message) = update else { break };
        if let SessionMessage::SessionMessage(dispatch) = message {
            let _ = handle(dispatch, events, &mut permissions, true, None).await;
        }
    }
}

/// Waits, while there is no session, for the prompt that is to create it, keeping the
/// setting changes made meanwhile in `changes`. `None` once the front end has gone.
async fn creating_prompt(
    commands: &Receiver<SessionCommand>,
    changes: &mut Vec<(String, SettingValue)>,
) -> Option<Prompt> {
    loop {
        match commands.recv().await {
            Ok(SessionCommand::Prompt(prompt)) => return Some(prompt),
            // No session, so nothing is running to stop, nor a context to read.
            Ok(SessionCommand::Cancel | SessionCommand::ReadContext) => {}
            Ok(SessionCommand::ChangeSetting(setting, value)) => changes.push((setting, value)),
            Err(_) => return None,
        }
    }
}

/// Runs the session's prompts one at a time, as they arrive, until the front end closes
/// the session. Prompts sent during a turn wait in `waiting` for their own, as does a
/// `ReadContext`, and every prompt waits for the agent to answer the setting changes sent
/// before it, those already in `changes` included.
async fn serve(
    session: &mut ActiveSession<'_, Agent>,
    mut waiting: Waiting,
    mut changes: Changes,
    commands: &Receiver<SessionCommand>,
    events: &SessionEvents,
    agent: &AgentInfo,
) {
    // Off once the connection has gone, which the next prompt then reports.
    let mut reading = true;
    loop {
        let turn = match waiting.next() {
            Some(turn) => turn,
            None => match between_turns(session, commands, events, &mut reading).await {
                Some(SessionCommand::Prompt(prompt)) => Turn::Prompt(prompt),
                Some(SessionCommand::ReadContext) => Turn::ReadContext,
                // Nothing is running, so there is nothing to stop.
                Some(SessionCommand::Cancel) => continue,
                Some(SessionCommand::ChangeSetting(setting, value)) => {
                    // Only a closed connection fails this, which ends the session anyway.
                    let _ = change_setting(session, setting, value, events, &mut changes);
                    continue;
                }
                None => return,
            },
        };
        let reads_context = matches!(turn, Turn::ReadContext);
        // Sent to an agent without the command, it would go to the model as a prompt.
        if reads_context && !events.offers_context() {
            events.emit(SessionEvent::ContextReport(None));
            continue;
        }
        // The agent may still be applying a change, and a prompt it took first would
        // run with the setting as it was.
        let waited = wait_for_changes(session, &mut waiting, commands, events, &mut changes).await;
        match waited {
            Waited::Answered => {}
            // Stopped before it was sent, so it never runs. Before a context read, the
            // stop is meant for a prompt sent meanwhile, if one was.
            Waited::Stopped => {
                let stops_prompt = match turn {
                    Turn::Prompt(_) => true,
                    Turn::ReadContext => {
                        events.emit(SessionEvent::ContextReport(None));
                        waiting.prompts.pop_front().is_some()
                    }
                };
                if stops_prompt {
                    let stop_reason = format!("{:?}", StopReason::Cancelled);
                    events.emit(SessionEvent::TurnEnded { stop_reason });
                }
                continue;
            }
            Waited::Gone => return,
        }
        let prompt = match turn {
            Turn::Prompt(prompt) => prompt,
            // A prompt sent meanwhile goes first.
            Turn::ReadContext if !waiting.prompts.is_empty() => {
                waiting.read_context = true;
                continue;
            }
            Turn::ReadContext => Prompt::from(format!("/{CONTEXT_COMMAND}")),
        };
        // Read as it goes out, off this thread, which every session runs on.
        let images = agent.images;
        let content = smol::unblock(move || prompt_blocks(prompt, images)).await;
        let mut report = String::new();
        let into = reads_context.then_some(&mut report);
        let answer = run_turn(
            session,
            content,
            into,
            commands,
            &mut waiting,
            &mut changes,
            events,
        )
        .await;
        // Closed during the turn, which was stopped; prompts sent during it never run.
        if commands.is_closed() {
            return;
        }
        if reads_context {
            let report = match answer {
                Ok(response) if response.stop_reason == StopReason::EndTurn => {
                    Some(report).filter(|report| !report.trim().is_empty())
                }
                // Stopped for a prompt.
                Ok(_) => None,
                Err(err) => {
                    tracing::warn!(session = events.key.0, error = %err, "/context failed");
                    None
                }
            };
            events.emit(SessionEvent::ContextReport(report));
            continue;
        }
        match answer {
            Ok(response) => {
                if let Some(usage) = response.usage {
                    events.emit(SessionEvent::TurnUsage(turn_usage(usage)));
                }
                let stop_reason = format!("{:?}", response.stop_reason);
                events.emit(SessionEvent::TurnEnded { stop_reason });
            }
            Err(err) => {
                let message = describe_error(err, &agent.login_options);
                events.emit(SessionEvent::TurnFailed(message));
            }
        }
    }
}

/// Waits between turns for the front end's next command, passing on meanwhile what the
/// agent sends outside a turn, such as the title it makes once the first turn is over.
/// `None` once the front end has gone. `reading` is turned off when the connection has.
async fn between_turns(
    session: &mut ActiveSession<'_, Agent>,
    commands: &Receiver<SessionCommand>,
    events: &SessionEvents,
    reading: &mut bool,
) -> Option<SessionCommand> {
    enum Next {
        // Boxed, as an update is far larger than a command.
        Update(Box<Result<SessionMessage, Error>>),
        Command(Option<SessionCommand>),
    }
    loop {
        let open = *reading;
        let update = async {
            if !open {
                return smol::future::pending().await;
            }
            Next::Update(Box::new(session.read_update().await))
        };
        let command = async { Next::Command(commands.recv().await.ok()) };
        let update = match smol::future::or(update, command).await {
            Next::Command(command) => return command,
            Next::Update(update) => *update,
        };
        match update {
            Ok(SessionMessage::SessionMessage(dispatch)) => {
                // No turn is running to show a permission request in, so one that comes
                // is answered as cancelled as this drops.
                let mut permissions = Vec::new();
                let _ = handle(dispatch, events, &mut permissions, false, None).await;
            }
            Ok(_) => {}
            Err(_) => *reading = false,
        }
    }
}

/// Asks the agent to close the session, if `closes` says it can, so it frees what it
/// holds for it: Claude's adapter keeps a Claude Code process for each session until
/// then. The answer is only logged, as nothing more of the session is read.
fn close(key: SessionKey, session: &ActiveSession<'_, Agent>, closes: bool) {
    if !closes {
        return;
    }
    let asked = Instant::now();
    let request = CloseSessionRequest::new(session.session_id().clone());
    // Only a closed connection fails this, and then the agent has let go of everything.
    let _ = session
        .connection()
        .prepare_request(request)
        .on_receiving_result(async move |result| {
            match result {
                Ok(_) => tracing::info!(session = key.0, took_ms = millis(asked), "session/close"),
                Err(err) => tracing::warn!(
                    session = key.0,
                    took_ms = millis(asked),
                    error = %err,
                    "session/close failed"
                ),
            }
            Ok(())
        });
}

/// Asks the agent to delete the session, if `deletes` says it can. Its answer is only
/// logged, but for a failure, which shows as an error: the session is gone from the
/// front end, and the user would take it as deleted.
fn delete(
    cx: &ConnectionTo<Agent>,
    key: SessionKey,
    id: SessionId,
    deletes: bool,
    events: Sender<Event>,
) {
    if !deletes {
        return;
    }
    let asked = Instant::now();
    // Only a closed connection fails this, and the front end hears of that anyway.
    let _ = cx
        .prepare_request(DeleteSessionRequest::new(id))
        .on_receiving_result(async move |result| {
            match result {
                Ok(_) => tracing::info!(session = key.0, took_ms = millis(asked), "session/delete"),
                Err(err) => {
                    tracing::warn!(
                        session = key.0,
                        took_ms = millis(asked),
                        error = %err,
                        "session/delete failed"
                    );
                    emit(
                        &events,
                        Event::Error(format!("couldn't delete the session: {err}")),
                    );
                }
            }
            Ok(())
        });
}

/// The image types Claude takes as pixels, which Codex takes too.
const INLINE_IMAGE_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];
/// 3.75 MB is 5 MB as base64, the most Claude takes for one image on Bedrock, the
/// strictest of the places it runs.
const INLINE_IMAGE_MAX_BYTES: u64 = 3_750_000;

/// The prompt as the agent takes it: the files put with it, then its text, as images
/// read best before the text that asks about them. Reads the files, so it blocks.
fn prompt_blocks(prompt: Prompt, images: bool) -> Vec<ContentBlock> {
    let mut blocks: Vec<ContentBlock> = prompt
        .attachments
        .iter()
        .map(|attachment| attachment_block(attachment, images))
        .collect();
    // A prompt of no text and no files still goes as its empty text.
    if !prompt.text.is_empty() || blocks.is_empty() {
        blocks.push(prompt.text.into());
    }
    blocks
}

/// A file put with a prompt. An image goes as its pixels when the agent takes images
/// (`images`) and the image is of a type and size Claude takes; anything else, or an
/// image that can't be read, goes as a link to the file, which the agent opens with its
/// own tools. Both adapters pass a link on as `[@name](file:///...)` in the text.
fn attachment_block(attachment: &Attachment, images: bool) -> ContentBlock {
    let (path, mime) = match attachment {
        Attachment::Image { path, mime } => (path, Some(mime)),
        Attachment::File { path } => (path, None),
    };
    let uri = file_uri(path);
    // None for a folder, and for a file gone since it was put with the prompt.
    let size = std::fs::metadata(path)
        .ok()
        .filter(|meta| meta.is_file())
        .map(|meta| meta.len());
    let inline = images
        && mime.is_some_and(|mime| INLINE_IMAGE_TYPES.contains(&mime.as_str()))
        && size.is_some_and(|size| size <= INLINE_IMAGE_MAX_BYTES);
    if let Some(mime) = mime.filter(|_| inline)
        && let Ok(bytes) = std::fs::read(path)
    {
        let image = ImageContent::new(BASE64_STANDARD.encode(bytes), mime).uri(uri);
        return ContentBlock::Image(image);
    }
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let link = ResourceLink::new(name, uri)
        .mime_type(mime.cloned())
        .size(size.and_then(|size| i64::try_from(size).ok()));
    ContentBlock::ResourceLink(link)
}

/// `path` as a `file://` URL, with spaces and the like escaped. The front ends send
/// only whole paths, so a relative one goes as it is.
fn file_uri(path: &Path) -> String {
    Url::from_file_path(path).map_or_else(|()| path.display().to_string(), String::from)
}

/// Asks the agent to stop the session's turn; the prompt is answered with `Cancelled`.
fn cancel_turn(session: &ActiveSession<'_, Agent>) -> Result<(), Error> {
    let id = session.session_id().clone();
    session
        .connection()
        .send_notification(CancelNotification::new(id))
}

/// Sends one prompt and forwards the session's updates until the agent answers it. The
/// agent's text goes to `report` instead, when there is one, for a `ReadContext`.
/// The prompt is sent by hand instead of with `ActiveSession::send_prompt`, whose
/// failure would end the whole connection along with every other session.
async fn run_turn(
    session: &mut ActiveSession<'_, Agent>,
    content: Vec<ContentBlock>,
    mut report: Option<&mut String>,
    commands: &Receiver<SessionCommand>,
    waiting: &mut Waiting,
    changes: &mut Changes,
    events: &SessionEvents,
) -> Result<PromptResponse, Error> {
    let key = events.key;
    let reads_context = report.is_some();
    enum Next {
        Update(Result<SessionMessage, Error>),
        Answered(Result<Result<PromptResponse, Error>, oneshot::Canceled>),
        Chosen(usize, Option<String>),
        Command(Option<SessionCommand>),
    }
    let (answer_tx, mut answer) = oneshot::channel();
    session
        .connection()
        .prepare_request(PromptRequest::new(session.session_id().clone(), content))
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
        if commands.is_closed() {
            // Closed. An agent that takes `session/close` stops the turn on it too,
            // but one that doesn't hears of it only this way. The open permission
            // requests are answered as cancelled as `permissions` drops. Checked
            // before the updates, which are read first below while any are waiting.
            let _ = cancel_turn(session);
            tracing::info!(
                session = key.0,
                took_ms = millis(sent),
                first_update_ms,
                updates,
                "session/prompt closed"
            );
            return Err(internal_error("the session was closed"));
        }
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
                    let report = report.as_deref_mut();
                    handle(dispatch, events, &mut permissions, false, report).await?;
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
                    reads_context,
                    "session/prompt"
                );
                return answer.map_err(|_| internal_error("the prompt was dropped"))?;
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
                // A stop during a context read is meant for the prompt that stopped it,
                // which waits behind it and so never runs.
                if reads_context && waiting.prompts.pop_front().is_some() {
                    let stop_reason = format!("{:?}", StopReason::Cancelled);
                    events.emit(SessionEvent::TurnEnded { stop_reason });
                    continue;
                }
                stopped.get_or_insert_with(Instant::now);
                cancel_turn(session)?;
                // ACP has the client answer every open permission request as cancelled.
                for permission in permissions.drain(..) {
                    permission.answer(None)?;
                }
            }
            Next::Command(Some(SessionCommand::Prompt(prompt))) => {
                waiting.prompts.push_back(prompt);
                // A context read never holds up a prompt.
                if reads_context && stopped.is_none() {
                    stopped = Some(Instant::now());
                    cancel_turn(session)?;
                }
            }
            Next::Command(Some(SessionCommand::ChangeSetting(setting, value))) => {
                change_setting(session, setting, value, events, changes)?
            }
            // The one running answers it, if this turn is one.
            Next::Command(Some(SessionCommand::ReadContext)) => {
                waiting.read_context |= !reads_context;
            }
            // Closed; the check at the top of the loop stops the turn.
            Next::Command(None) => {}
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
    asked_by: AskedBy,
    /// Set by whoever sends it, so it goes out once.
    sent: bool,
    /// Told when the agent answers.
    answered: oneshot::Sender<()>,
}

/// Who asked for a setting change. The front end counts the answers to its own changes
/// to know when a setting shows the agent's value again, so acp-core's changes must not
/// answer as one of its.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AskedBy {
    FrontEnd,
    /// acp-core, setting a value back to what the agent reported; see
    /// `reported_thought_level`.
    Core,
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
    session: &ActiveSession<'_, Agent>,
    waiting: &mut Waiting,
    commands: &Receiver<SessionCommand>,
    events: &SessionEvents,
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
                    if change.asked_by == AskedBy::Core {
                        // Nothing the user did failed; the setting shows as reported.
                        tracing::warn!(session = events.key.0, setting = %change.setting, "the agent didn't answer the reported value set back");
                        continue;
                    }
                    let message = format!(
                        "couldn't change {}: the agent didn't answer within {} s",
                        change.setting,
                        SETTING_ANSWER_TIMEOUT.as_secs()
                    );
                    let setting = change.setting;
                    events.emit(SessionEvent::SettingFailed { setting, message });
                }
                changes.last = None;
            }
            Next::Command(Some(SessionCommand::Cancel)) => return Waited::Stopped,
            Next::Command(Some(SessionCommand::Prompt(prompt))) => {
                waiting.prompts.push_back(prompt)
            }
            Next::Command(Some(SessionCommand::ReadContext)) => waiting.read_context = true,
            Next::Command(Some(SessionCommand::ChangeSetting(setting, value))) => {
                // Only a closed connection fails this, which ends the session anyway.
                let _ = change_setting(session, setting, value, events, changes);
            }
            Next::Command(None) => return Waited::Gone,
        }
    }
    // The last answer and the close can come in the same poll, and the answer wins above.
    if commands.is_closed() {
        return Waited::Gone;
    }
    Waited::Answered
}

/// Asks the agent to change one of the session's settings for the front end, once it
/// has answered the changes asked before, for the next prompt to wait on. The answer,
/// the settings as they then stand, goes to the front end.
fn change_setting(
    session: &ActiveSession<'_, Agent>,
    setting: String,
    value: SettingValue,
    events: &SessionEvents,
    changes: &mut Changes,
) -> Result<(), Error> {
    let by = AskedBy::FrontEnd;
    queue_change(session, setting, value, by, events, changes)
}

/// Asks the agent to change one of the session's settings, for whoever `asked_by` says,
/// once it has answered the changes asked before.
fn queue_change(
    session: &ActiveSession<'_, Agent>,
    setting: String,
    value: SettingValue,
    asked_by: AskedBy,
    events: &SessionEvents,
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
            asked_by,
            sent: false,
            answered,
        });
    }
    send_change(
        session.connection().clone(),
        session.session_id().clone(),
        events.clone(),
        changes.queue.clone(),
    )
}

/// Sends the first of the session's waiting setting changes unless it is out already,
/// and once the agent answers it, the next.
fn send_change(
    connection: ConnectionTo<Agent>,
    session_id: SessionId,
    events: SessionEvents,
    queue: ChangeQueue,
) -> Result<(), Error> {
    // Nothing more goes to the agent for a closed session; what waits goes with it.
    if events.closed() {
        return Ok(());
    }
    let key = events.key;
    // Claimed under the lock, so a change that two callers reach goes out once.
    let first = queue.lock().changes.front_mut().and_then(|change| {
        (!change.sent).then(|| {
            change.sent = true;
            let (setting, value) = (change.setting.clone(), change.value.clone());
            (change.id, setting, value, change.asked_by)
        })
    });
    let Some((id, setting, value, asked_by)) = first else {
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
                    let settings = settings_of(&response.config_options);
                    Some(match asked_by {
                        AskedBy::FrontEnd => SessionEvent::SettingChanged { setting, settings },
                        // The front end has no change of its own waiting on this answer.
                        AskedBy::Core => SessionEvent::Settings(settings),
                    })
                }
                Err(err) => {
                    tracing::warn!(
                        session = key.0,
                        took_ms,
                        %setting,
                        error = %err,
                        "session/set_config_option failed"
                    );
                    // Nothing the user did failed when acp-core's own change does.
                    (asked_by == AskedBy::FrontEnd).then(|| SessionEvent::SettingFailed {
                        message: format!("couldn't change {setting}: {err}"),
                        setting,
                    })
                }
            };
            if let Some(event) = event {
                events.emit(event);
            }
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
                let _ = send_change(connection, session_id, events, queue);
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

/// Takes one message the agent sent for the session; `replaying` while it loads. The
/// agent's text goes to `report` instead, when there is one.
async fn handle(
    dispatch: Dispatch,
    events: &SessionEvents,
    permissions: &mut Vec<PendingPermission>,
    replaying: bool,
    report: Option<&mut String>,
) -> Result<(), Error> {
    MatchDispatch::new(dispatch)
        .if_notification(async |notification: SessionNotification| {
            if let SessionUpdate::AvailableCommandsUpdate(update) = &notification.update {
                events.note_commands(&update.available_commands);
            }
            match (flatten(notification.update, replaying), report) {
                (Some(SessionEvent::AgentText(text)), Some(report)) => report.push_str(&text),
                (Some(event), _) => events.emit(event),
                (None, _) => {}
            }
            Ok(())
        })
        .await
        .if_request(async |request: RequestPermissionRequest, responder| {
            permissions.push(ask_permission(request, responder, events));
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
    events: &SessionEvents,
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
            always: matches!(
                option.kind,
                PermissionOptionKind::AllowAlways | PermissionOptionKind::RejectAlways
            ),
        })
        .collect();
    let tool_call = request.tool_call.tool_call_id.0.to_string();
    let fields = request.tool_call.fields.clone();
    let title = fields.title.clone().unwrap_or_else(|| "tool call".into());
    events.emit(SessionEvent::ToolCallUpdate {
        id: tool_call.clone(),
        title: fields.title,
        kind: fields.kind.map(ToolKind::from),
        status: fields.status.map(ToolStatus::from),
        content: fields.content.map(tool_content),
        locations: fields.locations.map(tool_locations),
    });
    events.emit(SessionEvent::Permission(PermissionRequest {
        tool_call,
        title,
        options,
        respond: Some(respond),
    }));
    PendingPermission {
        asked: Instant::now(),
        options: request.options,
        responder: Some(responder),
        choice,
    }
}

fn flatten(update: SessionUpdate, replaying: bool) -> Option<SessionEvent> {
    Some(match update {
        // During a turn the front end has the prompt already, and an echo of it would
        // show twice.
        SessionUpdate::UserMessageChunk(chunk) if replaying => {
            SessionEvent::UserText(text_of(&chunk.content))
        }
        SessionUpdate::AgentMessageChunk(chunk) => SessionEvent::AgentText(text_of(&chunk.content)),
        SessionUpdate::AgentThoughtChunk(chunk) => {
            SessionEvent::AgentThought(text_of(&chunk.content))
        }
        SessionUpdate::ToolCall(call) => SessionEvent::ToolCall {
            id: call.tool_call_id.0.to_string(),
            title: call.title,
            kind: call.kind.into(),
            status: call.status.into(),
            content: tool_content(call.content),
            locations: tool_locations(call.locations),
        },
        SessionUpdate::ToolCallUpdate(update) => SessionEvent::ToolCallUpdate {
            id: update.tool_call_id.0.to_string(),
            title: update.fields.title,
            kind: update.fields.kind.map(ToolKind::from),
            status: update.fields.status.map(ToolStatus::from),
            content: update.fields.content.map(tool_content),
            locations: update.fields.locations.map(tool_locations),
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
        // Both adapters also send this update with only `_meta` in it. One without a
        // title, or clearing it, leaves the title as it was.
        SessionUpdate::SessionInfoUpdate(info) => match info.title {
            MaybeUndefined::Value(title) if !title.trim().is_empty() => SessionEvent::Title(title),
            _ => return None,
        },
        // A window of 0 says nothing, and Codex's adapter never sends one.
        SessionUpdate::UsageUpdate(usage) if usage.size > 0 => {
            SessionEvent::ContextUsage(ContextUsage {
                used: usage.used,
                size: usage.size,
            })
        }
        _ => return None,
    })
}

fn turn_usage(usage: Usage) -> TurnUsage {
    TurnUsage {
        input: usage.input_tokens,
        cached_read: usage.cached_read_tokens.unwrap_or(0),
        cached_write: usage.cached_write_tokens.unwrap_or(0),
        output: usage.output_tokens,
        thought: usage.thought_tokens.unwrap_or(0),
    }
}

/// The thought level the agent reports for a session it has just created or loaded, for
/// acp-core to set it to that same value. claude-agent-acp 0.94.0 only shows this value to a
/// client without JetBrains' `recommendedValue` extension: it reads it from Claude
/// Code's settings files, down to the old top-level `effortLevel`, which the CLI it
/// starts no longer reads. So the button said "Xhigh" while turns ran at the model's
/// default. Only `session/set_config_option` makes the CLI use a level, and the level
/// then stays when the session moves to another model that has it. "default" leaves
/// the level to the agent, so there is nothing to set.
fn reported_thought_level(settings: &[Setting]) -> Option<(String, SettingValue)> {
    let setting = settings
        .iter()
        .find(|setting| setting.kind == SettingKind::ThoughtLevel)?;
    match &setting.value {
        SettingValue::Choice(level) if level != "default" => {
            Some((setting.id.clone(), setting.value.clone()))
        }
        _ => None,
    }
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

/// A tool call's content as front ends show it. Terminals are left out: the client
/// doesn't offer them, so agents send a command's output as text instead.
fn tool_content(content: Vec<ToolCallContent>) -> Vec<ToolContent> {
    content
        .into_iter()
        .filter_map(|content| match content {
            ToolCallContent::Content(content) => Some(ToolContent::Text(text_of(&content.content))),
            ToolCallContent::Diff(diff) => Some(ToolContent::Diff {
                path: diff.path,
                old: diff.old_text,
                new: diff.new_text,
            }),
            _ => None,
        })
        .collect()
}

fn tool_locations(locations: Vec<ToolCallLocation>) -> Vec<ToolLocation> {
    locations
        .into_iter()
        .map(|location| ToolLocation {
            path: location.path,
            line: location.line,
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

/// `_auth/rate_limits_update`, an extension Claude's and Codex's adapters share until ACP
/// has a way to report rate limits. The agent advertises it with
/// `agentCapabilities._meta.rateLimits`, which needs no answer, so nothing here reads that.
#[derive(Clone, Debug, Serialize, Deserialize, JsonRpcNotification)]
#[notification(method = "_auth/rate_limits_update")]
#[serde(rename_all = "camelCase")]
struct RateLimitsUpdate {
    rate_limits: RateLimitsPayload,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RateLimitsPayload {
    windows: Vec<RateLimitsPayloadWindow>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RateLimitsPayloadWindow {
    id: String,
    label: String,
    used_percent: f64,
    /// ISO 8601.
    #[serde(default)]
    resets_at: Option<String>,
    #[serde(default)]
    window_minutes: Option<u32>,
    #[serde(default)]
    model: Option<String>,
}

impl RateLimitsUpdate {
    fn windows(self) -> Vec<RateLimitWindow> {
        self.rate_limits
            .windows
            .into_iter()
            .map(|window| RateLimitWindow {
                id: window.id,
                label: window.label,
                used_percent: window.used_percent.clamp(0., 100.),
                // A date it can't read is taken as not given.
                resets_at: window.resets_at.and_then(|at| {
                    chrono::DateTime::parse_from_rfc3339(&at)
                        .ok()
                        .map(SystemTime::from)
                }),
                window_minutes: window.window_minutes,
                model: window.model,
            })
            .collect()
    }
}

/// Whole milliseconds since `since`, for the timings sent to `tracing`.
fn millis(since: Instant) -> u64 {
    since.elapsed().as_millis() as u64
}

fn emit(events: &Sender<Event>, event: Event) {
    // A closed channel means the front end quit; nothing left to tell.
    let _ = events.send_blocking(event);
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// A folder of the test's own for the files it puts with a prompt.
    fn folder(test: &str) -> PathBuf {
        let folder = std::env::temp_dir().join(format!("acp-core-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir_all(&folder).unwrap();
        folder
    }

    fn image(path: &Path, mime: &str) -> Attachment {
        Attachment::Image {
            path: path.to_path_buf(),
            mime: mime.into(),
        }
    }

    fn link(block: &ContentBlock) -> &ResourceLink {
        match block {
            ContentBlock::ResourceLink(link) => link,
            _ => panic!("not a link"),
        }
    }

    #[test]
    fn sends_text_alone_as_one_block() {
        let blocks = prompt_blocks("hello".to_string().into(), true);
        assert_eq!(blocks.len(), 1);
        assert_eq!(text_of(&blocks[0]), "hello");
        let blocks = prompt_blocks(Prompt::default(), true);
        assert_eq!(blocks.len(), 1);
        assert_eq!(text_of(&blocks[0]), "");
    }

    #[test]
    fn sends_a_small_image_as_its_pixels_before_the_text() {
        let folder = folder("pixels");
        let path = folder.join("shot one.png");
        fs::write(&path, b"\x89PNG").unwrap();
        let prompt = Prompt {
            text: "what is this?".into(),
            attachments: vec![image(&path, "image/png")],
        };
        let blocks = prompt_blocks(prompt, true);
        assert_eq!(blocks.len(), 2);
        let ContentBlock::Image(sent) = &blocks[0] else {
            panic!("not an image");
        };
        assert_eq!(sent.data, "iVBORw==");
        assert_eq!(sent.mime_type, "image/png");
        let uri = sent.uri.as_deref().unwrap();
        assert!(uri.starts_with("file:///") && uri.ends_with("/shot%20one.png"));
        assert_eq!(text_of(&blocks[1]), "what is this?");
        // Nothing but the image: no empty text after it.
        let prompt = Prompt {
            text: String::new(),
            attachments: vec![image(&path, "image/png")],
        };
        assert_eq!(prompt_blocks(prompt, true).len(), 1);
    }

    #[test]
    fn sends_an_image_as_a_link_where_pixels_wont_do() {
        let folder = folder("image-links");
        let small = folder.join("small.png");
        fs::write(&small, b"\x89PNG").unwrap();
        let heic = folder.join("photo.heic");
        fs::write(&heic, b"heic").unwrap();
        let large = folder.join("large.png");
        fs::File::create(&large)
            .unwrap()
            .set_len(INLINE_IMAGE_MAX_BYTES + 1)
            .unwrap();
        let gone = folder.join("gone.png");
        // An agent that takes no images.
        let sent = attachment_block(&image(&small, "image/png"), false);
        assert_eq!(link(&sent).name, "small.png");
        assert_eq!(link(&sent).mime_type.as_deref(), Some("image/png"));
        assert_eq!(link(&sent).size, Some(4));
        // A type Claude doesn't take, and an image too large to send.
        let sent = attachment_block(&image(&heic, "image/heic"), true);
        assert_eq!(link(&sent).name, "photo.heic");
        let sent = attachment_block(&image(&large, "image/png"), true);
        assert_eq!(link(&sent).size, Some(INLINE_IMAGE_MAX_BYTES as i64 + 1));
        // Gone since it was put with the prompt.
        let sent = attachment_block(&image(&gone, "image/png"), true);
        assert_eq!(link(&sent).size, None);
        assert!(link(&sent).uri.ends_with("/gone.png"));
    }

    #[test]
    fn sends_files_and_folders_as_links() {
        let folder = folder("file-links");
        let file = folder.join("notes.txt");
        fs::write(&file, b"hello").unwrap();
        let sent = attachment_block(&Attachment::File { path: file }, true);
        assert_eq!(link(&sent).name, "notes.txt");
        assert_eq!(link(&sent).mime_type, None);
        assert_eq!(link(&sent).size, Some(5));
        assert!(link(&sent).uri.starts_with("file:///"));
        let sent = attachment_block(
            &Attachment::File {
                path: folder.clone(),
            },
            true,
        );
        assert_eq!(link(&sent).size, None);
        assert!(link(&sent).name.ends_with("file-links"));
    }
}

//! The line under the prompt box: the folder the session works in, as a button that opens
//! the folder menu until the session's first prompt, the folders sessions were started in
//! that the menu lists, and what git says of the folder (see `crate::git`): its branch, as
//! a button that opens the branch menu while no agent replies in its worktree, a mark for a
//! linked worktree, what the branch has changed since it left its base branch, and the
//! files not committed yet.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use acp_core::{SessionEvent, SessionKey};
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::chat_view::ChatView;
use crate::chat_view::pickers::MenuLook;
use crate::chat_view::tool_call::ToolLook;
use crate::config::{change_recent_folders, config_folder, load_recent_folders, put_first};
use crate::git::{self, Base, Changes, Head};
use crate::session::{Origin, Session};

/// Gap between the prompt box and the line.
const FOLDER_LINE_GAP: Pixels = px(4.);
const FOLDER_LINE_HEIGHT: Pixels = px(28.);
/// From the box's edges, so the folder's icon is level with the model button's, 16px in.
const FOLDER_LINE_INSET: Pixels = px(6.);
/// Below the lines under the box, in place of the 12px around the box.
pub(super) const UNDER_BOX_BOTTOM_PAD: Pixels = px(8.);
/// The folder button: the size and padding of the model button, without its caret.
const FOLDER_ICON_SIZE: Pixels = px(14.);
const FOLDER_TEXT_SIZE: Pixels = px(13.);
const FOLDER_TEXT_LINE: Pixels = px(16.);
const FOLDER_RADIUS: Pixels = px(6.);
const FOLDER_NAME_MAX: Pixels = px(200.);
/// How narrow the button gets when the line is short of room, once the branch's name
/// has given all it can.
pub(super) const FOLDER_BUTTON_MIN: Pixels = px(96.);
/// What git says, a step smaller than the button.
const GIT_TEXT_SIZE: Pixels = px(12.);
const GIT_ICON_SIZE: Pixels = px(14.);
/// From the button to the first item, and between items.
const GIT_FIRST_GAP: Pixels = px(8.);
const GIT_ITEM_GAP: Pixels = px(14.);
/// Between an item's icon and its words.
const GIT_PART_GAP: Pixels = px(6.);
const BRANCH_NAME_MAX: Pixels = px(240.);
const BRANCH_NAME_MIN: Pixels = px(40.);
/// Inside the branch, a button for its menu, around its icon and name. The gaps either side
/// give it back, so the branch sits where it would bare.
const BRANCH_PAD: Pixels = px(6.);
/// The dot before the count of files not committed.
const DIRTY_DOT_SIZE: Pixels = px(6.);
/// How long the agent's events must have stopped changing files before git is asked
/// again: a turn finishes calls in bursts, and one read after the burst does.
pub(super) const GIT_SETTLE: Duration = Duration::from_millis(500);

impl ChatView {
    /// Asks git again about the folder of the session `key`, after `wait`. The line
    /// shows the branch as soon as git gives it, and the rest once git has worked it out;
    /// what it showed before stays until then, so it doesn't blink.
    pub(super) fn read_git(&mut self, key: SessionKey, wait: Duration, cx: &mut Context<Self>) {
        let Some(session) = self.session_mut(key) else {
            return;
        };
        let folder = session.folder().to_path_buf();
        session.git_read = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let read = folder.clone();
            let repo = cx
                .background_spawn(async move { git::read_head(&read) })
                .await;
            let head = repo.as_ref().map(|repo| repo.head.clone());
            let shown = this.update(cx, |this, cx| {
                let Some(session) = this.session_mut(key) else {
                    return;
                };
                let last = session.git.take();
                session.git = repo.map(|mut repo| {
                    repo.changes = last
                        .filter(|last| last.head == repo.head)
                        .and_then(|last| last.changes);
                    repo
                });
                cx.notify();
            });
            let (Ok(()), Some(head)) = (shown, head) else {
                return;
            };
            let changes = cx
                .background_spawn(async move { git::read_changes(&folder, &head) })
                .await;
            let _ = this.update(cx, |this, cx| {
                let repo = this
                    .session_mut(key)
                    .and_then(|session| session.git.as_mut());
                if let Some(repo) = repo {
                    repo.changes = changes;
                    cx.notify();
                }
            });
        }));
    }

    /// Opens the system's folder panel at the folder of the session `key`, and has the
    /// session work in the folder picked there, which the folder menu lists from then on.
    pub(super) fn pick_folder(
        &mut self,
        key: SessionKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session(key) else {
            return;
        };
        let start = session.folder().to_path_buf();
        let picked = choose_folder(start, window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let Some(folder) = picked.await else {
                return;
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.set_folder(key, folder.clone(), window, cx);
                this.remember_folder(folder);
            });
        })
        .detach();
    }

    /// Takes up the folder menu's list from disk, or the first time there is none, starts
    /// it from the folders of the sessions kept, newest first.
    pub(super) fn restore_recent_folders(&mut self) {
        if let Some(folders) = load_recent_folders() {
            self.recent_folders = folders;
            return;
        }
        let mut kept: Vec<PathBuf> = Vec::new();
        // The sessions kept are oldest first.
        for session in self.sessions.iter().rev() {
            let folder = session.folder();
            if folder.is_dir() && !kept.iter().any(|known| known == folder) {
                kept.push(folder.to_path_buf());
            }
        }
        self.change_recent_folders(|folders| *folders = kept.clone());
    }

    /// Puts `folder` first in the folder menu, here and on disk.
    pub(super) fn remember_folder(&mut self, folder: PathBuf) {
        self.change_recent_folders(|folders| put_first(folders, &folder));
    }

    fn change_recent_folders(&mut self, change: impl Fn(&mut Vec<PathBuf>)) {
        let changed = config_folder()
            .ok_or_else(|| std::io::Error::other("there is no config folder"))
            .and_then(|folder| change_recent_folders(&folder, &change));
        match changed {
            Ok(folders) => self.recent_folders = folders,
            Err(err) => {
                eprintln!("couldn't save the recent folders: {err}");
                // The list still changes for this run.
                change(&mut self.recent_folders);
            }
        }
    }

    /// The session's folder and what git says of it.
    pub(super) fn render_folder_line(&self, session: &Session, cx: &mut Context<Self>) -> Div {
        let look = MenuLook::new(cx);
        // The same green and red as an edit's counts: the theme's are too light on white.
        let lines = ToolLook::new(cx);
        let folder = session.folder();
        let path = SharedString::from(tilde(folder));
        let key = session.key;
        // Once the agent has the session, it works in its folder for good: ACP has no
        // request to move a session.
        let pickable = session.title.is_none() && matches!(session.origin, Origin::New(_));
        // Locked, the button looks like plain text, so its tooltip says why it won't open.
        let tip = if pickable {
            path.clone()
        } else {
            SharedString::from(format!("{path}\nCan't change after the first prompt"))
        };
        let button = div()
            .id("folder")
            .aria_label(format!("Folder: {path}"))
            .flex()
            .flex_shrink(1.)
            .min_w(FOLDER_BUTTON_MIN)
            .h_full()
            .pl(px(10.))
            .pr(px(8.))
            .gap(GIT_PART_GAP)
            .items_center()
            .rounded(FOLDER_RADIUS)
            .text_size(FOLDER_TEXT_SIZE)
            .line_height(FOLDER_TEXT_LINE)
            .font_weight(FontWeight::MEDIUM)
            .text_color(look.foreground)
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .child(
                Icon::new(IconName::Folder)
                    .size(FOLDER_ICON_SIZE)
                    .text_color(look.muted)
                    .flex_none(),
            )
            .child(
                div()
                    .min_w(px(0.))
                    .max_w(FOLDER_NAME_MAX)
                    .truncate()
                    .child(folder_name(folder)),
            )
            .when(pickable, |this| {
                this.role(Role::Button)
                    .cursor_pointer()
                    .hover(|style| style.bg(look.hover))
                    .active(|style| style.bg(look.pressed))
            });
        let button = if pickable {
            self.render_folder_picker(key, folder, button, cx)
        } else {
            button.into_any_element()
        };

        let mut branch = None;
        let mut items = Vec::new();
        if let Some(repo) = &session.git {
            let (current, detached) = match &repo.head {
                Head::Branch(name) => (Some(name.as_str()), false),
                Head::Detached(_) => (None, true),
            };
            let shown = match &repo.head {
                Head::Branch(name) => item("git-branch", IconName::GitBranch, &look)
                    .aria_label(format!("Branch: {name}"))
                    .child(
                        div()
                            .min_w(px(0.))
                            .truncate()
                            .text_color(look.foreground)
                            .child(name.clone()),
                    ),
                Head::Detached(hash) => item("git-branch", IconName::GitCommitHorizontal, &look)
                    .aria_label(format!("Detached at {hash}"))
                    .child(div().text_color(look.foreground).child(hash.clone()))
                    .child("detached"),
            }
            .h_full()
            .px(BRANCH_PAD)
            .rounded(FOLDER_RADIUS);
            // Not while an agent works anywhere in the worktree, this session's or another's:
            // the switch would change the files under it.
            let working = self.working_in(&repo.root);
            let switchable = !session.busy
                && working.is_none()
                && repo
                    .branches
                    .iter()
                    .any(|name| Some(name.as_str()) != current);
            branch = Some(if switchable {
                let shown = shown
                    .role(Role::Button)
                    .min_w(px(0.))
                    .cursor_pointer()
                    .hover(|style| style.bg(look.hover))
                    .active(|style| style.bg(look.pressed));
                let place = branch_place(StyleRefinement::default().flex(), detached);
                self.render_branch_picker(key, &repo.branches, current, shown, place, cx)
            } else {
                // Another session's turn locks it with no sign of that here, so its
                // tooltip says which; this session's own turn needs no telling.
                let tip = working
                    .filter(|_| !session.busy)
                    .map(|other| match &other.title {
                        Some(title) => SharedString::from(format!(
                            "Can't switch while \u{201c}{title}\u{201d} is replying"
                        )),
                        None => {
                            SharedString::from("Can't switch while another session is replying")
                        }
                    });
                let shown = match tip {
                    Some(tip) => {
                        shown.tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    }
                    None => shown,
                };
                branch_place(shown, detached).into_any_element()
            });
            if let Some(main) = &repo.worktree_of {
                let tip = SharedString::from(format!("Worktree of {}", tilde(main)));
                items.push(
                    item("git-worktree", IconName::Folders, &look)
                        .flex_none()
                        .child("worktree")
                        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx)),
                );
            }
            if let Some(changes) = &repo.changes {
                items.extend(base_item(changes, &look, lines));
                items.extend(status_item(changes, &look));
            }
        }

        div()
            .mt(FOLDER_LINE_GAP)
            .h(FOLDER_LINE_HEIGHT)
            .px(FOLDER_LINE_INSET)
            .flex()
            .items_center()
            .overflow_hidden()
            .whitespace_nowrap()
            .child(button)
            .children(branch)
            .children(items.into_iter().enumerate().map(|(ix, item)| {
                // The branch's padding comes out of the gap after it as well.
                let gap = if ix == 0 {
                    GIT_ITEM_GAP - BRANCH_PAD
                } else {
                    GIT_ITEM_GAP
                };
                item.ml(gap)
            }))
    }

    /// The session whose turn is running in a folder of the worktree at `root`, if any.
    fn working_in(&self, root: &Path) -> Option<&Session> {
        self.sessions
            .iter()
            .find(|session| session.is_live() && in_worktree(session, root))
    }

    /// Switches the folder of the session `key` to `branch`, then has git asked again about
    /// every session in that worktree. What git says when it refuses, such as changes not
    /// committed that the switch would overwrite, goes in the session's transcript.
    pub(super) fn switch_branch(
        &mut self,
        key: SessionKey,
        branch: String,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session(key) else {
            return;
        };
        // Asked again, as a turn may have started since the menu opened.
        let Some(root) = session.git.as_ref().map(|repo| repo.root.clone()) else {
            return;
        };
        if session.busy || self.working_in(&root).is_some() {
            return;
        }
        let folder = session.folder().to_path_buf();
        cx.spawn(async move |this, cx| {
            let to = branch.clone();
            let switched = cx
                .background_spawn(async move { git::switch(&folder, &to) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Err(said) = switched
                    && let Some(session) = this.session_mut(key)
                {
                    session.push_error(format!("Couldn't switch to {branch}: {said}"), cx);
                }
                let shown: Vec<SessionKey> = this
                    .sessions
                    .iter()
                    .filter(|session| in_worktree(session, &root))
                    .map(|session| session.key)
                    .collect();
                for key in shown {
                    this.read_git(key, Duration::ZERO, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
}

/// Whether the session works in the worktree at `root`, from anywhere in it. Told by the
/// worktree git found for its folder, so a linked worktree kept inside the main one, as
/// `.claude/worktrees/` is, counts as its own; by its folder alone until git has said.
fn in_worktree(session: &Session, root: &Path) -> bool {
    match &session.git {
        Some(repo) => repo.root == root,
        None => session.folder().starts_with(root),
    }
}

/// Where the branch sits in the line: after the folder button, less its own padding, and
/// giving way first when the line is short of room, down to 40px of its name. A detached
/// commit's short hash never gives way. Laid on the branch itself, or on the menu's box
/// around it.
fn branch_place<S: Styled>(this: S, detached: bool) -> S {
    let this = this.ml(GIT_FIRST_GAP - BRANCH_PAD).h_full();
    if detached {
        return this.flex_none();
    }
    let around = GIT_ICON_SIZE + GIT_PART_GAP + BRANCH_PAD * 2.;
    this.flex_shrink(1000.)
        .min_w(around + BRANCH_NAME_MIN)
        .max_w(around + BRANCH_NAME_MAX)
}

/// The branch's changes since it left its base, or that it has none to compare with.
fn base_item(changes: &Changes, look: &MenuLook, lines: ToolLook) -> Option<Stateful<Div>> {
    let item = item("git-base", IconName::GitCompare, look).flex_none();
    let item = match changes.base.as_ref()? {
        Base::Missing => item.child("No base branch").tooltip(|window, cx| {
            Tooltip::new("No origin/HEAD, main or master to compare with").build(window, cx)
        }),
        Base::Diff {
            name,
            files,
            added,
            removed,
        } => {
            let tabular = FontFeatures(Arc::new(vec![("tnum".into(), 1)]));
            let tip = SharedString::from(format!(
                "{} changed since this branch left {name}",
                count(*files, "file", "files")
            ));
            item.child(name.clone())
                .child(
                    div()
                        .font_features(tabular.clone())
                        .text_color(lines.added)
                        .child(format!("+{}", thousands(*added))),
                )
                .child(
                    div()
                        .font_features(tabular)
                        .text_color(lines.removed)
                        .child(format!("\u{2212}{}", thousands(*removed))),
                )
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        }
    };
    Some(item)
}

/// How many files are changed and not committed, and how many are untracked. None when
/// there are none: the line only speaks up about what needs a look.
fn status_item(changes: &Changes, look: &MenuLook) -> Option<Stateful<Div>> {
    let (uncommitted, untracked) = (changes.uncommitted, changes.untracked);
    if uncommitted == 0 && untracked == 0 {
        return None;
    }
    let item = div()
        .id("git-status")
        .flex()
        .flex_none()
        .items_center()
        .gap(GIT_PART_GAP)
        .text_size(GIT_TEXT_SIZE)
        .line_height(FOLDER_TEXT_LINE)
        .text_color(look.muted);
    let mut short = Vec::new();
    let mut long = Vec::new();
    if uncommitted > 0 {
        short.push(format!("{} uncommitted", thousands(uncommitted)));
        long.push(format!(
            "{} with uncommitted changes",
            count(uncommitted, "file", "files")
        ));
    }
    if untracked > 0 {
        short.push(format!("{} untracked", thousands(untracked)));
        long.push(count(untracked, "untracked file", "untracked files"));
    }
    let tip = SharedString::from(long.join(" \u{b7} "));
    let item = item
        .child(
            // The model panel's star color, which reads as "needs a look" in both themes.
            div()
                .size(DIRTY_DOT_SIZE)
                .flex_none()
                .rounded_full()
                .bg(look.star),
        )
        .child(short.join(" \u{b7} "))
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx));
    Some(item)
}

/// One of git's items: its icon, then whatever the caller adds after it.
fn item(id: &'static str, icon: IconName, look: &MenuLook) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(GIT_PART_GAP)
        .text_size(GIT_TEXT_SIZE)
        .line_height(FOLDER_TEXT_LINE)
        .text_color(look.muted)
        .child(Icon::new(icon).size(GIT_ICON_SIZE).flex_none())
}

/// Whether the event may mean the agent changed files in the session's folder: any tool
/// call that finished may have, as a command runs as an `Execute` call and an MCP tool's
/// kind is anyone's guess, and so may a turn that ended.
pub(super) fn may_change_files(event: &SessionEvent) -> bool {
    match event {
        SessionEvent::ToolCall { status, .. }
        | SessionEvent::ToolCallUpdate {
            status: Some(status),
            ..
        } => status.finished(),
        SessionEvent::TurnEnded { .. } | SessionEvent::TurnFailed(_) => true,
        _ => false,
    }
}

/// The folder's own name, `~` for the home folder.
pub(super) fn folder_name(folder: &Path) -> String {
    if dirs::home_dir().is_some_and(|home| home == folder) {
        return "~".to_string();
    }
    folder
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| folder.display().to_string())
}

/// The folder's whole path, from `~` when it is in the home folder.
pub(super) fn tilde(folder: &Path) -> String {
    let home = dirs::home_dir();
    match home
        .as_deref()
        .and_then(|home| folder.strip_prefix(home).ok())
    {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Some(rest) => format!("~/{}", rest.display()),
        None => folder.display().to_string(),
    }
}

/// "1 file", "2 files".
fn count(n: usize, one: &str, many: &str) -> String {
    format!("{} {}", thousands(n), if n == 1 { one } else { many })
}

/// The number with a comma between each three digits: 12,480.
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (ix, digit) in digits.chars().enumerate() {
        if ix > 0 && (digits.len() - ix).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// What the folder panel gives: the folder picked, or None if it was cancelled.
#[cfg(target_os = "macos")]
fn choose_folder(
    start: PathBuf,
    window: &Window,
    _cx: &mut App,
) -> impl Future<Output = Option<PathBuf>> + use<> {
    let ns_window = panel::ns_window(window);
    // The panel opens as this is first polled, in a task of its own: a click is handled
    // inside the window's update, and the window resigning key to the sheet would call
    // back into gpui while it is busy.
    async move {
        let picked = panel::open(&start, ns_window.as_deref());
        picked.recv().await.ok().flatten()
    }
}

/// gpui's own panel, which can't be told where to start.
#[cfg(not(target_os = "macos"))]
fn choose_folder(
    _start: PathBuf,
    _window: &Window,
    cx: &mut App,
) -> impl Future<Output = Option<PathBuf>> + use<> {
    let picked = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some("Choose".into()),
    });
    async move { picked.await.ok()?.ok()??.into_iter().next() }
}

/// AppKit's folder panel, as a sheet on the window, opened at the session's folder.
#[cfg(target_os = "macos")]
mod panel {
    use std::path::{Path, PathBuf};

    use block2::RcBlock;
    use gpui_kit::Window;
    use objc2::MainThreadMarker;
    use objc2::rc::Retained;
    use objc2_app_kit::{NSModalResponse, NSModalResponseOK, NSOpenPanel, NSView, NSWindow};
    use objc2_foundation::{NSString, NSURL};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use smol::channel::Receiver;

    /// The window's AppKit window, for the sheet to hang from.
    pub(super) fn ns_window(window: &Window) -> Option<Retained<NSWindow>> {
        // `Window::window_handle` is gpui's own handle, not the platform's.
        let handle = HasWindowHandle::window_handle(window).ok()?;
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            return None;
        };
        // SAFETY: the handle's view is the window's content view, which lives as long as
        // the window.
        unsafe { handle.ns_view.cast::<NSView>().as_ref() }.window()
    }

    /// Opens the panel, which picks one folder and can make a new one, and gives the
    /// folder picked once it closes: None if it was cancelled.
    pub(super) fn open(start: &Path, window: Option<&NSWindow>) -> Receiver<Option<PathBuf>> {
        let (done, picked) = smol::channel::bounded(1);
        let Some(mtm) = MainThreadMarker::new() else {
            return picked;
        };
        let panel = NSOpenPanel::openPanel(mtm);
        panel.setCanChooseFiles(false);
        panel.setCanChooseDirectories(true);
        panel.setAllowsMultipleSelection(false);
        panel.setCanCreateDirectories(true);
        panel.setDirectoryURL(NSURL::from_directory_path(start).as_deref());
        panel.setPrompt(Some(&NSString::from_str("Choose")));
        let handler = RcBlock::new({
            let panel = panel.clone();
            move |response: NSModalResponse| {
                let folder = (response == NSModalResponseOK)
                    .then(|| panel.URLs().firstObject())
                    .flatten()
                    .and_then(|url| url.to_file_path());
                let _ = done.try_send(folder);
            }
        });
        match window {
            Some(window) => panel.beginSheetModalForWindow_completionHandler(window, &handler),
            None => panel.beginWithCompletionHandler(&handler),
        }
        picked
    }
}

#[cfg(test)]
mod tests {
    // Not a glob: gpui's own `test` attribute would shadow the standard one.
    use super::{count, thousands};

    #[test]
    fn thousands_are_set_apart() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(12_480), "12,480");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn counts_say_one_or_many() {
        assert_eq!(count(1, "file", "files"), "1 file");
        assert_eq!(count(2_000, "file", "files"), "2,000 files");
    }
}

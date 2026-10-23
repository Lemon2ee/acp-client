//! The info panel's Pull requests section: each pull request the session opened, with its
//! state, branches, size, checks and reviews, opening to the files it changes; a file
//! opens its diff in the panel's place. What it shows comes from the user's `gh`, asked
//! while the panel is shown: as a pull request is found, as the session shows, when a
//! turn ends, and every two minutes while one is open.

use std::rc::Rc;
use std::time::{Duration, Instant};

use acp_core::SessionKey;
use gpui_kit::assets;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{
    BODY_BOTTOM, BODY_LEFT, BODY_RIGHT, CHEVRON_SIZE, EMPTY_INDENT, LINE_HEIGHT, PanelLook,
    ROW_GAP, ROW_PAD, ROW_RADIUS, TEXT_LINE_HEIGHT, TEXT_SIZE, chevron, section_header,
};
use crate::chat_view::ChatView;
use crate::chat_view::tool_call::{CODE_LINE_HEIGHT, CODE_SIZE, ToolLook, diff_line, frame};
use crate::session::{
    ChecksState, GhProblem, PrState, PrStatus, PullRequest, Session, file_diffs, read_diff, view,
};

/// How long an answer from gh holds before showing the session asks again.
const CHECK_AGAIN_AFTER: Duration = Duration::from_secs(30);
/// How often gh is asked about the open pull requests on screen.
const REFRESH: Duration = Duration::from_secs(120);
const ICON_SIZE: Pixels = px(14.);
const TITLE_SIZE: Pixels = px(13.);
/// A pull request's second and third lines start under its title, past its icon.
const LINE_INDENT: Pixels = px(22.);
/// Its files sit in a little further, and the note on gh further still.
const FILE_INDENT: Pixels = px(28.);
const NOTE_INDENT: Pixels = px(28.);
/// Between the numbers of lines added and removed.
const COUNTS_GAP: Pixels = px(4.);
/// The diff's own header: the back button, the file's name and its counts.
const DIFF_HEADER_HEIGHT: Pixels = px(36.);
const DIFF_HEADER_LEFT: Pixels = px(5.);
const DIFF_HEADER_RIGHT: Pixels = px(10.);
const BACK_SIZE: Pixels = px(28.);
const BACK_ICON_SIZE: Pixels = px(16.);
/// The line under the header, with the pull request and the file's folder, in line with
/// the file's name.
const DIFF_PATH_LEFT: Pixels = px(15.);
const DIFF_PATH_RIGHT: Pixels = px(12.);
const DIFF_PATH_BELOW: Pixels = px(10.);
/// Above and below the diff's lines, inside its box.
const DIFF_BOX_PAD: Pixels = px(6.);

/// A file of a pull request, whose diff the panel shows in place of its sections.
#[derive(Clone)]
pub(crate) struct DiffView {
    key: SessionKey,
    url: String,
    path: String,
}

impl ChatView {
    /// Keeps the pull requests the sessions have just found with them, and asks gh about
    /// those the panel shows that it hasn't been asked about.
    pub(crate) fn note_pull_requests(&mut self, cx: &mut Context<Self>) {
        let found: Vec<SessionKey> = self
            .sessions
            .iter_mut()
            .filter_map(|session| {
                std::mem::take(&mut session.pull_requests.found).then_some(session.key)
            })
            .collect();
        for key in found {
            self.keep_pull_requests(key);
        }
        self.check_pull_requests(None, cx);
    }

    /// Asks gh again about the pull requests the panel shows, unless it answered just now.
    pub(crate) fn recheck_pull_requests(&mut self, cx: &mut Context<Self>) {
        self.check_pull_requests(Some(CHECK_AGAIN_AFTER), cx);
    }

    /// Asks gh about each pull request of the session on screen, while the panel shows,
    /// that it hasn't answered about, or answered about longer ago than `older_than`.
    fn check_pull_requests(&mut self, older_than: Option<Duration>, cx: &mut Context<Self>) {
        if !self.info_panel.open {
            return;
        }
        let session = &mut self.sessions[self.active];
        let key = session.key;
        for pr in &mut session.pull_requests.list {
            let due = pr
                .checked
                .is_none_or(|at| older_than.is_some_and(|age| at.elapsed() >= age));
            if pr.checking || !due {
                continue;
            }
            pr.checking = true;
            let url = pr.url.clone();
            cx.spawn(async move |this, cx| {
                let asked = url.clone();
                let answer = cx
                    .background_executor()
                    .spawn(async move { view(&asked) })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.checked_pull_request(key, &url, answer, cx);
                });
            })
            .detach();
        }
        self.keep_pr_clock(cx);
    }

    fn checked_pull_request(
        &mut self,
        key: SessionKey,
        url: &str,
        answer: Result<PrStatus, GhProblem>,
        cx: &mut Context<Self>,
    ) {
        let Some(pr) = self
            .session_mut(key)
            .and_then(|session| session.pull_requests.find_mut(url))
        else {
            return;
        };
        pr.checking = false;
        pr.checked = Some(Instant::now());
        match answer {
            Ok(status) => {
                pr.status = Some(status);
                pr.problem = None;
            }
            // What gh said before still shows.
            Err(problem) => pr.problem = Some(problem),
        }
        self.keep_pull_requests(key);
        // A diff waits for the head commit gh names.
        self.read_diff_if_shown(cx);
        cx.notify();
    }

    /// Asks gh about the open pull requests on screen every two minutes, while there are
    /// any and the panel shows.
    fn keep_pr_clock(&mut self, cx: &mut Context<Self>) {
        if self.info_panel.pr_clock.is_some() || !self.wants_pr_clock() {
            return;
        }
        self.info_panel.pr_clock = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(REFRESH).await;
                let going = this.update(cx, |this, cx| {
                    let going = this.wants_pr_clock();
                    if going {
                        // Half, so one checked just before the tick isn't skipped for
                        // a whole round.
                        this.check_pull_requests(Some(REFRESH / 2), cx);
                    } else {
                        this.info_panel.pr_clock = None;
                    }
                    going
                });
                if !matches!(going, Ok(true)) {
                    break;
                }
            }
        }));
    }

    fn wants_pr_clock(&self) -> bool {
        self.info_panel.open
            && self.sessions[self.active]
                .pull_requests
                .list
                .iter()
                .any(PullRequest::is_open)
    }

    /// Asks gh for the diff of the pull request whose file the panel shows, unless it
    /// has the diff at the head commit gh last named.
    fn read_diff_if_shown(&mut self, cx: &mut Context<Self>) {
        let Some(shown) = &self.info_panel.diff else {
            return;
        };
        let (key, url) = (shown.key, shown.url.clone());
        let Some(pr) = self
            .session_mut(key)
            .and_then(|session| session.pull_requests.find_mut(&url))
        else {
            return;
        };
        let Some(head) = pr.status.as_ref().map(|status| status.head_commit.clone()) else {
            return;
        };
        let read = pr.diffs.as_ref().is_some_and(|(at, _)| *at == head);
        if read || pr.reading_diff || pr.diff_problem.is_some() {
            return;
        }
        pr.reading_diff = true;
        cx.spawn(async move |this, cx| {
            let asked = url.clone();
            let answer = cx
                .background_executor()
                .spawn(async move { read_diff(&asked) })
                .await;
            let _ = this.update(cx, |this, cx| {
                let Some(pr) = this
                    .session_mut(key)
                    .and_then(|session| session.pull_requests.find_mut(&url))
                else {
                    return;
                };
                pr.reading_diff = false;
                match answer {
                    Ok(patch) => pr.diffs = Some((head, Rc::new(file_diffs(&patch)))),
                    Err(problem) => pr.diff_problem = Some(problem),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The Pull requests section: its header with how many there are, then each pull
    /// request, and a note when gh can't say how they stand.
    pub(super) fn render_pr_section(
        &self,
        session: &Session,
        look: PanelLook,
        cx: &mut Context<Self>,
    ) -> Div {
        let list = &session.pull_requests.list;
        let unfolded = self.info_panel.pr_section;
        let header = section_header(
            "info-pr-section",
            "Pull requests",
            Some(list.len().to_string()),
            unfolded,
            look,
        )
        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
            this.info_panel.pr_section = !this.info_panel.pr_section;
            cx.notify();
        }));
        let section = div().flex().flex_col().child(header);
        if !unfolded {
            return section;
        }
        if list.is_empty() {
            return section.child(
                div()
                    .h(LINE_HEIGHT)
                    .pl(EMPTY_INDENT)
                    .pr(ROW_PAD)
                    .flex()
                    .items_center()
                    .text_size(TEXT_SIZE)
                    .line_height(TEXT_LINE_HEIGHT)
                    .text_color(look.faint)
                    .child("None yet"),
            );
        }
        let no_gh = list.iter().any(|pr| pr.problem == Some(GhProblem::NoGh));
        let failed = list.iter().find_map(|pr| match &pr.problem {
            Some(GhProblem::Failed(said)) if pr.status.is_none() => Some(said.clone()),
            _ => None,
        });
        let note = if no_gh {
            let command = "gh auth login";
            let text = format!("Status comes from the GitHub CLI. Run {command} in a terminal.");
            let at = text.find(command).unwrap_or_default();
            let highlight = HighlightStyle {
                color: Some(look.foreground),
                ..HighlightStyle::default()
            };
            Some(StyledText::new(text).with_highlights([(at..at + command.len(), highlight)]))
        } else {
            failed.map(|said| StyledText::new(format!("The GitHub CLI said: {said}")))
        };
        section
            .children(
                list.iter()
                    .enumerate()
                    .map(|(ix, pr)| self.render_pull_request(session.key, ix, pr, look, cx)),
            )
            .children(note.map(|note| {
                div()
                    .pt(px(2.))
                    .pb(px(4.))
                    .pl(NOTE_INDENT)
                    .pr(ROW_PAD)
                    .text_size(TEXT_SIZE)
                    .line_height(TEXT_LINE_HEIGHT)
                    .text_color(look.muted)
                    .child(note)
            }))
    }

    /// A pull request: its state's icon and its title; its number, state and branches,
    /// with the lines it adds and removes; for an open one, its checks and reviews. It
    /// opens to the files it changes.
    fn render_pull_request(
        &self,
        key: SessionKey,
        ix: usize,
        pr: &PullRequest,
        look: PanelLook,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.info_panel.pr_open.as_deref() == Some(pr.url.as_str());
        let status = pr.status.as_ref();
        let state = status.map_or(PrState::Draft, |status| status.state);
        let (icon, color) = match (status, state) {
            // Grey and with the draft's icon until gh says.
            (None, _) | (_, PrState::Draft) => (assets::IconName::GitPullRequestDraft, look.draft),
            (_, PrState::Open) => (assets::IconName::GitPullRequest, look.open),
            (_, PrState::Merged) => (assets::IconName::GitMerge, look.merged),
            (_, PrState::Closed) => (assets::IconName::GitPullRequestClosed, look.closed),
        };
        let title = div()
            .h(LINE_HEIGHT)
            .w_full()
            .flex()
            .items_center()
            .gap(ROW_GAP)
            .child(
                Icon::new(icon)
                    .with_size(ICON_SIZE)
                    .flex_none()
                    .text_color(color),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .text_size(TITLE_SIZE)
                    .line_height(TEXT_LINE_HEIGHT)
                    .text_color(look.foreground)
                    .child(pr.title()),
            )
            .child(chevron(open, look));
        let checking = status.is_none() && pr.problem.is_none();
        let meta = match status {
            Some(status) => format!(
                "#{} · {} · {} ← {}",
                pr.number,
                status.state.name(),
                status.base,
                status.head
            ),
            None if checking => format!("#{} · Checking…", pr.number),
            None => format!("#{} · No status", pr.number),
        };
        let second = sub_line(look)
            .when(checking, |line| {
                line.child(
                    Spinner::new()
                        .icon(IconName::LoaderCircle)
                        .with_size(CHEVRON_SIZE)
                        .color(look.muted),
                )
            })
            .child(meta_text(meta))
            .children(status.map(|status| counts(status.added, status.removed, look)));
        let third = status
            .filter(|status| matches!(status.state, PrState::Open | PrState::Draft))
            .map(|status| {
                let (icon, color) = match status.checks.state() {
                    ChecksState::Passed => (assets::IconName::CircleCheck, look.open),
                    ChecksState::Failed => (assets::IconName::CircleX, look.closed),
                    ChecksState::Running => (assets::IconName::Clock, look.running),
                    ChecksState::None => (assets::IconName::CircleMinus, look.muted),
                };
                sub_line(look)
                    .child(
                        Icon::new(icon)
                            .with_size(ICON_SIZE)
                            .flex_none()
                            .text_color(color),
                    )
                    .child(meta_text(format!(
                        "{} · {}",
                        status.checks.text(),
                        status.review.text()
                    )))
            });
        let url = pr.url.clone();
        let row = div()
            .id(ElementId::Name(format!("info-pr-{ix}").into()))
            .role(Role::Button)
            .aria_label(pr.title())
            .w_full()
            .px(ROW_PAD)
            .flex()
            .flex_col()
            .rounded(ROW_RADIUS)
            .cursor_pointer()
            .hover(move |style| style.bg(look.hover))
            .child(title)
            .child(second)
            .children(third)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                let opened = &mut this.info_panel.pr_open;
                *opened = (opened.as_ref() != Some(&url)).then(|| url.clone());
                cx.notify();
            }));
        let files = status
            .filter(|_| open)
            .map(|status| &status.files[..])
            .unwrap_or_default();
        div()
            .flex()
            .flex_col()
            .child(row)
            .children(files.iter().enumerate().map(|(file_ix, file)| {
                let (folder, name) = match file.path.rsplit_once('/') {
                    Some((folder, name)) => (folder, name),
                    None => ("", file.path.as_str()),
                };
                let shown = DiffView {
                    key,
                    url: pr.url.clone(),
                    path: file.path.clone(),
                };
                div()
                    .id(ElementId::Name(
                        format!("info-pr-{ix}-file-{file_ix}").into(),
                    ))
                    .role(Role::Button)
                    .aria_label(format!("Show the diff of {}", file.path))
                    .h(LINE_HEIGHT)
                    .w_full()
                    .pl(FILE_INDENT)
                    .pr(ROW_PAD)
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .rounded(ROW_RADIUS)
                    .text_size(TEXT_SIZE)
                    .line_height(TEXT_LINE_HEIGHT)
                    .cursor_pointer()
                    .hover(move |style| style.bg(look.hover))
                    .child(
                        div()
                            .flex_none()
                            .max_w(relative(0.6))
                            .truncate()
                            .text_color(look.foreground)
                            .child(name.to_string()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .text_color(look.muted)
                            .child(folder.to_string()),
                    )
                    .child(counts(file.added, file.removed, look))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.open_diff(shown.clone(), cx);
                    }))
            }))
            .into_any_element()
    }

    fn open_diff(&mut self, shown: DiffView, cx: &mut Context<Self>) {
        if let Some(pr) = self
            .session_mut(shown.key)
            .and_then(|session| session.pull_requests.find_mut(&shown.url))
        {
            // Asks again after a diff that didn't come.
            pr.diff_problem = None;
        }
        self.info_panel.diff = Some(shown);
        self.read_diff_if_shown(cx);
        cx.notify();
    }

    /// The diff of the file the panel shows for this session, in the sections' place:
    /// a back button and the file's name over its lines.
    pub(super) fn render_diff_view(
        &self,
        session: &Session,
        look: PanelLook,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let shown = self.info_panel.diff.as_ref()?;
        if shown.key != session.key {
            return None;
        }
        let pr = session
            .pull_requests
            .list
            .iter()
            .find(|pr| pr.url == shown.url)?;
        let file = pr
            .status
            .as_ref()?
            .files
            .iter()
            .find(|file| file.path == shown.path)?;
        let name = file.path.rsplit('/').next().unwrap_or(&file.path);
        let header = div()
            .h(DIFF_HEADER_HEIGHT)
            .flex_none()
            .pl(DIFF_HEADER_LEFT)
            .pr(DIFF_HEADER_RIGHT)
            .flex()
            .items_center()
            .gap(px(4.))
            .child(
                div()
                    .id("info-diff-back")
                    .role(Role::Button)
                    .aria_label("Back to the list")
                    .size(BACK_SIZE)
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(ROW_RADIUS)
                    .cursor_pointer()
                    .hover(move |style| style.bg(look.hover))
                    .child(
                        Icon::new(IconName::ChevronLeft)
                            .with_size(BACK_ICON_SIZE)
                            .text_color(look.muted),
                    )
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.info_panel.diff = None;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .text_size(TITLE_SIZE)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(look.foreground)
                    .child(name.to_string()),
            )
            .child(counts(file.added, file.removed, look));
        let path = div()
            .flex_none()
            .pl(DIFF_PATH_LEFT)
            .pr(DIFF_PATH_RIGHT)
            .pb(DIFF_PATH_BELOW)
            .truncate()
            .text_size(TEXT_SIZE)
            .line_height(TEXT_LINE_HEIGHT)
            .text_color(look.muted)
            .child(format!("#{} · {}", pr.number, file.path));
        let head = pr.status.as_ref().map(|status| status.head_commit.as_str());
        let diff = pr
            .diffs
            .as_ref()
            .filter(|(at, _)| Some(at.as_str()) == head)
            .map(|(_, diffs)| diffs.get(&file.path));
        let body = match (diff, &pr.diff_problem) {
            (Some(Some(diff)), _) if !diff.lines.is_empty() => {
                let tool = ToolLook::new(cx);
                let lines = diff.lines.clone();
                let numbered = diff.numbered;
                let count = lines.len();
                let list = uniform_list(
                    ElementId::Name(format!("info-diff-{}", file.path).into()),
                    count,
                    move |range, _, _| {
                        range
                            .map(|ix| diff_line(&lines[ix], numbered, tool))
                            .collect::<Vec<_>>()
                    },
                )
                .with_width_from_item(Some(diff.widest))
                .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                .size_full();
                // As tall as its lines, and no taller than the panel leaves it: then its
                // lines scroll.
                // Its 1px edge above and below.
                let edges = px(2.);
                frame(tool)
                    .flex_shrink(1.)
                    .min_h(px(0.))
                    .h(CODE_LINE_HEIGHT * count as f32 + DIFF_BOX_PAD * 2. + edges)
                    .ml(BODY_LEFT)
                    .mr(BODY_RIGHT)
                    .mb(BODY_BOTTOM)
                    .py(DIFF_BOX_PAD)
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_size(CODE_SIZE)
                    .line_height(CODE_LINE_HEIGHT)
                    .text_color(tool.code_text)
                    .child(list)
            }
            (Some(_), _) => diff_note(
                "No lines to show: the file is binary, or only moved.",
                false,
                look,
            ),
            (None, Some(GhProblem::NoGh)) => diff_note(
                "The diff comes from the GitHub CLI. Run gh auth login in a terminal.",
                false,
                look,
            ),
            (None, Some(GhProblem::Failed(said))) => {
                diff_note(&format!("The GitHub CLI said: {said}"), false, look)
            }
            (None, None) => diff_note("Reading the diff…", true, look),
        };
        Some(
            div()
                .flex_1()
                .min_h(px(0.))
                .flex()
                .flex_col()
                .child(header)
                .child(path)
                .child(body)
                .into_any_element(),
        )
    }
}

/// A pull request's second or third line, under its title.
fn sub_line(look: PanelLook) -> Div {
    div()
        .h(LINE_HEIGHT)
        .w_full()
        .pl(LINE_INDENT)
        .flex()
        .items_center()
        .gap(ROW_GAP)
        .text_size(TEXT_SIZE)
        .line_height(TEXT_LINE_HEIGHT)
        .text_color(look.muted)
}

fn meta_text(text: String) -> Div {
    div().flex_1().min_w(px(0.)).truncate().child(text)
}

/// The lines a change adds and removes: "+38 −6".
fn counts(added: u64, removed: u64, look: PanelLook) -> Div {
    div()
        .flex_none()
        .flex()
        .gap(COUNTS_GAP)
        .text_size(TEXT_SIZE)
        .child(div().text_color(look.added).child(format!("+{added}")))
        .child(div().text_color(look.removed).child(format!("−{removed}")))
}

/// What shows in place of a diff's lines, after a spinner while it is on its way.
fn diff_note(text: &str, spinning: bool, look: PanelLook) -> Div {
    div()
        .pl(DIFF_PATH_LEFT)
        .pr(DIFF_PATH_RIGHT)
        .flex()
        .items_center()
        .gap(px(6.))
        .text_size(TEXT_SIZE)
        .line_height(TEXT_LINE_HEIGHT)
        .text_color(look.muted)
        .when(spinning, |note| {
            note.child(
                Spinner::new()
                    .icon(IconName::LoaderCircle)
                    .with_size(CHEVRON_SIZE)
                    .color(look.muted),
            )
        })
        .child(div().min_w(px(0.)).child(text.to_string()))
}

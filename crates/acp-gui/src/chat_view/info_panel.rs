//! The session's info panel, right of the transcript and under the title row, with its
//! button at the title row's right end; its left edge drags its width. It shows the pull
//! requests the session opened (see `pull_requests`), the files it made (see `files`),
//! and the session's context window: how full it is and, opened, what fills it.

mod files;
mod pull_requests;

use std::path::Path;

use acp_core::{Command, Harness};
use chrono::{DateTime, Local};
use gpui_kit::base::HandleEdge;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, resize_handle};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::chat_view::ChatView;
use crate::chat_view::info_panel::pull_requests::DiffView;
use crate::config::harness_of;
use crate::session::{Category, CategoryKind, Session, short_tokens};

/// What the panel keeps for the whole window, whichever session is on screen.
pub(crate) struct InfoPanelState {
    /// Shown, rather than slid away by its button.
    pub(crate) open: bool,
    /// How wide it is while shown. Dragged at its left edge.
    pub(crate) width: Pixels,
    /// The Context window section is unfolded.
    context_section: bool,
    /// The context window's block is opened to what fills it.
    context_detail: bool,
    /// The category opened to what makes it up, by name.
    category: Option<String>,
    /// The Pull requests section is unfolded.
    pr_section: bool,
    /// The Files section is unfolded.
    files_section: bool,
    /// The pull request opened to its files, by its address.
    pr_open: Option<String>,
    /// The file whose diff takes the panel's place, in place of its list.
    diff: Option<DiffView>,
    /// Asks gh about the open pull requests on screen every so often.
    pr_clock: Option<Task<()>>,
}

impl Default for InfoPanelState {
    fn default() -> Self {
        Self {
            open: true,
            width: PANEL_WIDTH,
            context_section: true,
            context_detail: false,
            category: None,
            pr_section: true,
            files_section: true,
            pr_open: None,
            diff: None,
            pr_clock: None,
        }
    }
}

/// How wide the panel starts, and how narrow or wide a drag can make it. The minimum
/// keeps a category's name beside its two columns of numbers.
const PANEL_WIDTH: Pixels = px(296.);
const PANEL_MIN_WIDTH: Pixels = px(260.);
const PANEL_MAX_WIDTH: Pixels = px(560.);
/// The panel button's width and height, as the sidebar button's.
const BUTTON_SIZE: Pixels = px(28.);
/// From the button to the window's right edge.
const BUTTON_INSET: Pixels = px(8.);
/// Around the panel's sections: the left one a pixel wider, for the edge's line.
const BODY_LEFT: Pixels = px(9.);
const BODY_RIGHT: Pixels = px(8.);
const BODY_BOTTOM: Pixels = px(12.);
const SECTION_GAP: Pixels = px(12.);
const HEADER_HEIGHT: Pixels = px(28.);
/// Every line of a section's list is this tall, so their rhythm stays even.
const LINE_HEIGHT: Pixels = px(24.);
const ROW_PAD: Pixels = px(6.);
const ROW_RADIUS: Pixels = px(6.);
/// Between a row's swatch, its name and its numbers.
const ROW_GAP: Pixels = px(8.);
const TEXT_SIZE: Pixels = px(12.);
const TEXT_LINE_HEIGHT: Pixels = px(16.);
/// The context window's count, over its bar.
const COUNT_SIZE: Pixels = px(13.);
const CHEVRON_SIZE: Pixels = px(12.);
const SWATCH_SIZE: Pixels = px(8.);
/// The two columns of numbers right of a category's name: its tokens and its share.
const TOKENS_WIDTH: Pixels = px(48.);
const SHARE_WIDTH: Pixels = px(40.);
/// A category's items sit in under its name, past its swatch.
const ITEM_INDENT: Pixels = px(22.);
const BAR_HEIGHT: Pixels = px(6.);
const BAR_GAP: Pixels = px(2.);
const SEGMENT_MIN_WIDTH: Pixels = px(2.);
/// What a list being counted again shows at, under its old numbers, and one kept from the
/// last run while the agent loads the session.
const STALE_OPACITY: f32 = 0.45;

/// The panel button's icon: a window outline with a line down near its right side.
const PANEL_ICON: &[u8] = include_bytes!("../../assets/info-panel.svg");

/// What a drag of the panel's edge carries. gpui tells drags apart by this type, and
/// draws it under the pointer: nothing, since the panel itself follows the pointer.
#[derive(Clone)]
pub(crate) struct ResizeInfoPanel;

impl Render for ResizeInfoPanel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// The panel's colors. The categories' seven are a validated set, each kept apart from
/// the others and from the panel's grays in both modes.
#[derive(Clone, Copy)]
struct PanelLook {
    foreground: Hsla,
    muted: Hsla,
    faint: Hsla,
    hover: Hsla,
    /// A pull request's state, and its checks: passed in `open`'s green, failed in
    /// `closed`'s red, running in `running`'s amber.
    open: Hsla,
    draft: Hsla,
    merged: Hsla,
    closed: Hsla,
    running: Hsla,
    /// The lines a change adds and removes.
    added: Hsla,
    removed: Hsla,
    /// The bar's part in use while it isn't split into categories.
    used: Hsla,
    /// The bar's free part, and the outline of its part kept for compacting.
    track: Hsla,
    /// The lines across the part kept for compacting.
    hatch: Hsla,
    categories: [Hsla; 7],
}

impl PanelLook {
    fn new(cx: &App) -> Self {
        let color = |hex: u32| Hsla::from(rgb(hex));
        let theme = cx.theme();
        if theme.is_dark() {
            Self {
                foreground: theme.foreground,
                muted: theme.muted_foreground,
                faint: color(0x8f8f8f),
                hover: white().opacity(0.07),
                open: color(0x4ade80),
                draft: theme.muted_foreground,
                merged: color(0xa78bfa),
                closed: color(0xf87171),
                running: color(0xf5b83d),
                added: color(0x4ade80),
                removed: color(0xf87171),
                used: color(0xd4d4d4),
                track: white().opacity(0.12),
                hatch: white().opacity(0.30),
                categories: [
                    0x3987e5, 0xd95926, 0x199e70, 0xc98500, 0xd55181, 0x008300, 0x9085e9,
                ]
                .map(color),
            }
        } else {
            Self {
                foreground: theme.foreground,
                muted: theme.muted_foreground,
                faint: color(0x737373),
                hover: black().opacity(0.05),
                open: color(0x15803d),
                draft: theme.muted_foreground,
                merged: color(0x7c3aed),
                closed: color(0xb91c1c),
                running: color(0xb07800),
                added: color(0x15803d),
                removed: color(0xb91c1c),
                used: color(0x404040),
                track: black().opacity(0.10),
                hatch: black().opacity(0.26),
                categories: [
                    0x2a78d6, 0xeb6834, 0x1baf7a, 0xeda100, 0xe87ba4, 0x008300, 0x6250d6,
                ]
                .map(color),
            }
        }
    }
}

/// How a part of the context window is drawn, in its bar and by its row.
#[derive(Clone, Copy)]
enum Swatch {
    Color(Hsla),
    /// Kept free for compacting: crossed by thin lines.
    Hatched,
    /// Free: an outline only.
    Empty,
    /// A row with no part of the bar, such as one of Codex's totals.
    None,
}

impl ChatView {
    /// Asks the agent what fills the context window of the session on screen, when the
    /// panel shows its Context window section and the last read is older than its last
    /// prompt. One asked for during a turn runs once the turn is over.
    pub(crate) fn read_context_if_shown(&mut self) {
        if !self.info_panel.open || !self.info_panel.context_section {
            return;
        }
        let session = &mut self.sessions[self.active];
        let context = &session.context;
        if !session.created || !context.readable || context.reading || context.fresh {
            return;
        }
        // To claude-agent-acp 0.94.0 a read is a turn, and it names a session once, from
        // its first turn's text: read before the first prompt, the session would be named
        // after its `/context` report, keep that name, and never be kept for later runs.
        if !session.prompted() {
            return;
        }
        let read = Command::ReadContext(session.key);
        if self.agents[session.agent].commands.try_send(read).is_ok() {
            session.context.reading = true;
            session.context.fresh = true;
        }
    }

    /// The button at the title row's right end that shows or hides the panel. Drawn over
    /// the row, which moves the window, as the sidebar button is.
    pub(crate) fn render_info_panel_button(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = cx.theme();
        let (muted, hover) = (theme.muted_foreground, theme.secondary);
        div()
            .id("toggle-info-panel")
            .role(Role::Button)
            .aria_label(if self.info_panel.open {
                "Hide session info"
            } else {
                "Show session info"
            })
            .absolute()
            .top((self.title_bar_height - BUTTON_SIZE) / 2.)
            .right(BUTTON_INSET)
            .size(BUTTON_SIZE)
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .cursor_pointer()
            .hover(|style| style.bg(hover))
            .occlude()
            .child(
                Icon::empty()
                    .data(PANEL_ICON)
                    .size(px(18.))
                    .text_color(muted),
            )
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.info_panel.open = !this.info_panel.open;
                this.read_context_if_shown();
                this.recheck_pull_requests(cx);
                cx.notify();
            }))
    }

    /// How much room the title row leaves its right end for the panel button.
    pub(crate) fn info_panel_button_room() -> Pixels {
        BUTTON_INSET * 2. + BUTTON_SIZE
    }

    /// The panel, `hidden` of the way slid out past the window's right edge: it keeps
    /// its width, and less of it shows.
    pub(crate) fn render_info_panel(&self, hidden: f32, cx: &mut Context<Self>) -> Div {
        let width = self.info_panel.width;
        let session = &self.sessions[self.active];
        let look = PanelLook::new(cx);
        let body = match self.render_diff_view(session, look, cx) {
            Some(diff) => diff,
            None => div()
                .id("info-panel")
                .flex_1()
                .min_h(px(0.))
                .overflow_y_scroll()
                .pl(BODY_LEFT)
                .pr(BODY_RIGHT)
                .pb(BODY_BOTTOM)
                .flex()
                .flex_col()
                .gap(SECTION_GAP)
                .child(self.render_pr_section(session, look, cx))
                .child(self.render_files_section(session, look, cx))
                .child(self.render_context_section(session, look, cx))
                .into_any_element(),
        };
        div()
            .flex_none()
            .h_full()
            .w(width * (1. - hidden))
            .overflow_hidden()
            .child(
                div()
                    .w(width)
                    .h_full()
                    .relative()
                    .flex()
                    .flex_col()
                    .child(self.info_panel_edge(cx))
                    .child(body),
            )
    }

    /// The line between the transcript and the panel, and the band over it that drags
    /// the panel's width: the root follows the drag. A double-click puts the width back.
    fn info_panel_edge(&self, cx: &Context<Self>) -> impl IntoElement {
        resize_handle("info-panel-edge", Axis::Horizontal)
            .inside(HandleEdge::Leading)
            .on_drag(ResizeInfoPanel, |_, _, _, cx| cx.new(|_| ResizeInfoPanel))
            .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                if event.click_count() == 2 {
                    this.info_panel.width = PANEL_WIDTH;
                    cx.notify();
                }
            }))
    }

    /// Follows a drag of the panel's edge: the panel starts where the pointer is.
    pub(crate) fn resize_info_panel(
        &mut self,
        event: &DragMoveEvent<ResizeInfoPanel>,
        cx: &mut Context<Self>,
    ) {
        let width = event.bounds.right() - event.event.position.x;
        self.info_panel.width = width.clamp(PANEL_MIN_WIDTH, PANEL_MAX_WIDTH);
        cx.notify();
    }

    /// The Context window section: its header, then the count over a bar of the window,
    /// which opens to what fills it.
    fn render_context_section(
        &self,
        session: &Session,
        look: PanelLook,
        cx: &mut Context<Self>,
    ) -> Div {
        let context = &session.context;
        let panel = &self.info_panel;
        let share = context
            .usage
            .map(|usage| format!("{:.0}%", percent(usage.used, usage.size)));
        let header = section_header(
            "info-context-section",
            "Context window",
            share,
            panel.context_section,
            look,
        )
        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
            this.info_panel.context_section = !this.info_panel.context_section;
            this.read_context_if_shown();
            cx.notify();
        }));
        let section = div().flex().flex_col().child(header);
        if !panel.context_section {
            return section;
        }
        let Some(usage) = context.usage else {
            return section.child(
                div()
                    .pt(px(2.))
                    .px(ROW_PAD)
                    .pb(px(6.))
                    .text_size(TEXT_SIZE)
                    .line_height(TEXT_LINE_HEIGHT)
                    .text_color(look.faint)
                    .child("Shows after the agent's first reply"),
            );
        };
        let detail = panel.context_detail;
        let report = context.report.as_ref().map(|(report, _)| report);
        // Used, then kept for compacting, then free, as the bar runs.
        let mut parts: Vec<&Category> = report
            .map(|report| report.categories.iter().collect())
            .unwrap_or_default();
        parts.sort_by_key(|category| match category.kind {
            CategoryKind::Used => 0,
            CategoryKind::Buffer => 1,
            CategoryKind::Free => 2,
        });
        let buffer: u64 = parts
            .iter()
            .filter(|category| category.kind == CategoryKind::Buffer)
            .map(|category| category.tokens)
            .sum();
        let mut colors = look.categories.iter().cycle();
        let swatches: Vec<Swatch> = parts
            .iter()
            .map(|category| match category.kind {
                CategoryKind::Used => Swatch::Color(*colors.next().unwrap_or(&look.used)),
                CategoryKind::Buffer => Swatch::Hatched,
                CategoryKind::Free => Swatch::Empty,
            })
            .collect();
        // Opened, the bar is the report's own; closed, it is the latest count, with the
        // report's part kept for compacting.
        let bar: Vec<(u64, Swatch)> = if detail && !parts.is_empty() {
            parts
                .iter()
                .zip(&swatches)
                .map(|(category, swatch)| (category.tokens, *swatch))
                .collect()
        } else {
            let free = usage.size.saturating_sub(usage.used + buffer);
            [
                (usage.used, Swatch::Color(look.used)),
                (buffer, Swatch::Hatched),
                (free, Swatch::Empty),
            ]
            .into_iter()
            .filter(|(tokens, _)| *tokens > 0)
            .collect()
        };
        let mut of = format!("of {} tokens", short_tokens(usage.size));
        if let Some(at) = context.kept_at.and_then(local) {
            of.push_str(&format!(" · as of {}", day_or_time(at)));
        }
        let block = div()
            .id("info-context")
            .role(Role::Button)
            .aria_label(if detail {
                "Hide what fills the context window"
            } else {
                "Show what fills the context window"
            })
            .w_full()
            .px(ROW_PAD)
            .pt(px(6.))
            .pb(px(8.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .rounded(ROW_RADIUS)
            .cursor_pointer()
            .hover(move |style| style.bg(look.hover))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_size(COUNT_SIZE)
                    .line_height(TEXT_LINE_HEIGHT)
                    .child(short_tokens(usage.used))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .text_color(look.muted)
                            .child(of),
                    )
                    .child(chevron(detail, look)),
            )
            .child(context_bar(&bar, look))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.info_panel.context_detail = !this.info_panel.context_detail;
                cx.notify();
            }));
        let section = section.child(block);
        if !detail {
            return section;
        }
        let footer = if context.reading {
            let text = if session.busy {
                "Counting when this turn ends…"
            } else {
                "Counting…"
            };
            Some((true, text.to_string()))
        } else if let Some((_, at)) = &context.report {
            local(*at).map(|at| {
                (
                    false,
                    format!("Counted after the last turn, {}", day_and_time(at)),
                )
            })
        } else if context.readable {
            Some((false, "Counts after the next turn".to_string()))
        } else {
            let totals = context.totals != Default::default();
            // Another command's name is the whole command line, which would fill the
            // panel and may carry a secret.
            let agent =
                harness_of(&self.agents[session.agent].name).map_or("This agent", Harness::name);
            let says = format!("{agent} doesn't say what fills the window.");
            Some((
                false,
                if totals {
                    format!("This session's totals, from each turn. {says}")
                } else {
                    says
                },
            ))
        };
        let list = if !parts.is_empty() {
            let size = report.and_then(|report| report.size).unwrap_or(usage.size);
            let folder = session.folder();
            let rows = parts
                .iter()
                .zip(swatches)
                .enumerate()
                .map(|(ix, (category, swatch))| {
                    self.render_category(ix, category, swatch, size, folder, look, cx)
                });
            div()
                .flex()
                .flex_col()
                .children(rows)
                // Being read again, or kept from the last run while the agent loads it.
                .when(context.reading || session.loading, |list| {
                    list.opacity(STALE_OPACITY)
                })
        } else if !context.readable {
            let totals = context.totals;
            let rows = [
                ("Input", totals.input),
                ("Cached input", totals.cached_read),
                ("Cache writes", totals.cached_write),
                ("Output", totals.output),
                ("Reasoning", totals.thought),
            ]
            .into_iter()
            // Cache writes are Claude's, and Claude says what fills the window instead.
            .filter(|(name, tokens)| *tokens > 0 || *name != "Cache writes")
            .filter(|_| totals != Default::default())
            .map(|(name, tokens)| {
                row(look)
                    .child(swatch(Swatch::None, look))
                    .child(row_name(name.into(), None, look))
                    .child(number(Some(short_tokens(tokens)), TOKENS_WIDTH, look))
                    .child(number(None, SHARE_WIDTH, look))
                    .into_any_element()
            });
            div().flex().flex_col().children(rows)
        } else {
            div()
        };
        section.child(list).children(footer.map(|(counting, text)| {
            div()
                .pt(px(6.))
                .px(ROW_PAD)
                .flex()
                .items_center()
                .gap(px(6.))
                .text_size(TEXT_SIZE)
                .line_height(TEXT_LINE_HEIGHT)
                .text_color(look.faint)
                .when(counting, |footer| {
                    footer.child(
                        Spinner::new()
                            .icon(IconName::LoaderCircle)
                            .with_size(CHEVRON_SIZE)
                            .color(look.muted),
                    )
                })
                .child(div().min_w(px(0.)).child(text))
        }))
    }

    /// A category's row: its swatch, its name, its tokens and its share of the window.
    /// One the report breaks down opens to what makes it up.
    #[allow(clippy::too_many_arguments)]
    fn render_category(
        &self,
        ix: usize,
        category: &Category,
        swatch_kind: Swatch,
        size: u64,
        folder: &Path,
        look: PanelLook,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let numbers = |row: Div| {
            row.child(number(
                Some(short_tokens(category.tokens)),
                TOKENS_WIDTH,
                look,
            ))
            .child(number(
                Some(format!("{:.1}%", percent(category.tokens, size))),
                SHARE_WIDTH,
                look,
            ))
        };
        let name = SharedString::from(category.name.clone());
        if category.items.is_empty() {
            return numbers(
                row(look)
                    .child(swatch(swatch_kind, look))
                    .child(row_name(name, None, look)),
            )
            .into_any_element();
        }
        let open = self.info_panel.category.as_deref() == Some(category.name.as_str());
        let items = category.items.iter().map(|item| {
            div()
                .h(LINE_HEIGHT)
                .pl(ITEM_INDENT)
                .pr(ROW_PAD)
                .flex()
                .items_center()
                .gap(ROW_GAP)
                .text_size(TEXT_SIZE)
                .line_height(TEXT_LINE_HEIGHT)
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .truncate()
                        .text_color(look.muted)
                        .child(shown_path(&item.name, folder)),
                )
                .child(number(Some(short_tokens(item.tokens)), TOKENS_WIDTH, look))
                .child(number(None, SHARE_WIDTH, look))
        });
        let toggled = category.name.clone();
        let header = numbers(row(look).child(swatch(swatch_kind, look)).child(row_name(
            name.clone(),
            Some(open),
            look,
        )))
        .id(ElementId::Name(format!("info-category-{ix}").into()))
        .role(Role::Button)
        .aria_label(name)
        .cursor_pointer()
        .hover(move |style| style.bg(look.hover))
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            let category = &mut this.info_panel.category;
            *category = (category.as_ref() != Some(&toggled)).then(|| toggled.clone());
            cx.notify();
        }));
        div()
            .flex()
            .flex_col()
            .child(header)
            .when(open, |this| this.children(items))
            .into_any_element()
    }
}

/// A section's header: a chevron that turns as it unfolds, its name and, at its end,
/// a count.
fn section_header(
    id: &'static str,
    name: &'static str,
    count: Option<String>,
    open: bool,
    look: PanelLook,
) -> Stateful<Div> {
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(name)
        .h(HEADER_HEIGHT)
        .w_full()
        .flex_none()
        .px(ROW_PAD)
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded(ROW_RADIUS)
        .text_size(TEXT_SIZE)
        .line_height(TEXT_LINE_HEIGHT)
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(look.muted)
        .cursor_pointer()
        .hover(move |style| style.bg(look.hover))
        .child(chevron(open, look))
        .child(name)
        .children(count.map(|count| div().ml_auto().font_weight(FontWeight::NORMAL).child(count)))
}

/// A line of a section's list.
fn row(look: PanelLook) -> Div {
    div()
        .h(LINE_HEIGHT)
        .w_full()
        .px(ROW_PAD)
        .flex()
        .items_center()
        .gap(ROW_GAP)
        .rounded(ROW_RADIUS)
        .text_size(TEXT_SIZE)
        .line_height(TEXT_LINE_HEIGHT)
        .text_color(look.muted)
}

/// A row's name, cut short to fit, with a chevron after it for one that opens.
fn row_name(name: SharedString, open: Option<bool>, look: PanelLook) -> Div {
    div()
        .flex_1()
        .min_w(px(0.))
        .flex()
        .items_center()
        .gap(px(4.))
        .child(
            div()
                .min_w(px(0.))
                .truncate()
                .text_color(look.foreground)
                .child(name),
        )
        .children(open.map(|open| chevron(open, look)))
}

/// One of a row's columns of numbers, set to its right edge.
fn number(text: Option<String>, width: Pixels, look: PanelLook) -> Div {
    div()
        .w(width)
        .flex_none()
        .flex()
        .justify_end()
        .text_color(look.muted)
        .children(text)
}

/// A part's color by its row.
fn swatch(kind: Swatch, look: PanelLook) -> Div {
    let swatch = div().size(SWATCH_SIZE).flex_none().rounded(px(2.));
    match kind {
        Swatch::Color(color) => swatch.bg(color),
        Swatch::Hatched => swatch
            .bg(pattern_slash(look.muted, 1.4, 2.8))
            .border_1()
            .border_color(look.muted),
        Swatch::Empty => swatch.border_1().border_color(look.muted),
        Swatch::None => swatch,
    }
}

/// The window as a bar of its parts, each as long as its share, 2px apart.
fn context_bar(parts: &[(u64, Swatch)], look: PanelLook) -> Div {
    let last = parts.len().saturating_sub(1);
    let radius = BAR_HEIGHT / 2.;
    div()
        .w_full()
        .h(BAR_HEIGHT)
        .flex()
        .gap(BAR_GAP)
        .children(parts.iter().enumerate().map(|(ix, (tokens, kind))| {
            let mut segment = div()
                .h(BAR_HEIGHT)
                .min_w(SEGMENT_MIN_WIDTH)
                .flex_basis(px(0.))
                .flex_shrink_0()
                .when(ix == 0, |segment| {
                    segment.rounded_tl(radius).rounded_bl(radius)
                })
                .when(ix == last, |segment| {
                    segment.rounded_tr(radius).rounded_br(radius)
                });
            segment.style().flex_grow = Some(*tokens as f32);
            match kind {
                Swatch::Color(color) => segment.bg(*color),
                Swatch::Hatched => segment
                    .bg(pattern_slash(look.hatch, 2.1, 3.5))
                    .border_1()
                    .border_color(look.track),
                Swatch::Empty | Swatch::None => segment.bg(look.track),
            }
        }))
}

/// A chevron that points right, and down once what it heads is open.
fn chevron(open: bool, look: PanelLook) -> Icon {
    Icon::new(IconName::ChevronRight)
        .with_size(CHEVRON_SIZE)
        .flex_none()
        .text_color(look.muted)
        .when(open, |icon| {
            icon.rotate(radians(std::f32::consts::FRAC_PI_2))
        })
}

/// `part` of `whole`, in percent.
fn percent(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        return 0.;
    }
    part as f64 / whole as f64 * 100.
}

/// A time kept as seconds since 1970, in the user's time zone.
fn local(at: i64) -> Option<DateTime<Local>> {
    DateTime::from_timestamp(at, 0).map(|at| at.with_timezone(&Local))
}

/// "14:36" today, "Oct 21" before.
fn day_or_time(at: DateTime<Local>) -> String {
    if at.date_naive() == Local::now().date_naive() {
        at.format("%H:%M").to_string()
    } else {
        at.format("%b %-d").to_string()
    }
}

/// "14:36" today, "Oct 21, 14:36" before.
fn day_and_time(at: DateTime<Local>) -> String {
    if at.date_naive() == Local::now().date_naive() {
        at.format("%H:%M").to_string()
    } else {
        at.format("%b %-d, %H:%M").to_string()
    }
}

/// A memory file's path, which starts its item's name: inside the session's folder,
/// from there, and inside the home folder, from `~`.
fn shown_path(name: &str, folder: &Path) -> String {
    let within = |base: &Path| {
        let base = base.to_string_lossy();
        name.strip_prefix(base.as_ref())
            .filter(|rest| rest.starts_with('/'))
            .map(str::to_string)
    };
    if let Some(rest) = within(folder) {
        return rest[1..].to_string();
    }
    match dirs::home_dir().and_then(|home| within(&home)) {
        Some(rest) => format!("~{rest}"),
        None => name.to_string(),
    }
}

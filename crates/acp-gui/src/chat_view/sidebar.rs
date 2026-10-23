//! The sidebar: its two lists and the tabs that switch between them, the sessions with
//! each session's card and its menu, renaming a session on its card, asking before
//! deleting one, picking several sessions and the bar shown while they are picked, the
//! Archived list, and dragging the sidebar's edge. The other list, Stuff, is drawn in
//! `stuff.rs`.

use acp_core::SessionKey;
use gpui_kit::base::HandleEdge;
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, WindowExt as _, resize_handle,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::chat_view::ChatView;
use crate::chrome::as_title_bar;
use crate::config::TitleSource;

/// What the sidebar keeps for the whole window, whichever session is on screen.
pub(crate) struct SidebarState {
    /// Shown, rather than slid away by the sidebar button.
    pub(crate) open: bool,
    /// The list on show.
    tab: SidebarTab,
    /// The list of archived sessions is open.
    archived_open: bool,
    /// How wide the sidebar is while shown. Dragged at its right edge.
    pub(crate) width: Pixels,
    /// The session whose name is being changed on its card.
    pub(crate) renaming: Option<Renaming>,
    /// The sessions picked to act on together.
    selection: Selection,
}

impl Default for SidebarState {
    fn default() -> Self {
        Self {
            open: true,
            tab: SidebarTab::Sessions,
            archived_open: false,
            width: SIDEBAR_WIDTH,
            renaming: None,
            selection: Selection::default(),
        }
    }
}

impl SidebarState {
    /// Lets go of the session, which is leaving the list it was picked in.
    pub(crate) fn unselect(&mut self, key: SessionKey) {
        self.selection.remove(key);
    }
}

/// Sessions picked in one of the sidebar's lists, apart from the session on screen:
/// picking one doesn't show it, and showing another keeps them picked.
#[derive(Default)]
struct Selection {
    /// Picked in the Archived list rather than among the others. Never both at once, so
    /// the bar's actions all go the same way.
    archived: bool,
    keys: Vec<SessionKey>,
    /// Where a ⇧-click picks from: the card last picked by itself.
    anchor: Option<SessionKey>,
}

impl Selection {
    /// Something is picked in the list `archived`, so its cards show whether they are.
    fn shown_in(&self, archived: bool) -> bool {
        !self.keys.is_empty() && self.archived == archived
    }

    fn has(&self, key: SessionKey) -> bool {
        self.keys.contains(&key)
    }

    /// Starts over in the list `archived` if what is picked is in the other one.
    fn move_to(&mut self, archived: bool) {
        if self.archived != archived {
            self.clear();
            self.archived = archived;
        }
    }

    /// Picks the card, or lets it go, in the list `archived`.
    fn toggle(&mut self, key: SessionKey, archived: bool) {
        self.move_to(archived);
        if self.has(key) {
            self.remove(key);
        } else {
            self.keys.push(key);
            self.anchor = Some(key);
        }
    }

    /// Picks every card of `order`, its list top to bottom, from the anchor to `key`.
    /// With no anchor in it, the range starts at `shown`, the session on screen, and with
    /// that not in the list either, only `key` is picked.
    fn extend(&mut self, key: SessionKey, archived: bool, order: &[SessionKey], shown: SessionKey) {
        self.move_to(archived);
        let position = |key: SessionKey| order.iter().position(|&listed| listed == key);
        let Some(end) = position(key) else {
            return;
        };
        let Some(start) = self.anchor.and_then(position).or_else(|| position(shown)) else {
            self.toggle(key, archived);
            return;
        };
        for &listed in &order[start.min(end)..=start.max(end)] {
            if !self.has(listed) {
                self.keys.push(listed);
            }
        }
        self.anchor = Some(order[start]);
    }

    fn remove(&mut self, key: SessionKey) {
        self.keys.retain(|&picked| picked != key);
        if self.anchor == Some(key) {
            self.anchor = None;
        }
    }

    fn clear(&mut self) {
        self.keys.clear();
        self.anchor = None;
    }
}

/// The sidebar's two lists, one on show at a time.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SidebarTab {
    Sessions,
    /// The note and the board of things kept for later (see `stuff.rs`).
    Stuff,
}

/// A field on a session's sidebar card for changing its name.
pub(crate) struct Renaming {
    pub(crate) key: SessionKey,
    input: Entity<InputState>,
    _events: Subscription,
}

/// How wide the sidebar starts, and how narrow or wide a drag can make it. The minimum
/// keeps the sidebar button and the "New session" row whole.
const SIDEBAR_WIDTH: Pixels = px(240.);
const SIDEBAR_MIN_WIDTH: Pixels = px(180.);
const SIDEBAR_MAX_WIDTH: Pixels = px(480.);
/// The filled square behind the "New session" plus. A bare plus is two thin lines in a
/// mostly empty box, and looked lighter than the gear at the sidebar's foot.
const NEW_SESSION_CHIP_SIZE: Pixels = px(20.);
/// The "New session" row, which the square watches to change its fill on hover.
const NEW_SESSION_GROUP: &str = "new-session";
/// The tabs above the lists: a 28px track, 8px in from the sidebar's sides like the
/// cards, holding the two tabs 2px in and 2px apart.
const TABS_HEIGHT: Pixels = px(28.);
const TABS_PAD: Pixels = px(2.);
const TABS_RADIUS: Pixels = px(8.);
const TAB_RADIUS: Pixels = px(6.);
const TAB_TEXT_SIZE: Pixels = px(13.);
const TAB_ICON_SIZE: Pixels = px(14.);
/// The circle at a card's end while its list is picked from: an icon's size, ringed
/// 1.5px wide, and filled around a check once the card is picked.
const SELECT_MARK_SIZE: Pixels = px(14.);
const SELECT_RING_WIDTH: Pixels = px(1.5);
const SELECT_CHECK_SIZE: Pixels = px(10.);
/// A card's sides, which the circle's click target reaches out to, so it is the card's
/// end rather than the circle alone.
const CARD_PAD_X: Pixels = px(12.);
/// The bar's buttons, and its right padding: 5px puts the last button's icon over the
/// cards' circles.
const BAR_BUTTON_SIZE: Pixels = px(28.);
const BAR_PAD_RIGHT: Pixels = px(5.);

/// What a drag of the sidebar's edge carries. gpui tells drags apart by this type, and
/// draws it under the pointer: nothing, since the sidebar itself follows the pointer.
#[derive(Clone)]
pub(crate) struct ResizeSidebar;

impl Render for ResizeSidebar {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

impl ChatView {
    /// Puts a field for the session's name on its sidebar card, with the name selected.
    /// Enter, or focus going elsewhere, keeps what was typed; Escape drops it.
    fn start_rename(&mut self, key: SessionKey, window: &mut Window, cx: &mut Context<Self>) {
        let Some(title) = self.session(key).and_then(|session| session.title.clone()) else {
            return;
        };
        // Before the new field takes focus: the old one's blur would come after this
        // returns, and close the new one instead.
        self.finish_rename(true, cx);
        let input = cx.new(|cx| InputState::new(window, cx).default_value(title));
        let events = cx.subscribe_in(&input, window, |this, _, event, window, cx| match event {
            InputEvent::PressEnter { .. } => {
                this.finish_rename(true, cx);
                this.input.update(cx, |input, cx| input.focus(window, cx));
            }
            InputEvent::Blur => this.finish_rename(true, cx),
            _ => {}
        });
        input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        self.sidebar.renaming = Some(Renaming {
            key,
            input,
            _events: events,
        });
        cx.notify();
    }

    /// Closes the name field. With `keep`, the session takes the name typed, unless it
    /// is blank, and a session kept for later runs keeps it there too; one its agent
    /// hasn't given an id yet is kept with it once the agent does.
    pub(crate) fn finish_rename(&mut self, keep: bool, cx: &mut Context<Self>) {
        let Some(renaming) = self.sidebar.renaming.take() else {
            return;
        };
        cx.notify();
        let title = renaming.input.read(cx).value().trim().to_string();
        let Some(session) = self.session_mut(renaming.key) else {
            return;
        };
        if !keep || title.is_empty() || session.title.as_ref() == Some(&title) {
            return;
        }
        session.title = Some(title);
        session.title_source = TitleSource::User;
        self.keep_title(renaming.key);
    }

    /// Asks before deleting the session, as nothing brings it back.
    fn confirm_delete(&mut self, key: SessionKey, window: &mut Window, cx: &mut Context<Self>) {
        let Some(title) = self.session(key).and_then(|session| session.title.clone()) else {
            return;
        };
        let title = SharedString::from(format!("Delete “{title}”?"));
        let view = cx.entity().downgrade();
        // The dialog gives focus back to what had it as it opened, which is otherwise the
        // menu it was opened from, gone by the time the dialog closes.
        self.input.update(cx, |input, cx| input.focus(window, cx));
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let view = view.clone();
            dialog
                .title(title.clone())
                .description("The agent is asked to delete it too. This can't be undone.")
                .show_cancel(true)
                .ok_text("Delete")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    let _ = view.update(cx, |this, cx| this.delete_session(key, window, cx));
                    true
                })
        });
    }

    /// The cards of one of the sidebar's lists, top to bottom: newest first, and none for
    /// the session with no prompt yet.
    fn listed(&self, archived: bool) -> Vec<SessionKey> {
        self.sessions
            .iter()
            .rev()
            .filter(|session| session.title.is_some() && session.archived == archived)
            .map(|session| session.key)
            .collect()
    }

    /// Picks the session's card, or lets it go; a pick in one list lets go of those
    /// picked in the other.
    fn toggle_selected(&mut self, key: SessionKey, archived: bool, cx: &mut Context<Self>) {
        self.sidebar.selection.toggle(key, archived);
        cx.notify();
    }

    /// Picks every card from the one last picked, or from the session on screen, to this
    /// one.
    fn select_range(&mut self, key: SessionKey, archived: bool, cx: &mut Context<Self>) {
        let order = self.listed(archived);
        let shown = self.sessions[self.active].key;
        self.sidebar.selection.extend(key, archived, &order, shown);
        cx.notify();
    }

    fn end_selection(&mut self, cx: &mut Context<Self>) {
        if !self.sidebar.selection.keys.is_empty() {
            self.sidebar.selection.clear();
            cx.notify();
        }
    }

    /// Takes Esc to let go of the picked sessions, before a permission card answers to
    /// it: from the prompt, or with nothing focused. A menu, dialog or field with the
    /// focus keeps its own Esc. Returns whether it was taken.
    pub(crate) fn selection_keystroke(
        &mut self,
        keystroke: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if keystroke.key != "escape"
            || keystroke.modifiers.modified()
            || self.sidebar.selection.keys.is_empty()
            || window.has_active_dialog(cx)
        {
            return false;
        }
        let prompt = self.input.read(cx).focus_handle(cx);
        if !prompt.is_focused(window) && window.focused(cx).is_some() {
            return false;
        }
        self.end_selection(cx);
        true
    }

    /// The circle at a card's end while its list is picked from, filled while the card is
    /// picked. A click there picks the card or lets it go, and doesn't show its session.
    fn select_mark(
        &self,
        key: SessionKey,
        archived: bool,
        title: SharedString,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = cx.theme();
        let (foreground, muted, sidebar) = (
            theme.sidebar_foreground,
            theme.muted_foreground,
            theme.sidebar,
        );
        let picked = self.sidebar.selection.has(key);
        let ring = |color: Hsla| {
            vec![
                BoxShadow::new(px(0.), px(0.), color)
                    .spread_radius(SELECT_RING_WIDTH)
                    .inset(),
            ]
        };
        let group = SharedString::from(format!("select-{}", key.0));
        let mark = div()
            .size(SELECT_MARK_SIZE)
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .map(|mark| {
                if picked {
                    mark.bg(foreground)
                        .text_color(sidebar)
                        .child(Icon::new(IconName::Check).size(SELECT_CHECK_SIZE))
                } else {
                    mark.shadow(ring(muted))
                        .group_hover(group.clone(), move |style| style.shadow(ring(foreground)))
                }
            });
        div()
            .id(ElementId::Name(group.clone()))
            .group(group)
            .role(Role::CheckBox)
            .aria_label(title)
            .aria_toggled(picked.into())
            .h_full()
            .flex_none()
            .flex()
            .items_center()
            .pl_1p5()
            .pr(CARD_PAD_X)
            .mr(-CARD_PAD_X)
            .cursor_pointer()
            .child(mark)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                // The card's own click would show the session.
                cx.stop_propagation();
                this.toggle_selected(key, archived, cx);
            }))
    }

    /// What takes the "New session" row's place while sessions are picked: the × that
    /// lets go of them where the plus was, and how many are picked.
    fn selection_bar(&self, chip_hover: Hsla, cx: &mut Context<Self>) -> Stateful<Div> {
        let hover = cx.theme().secondary;
        let count = self.sidebar.selection.keys.len();
        let (archive_icon, archive_label) = if self.sidebar.selection.archived {
            (gpui_kit::assets::IconName::ArchiveRestore, "Unarchive")
        } else {
            (gpui_kit::assets::IconName::Archive, "Archive")
        };
        div()
            .id("selection-bar")
            .h(px(32.))
            .flex_none()
            .pl(px(8.5))
            .pr(BAR_PAD_RIGHT)
            .flex()
            .items_center()
            .gap_1p5()
            .text_sm()
            .child(
                div()
                    .id("end-selection")
                    .role(Role::Button)
                    .aria_label("End the selection")
                    .size(NEW_SESSION_CHIP_SIZE)
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .bg(hover)
                    .cursor_pointer()
                    .hover(move |style| style.bg(chip_hover))
                    .child(Icon::new(IconName::Close).small())
                    .tooltip(|window, cx| Tooltip::new("End the selection").build(window, cx))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.end_selection(cx))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .font_weight(FontWeight::MEDIUM)
                    .child(format!("{count} selected")),
            )
            .child(
                bar_button("archive-selected", archive_icon, archive_label, cx).on_click(
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.archive_selected(window, cx)
                    }),
                ),
            )
    }

    /// Moves the picked sessions to Archived, or out of it, all at once. Nothing asks
    /// first: Unarchive, or Archive, puts any back.
    fn archive_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let archived = !self.sidebar.selection.archived;
        let keys = std::mem::take(&mut self.sidebar.selection.keys);
        self.sidebar.selection.clear();
        for key in keys {
            self.set_archived(key, archived, window, cx);
        }
        cx.notify();
    }

    /// A session's card in the sidebar: its name, or the field to change it. The session
    /// on screen gets a filled card. While its list is picked from, a circle at its end
    /// says whether it is picked.
    fn render_card(&self, ix: usize, title: String, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (accent, accent_foreground) = (theme.sidebar_accent, theme.sidebar_accent_foreground);
        let (ring, muted) = (theme.ring, theme.muted_foreground);
        let key = self.sessions[ix].key;
        let archived = self.sessions[ix].archived;
        let card = div()
            .id(ElementId::Name(format!("session-{}", key.0).into()))
            .h(px(32.))
            .px(CARD_PAD_X)
            .flex()
            .items_center()
            .rounded_md()
            .text_sm()
            .map(|card| {
                if ix == self.active {
                    card.bg(accent)
                        .text_color(accent_foreground)
                        .font_weight(FontWeight::MEDIUM)
                } else {
                    card.hover(|style| style.bg(accent.opacity(0.5)))
                        .when(archived, |card| card.text_color(muted))
                }
            });
        if let Some(renaming) = self
            .sidebar
            .renaming
            .as_ref()
            .filter(|renaming| renaming.key == key)
        {
            // The name's own text, plain and in place, inside a ring drawn within
            // the card so nothing moves.
            return card
                .shadow(vec![
                    BoxShadow::new(px(0.), px(0.), ring)
                        .spread_radius(px(1.))
                        .inset(),
                ])
                .child(Input::new(&renaming.input).appearance(false).small().p_0())
                .on_action(cx.listener(|this, _: &Escape, window, cx| {
                    this.finish_rename(false, cx);
                    this.input.update(cx, |input, cx| input.focus(window, cx));
                }))
                // A click on the chat takes no focus from the field, so it
                // wouldn't close it otherwise.
                .on_mouse_down_out(
                    cx.listener(|this, _: &MouseDownEvent, _, cx| this.finish_rename(true, cx)),
                )
                .into_any_element();
        }
        let title = SharedString::from(title);
        let view = cx.entity().downgrade();
        let selecting = self.sidebar.selection.shown_in(archived);
        let picked = self.sidebar.selection.has(key);
        let picked_count = self.sidebar.selection.keys.len();
        card.role(Role::Button)
            .aria_label(title.clone())
            .cursor_pointer()
            .child(div().flex_1().min_w(px(0.)).truncate().child(title.clone()))
            .when(selecting, |card| {
                card.child(self.select_mark(key, archived, title, cx))
            })
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                let modifiers = event.modifiers();
                if modifiers.shift {
                    this.select_range(key, archived, cx)
                } else if modifiers.secondary() {
                    this.toggle_selected(key, archived, cx)
                } else if event.click_count() == 2 {
                    // The first click of the two has shown the session already.
                    this.start_rename(key, window, cx)
                } else {
                    this.select(key, window, cx)
                }
            }))
            .context_menu(move |menu, _, _| {
                let (rename, archive, delete) = (view.clone(), view.clone(), view.clone());
                let pick = view.clone();
                let archive_label = if archived { "Unarchive" } else { "Archive" };
                // A picked card's menu acts on all those picked.
                if picked {
                    let label = format!("{archive_label} {}", sessions(picked_count));
                    return menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                        let _ = archive.update(cx, |this, cx| this.archive_selected(window, cx));
                    }));
                }
                menu.item(PopupMenuItem::new("Rename").on_click(move |_, window, cx| {
                    let _ = rename.update(cx, |this, cx| this.start_rename(key, window, cx));
                }))
                // The way to start picking without a key held.
                .when(!picked, |menu| {
                    menu.item(PopupMenuItem::new("Select").on_click(move |_, _, cx| {
                        let _ = pick.update(cx, |this, cx| this.toggle_selected(key, archived, cx));
                    }))
                })
                .item(
                    PopupMenuItem::new(archive_label).on_click(move |_, window, cx| {
                        let _ = archive
                            .update(cx, |this, cx| this.set_archived(key, !archived, window, cx));
                    }),
                )
                .item(
                    PopupMenuItem::new("Delete…").on_click(move |_, window, cx| {
                        let _ = delete.update(cx, |this, cx| this.confirm_delete(key, window, cx));
                    }),
                )
            })
            .into_any_element()
    }

    pub(crate) fn render_sidebar(
        &self,
        title_bar: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = cx.theme();
        let (foreground, hover, muted) = (
            theme.sidebar_foreground,
            theme.secondary,
            theme.muted_foreground,
        );
        // #343434 in the dark theme and #d7d7d7 in the light one.
        let chip_hover = hover.blend(foreground.opacity(0.07));
        // Newest first. The session with no prompt yet has no card: the agent hasn't
        // created it, and it may give way to another.
        let (mut cards, mut archived) = (Vec::new(), Vec::new());
        for (ix, session) in self.sessions.iter().enumerate().rev() {
            let Some(title) = session.title.clone() else {
                continue;
            };
            let list = if session.archived {
                &mut archived
            } else {
                &mut cards
            };
            list.push(self.render_card(ix, title, cx));
        }
        // Closed, the list shows only that there are archived sessions.
        let archived_list = (!archived.is_empty()).then(|| {
            let open = self.sidebar.archived_open;
            div()
                .id("archived")
                .role(Role::Button)
                .aria_label(if open {
                    "Hide archived sessions"
                } else {
                    "Show archived sessions"
                })
                .h(px(32.))
                .mt_2()
                .px_3()
                .flex()
                .flex_none()
                .items_center()
                .gap_1()
                .rounded_md()
                .text_xs()
                .text_color(muted)
                .cursor_pointer()
                .hover(|style| style.bg(hover))
                .child("Archived")
                .child(
                    Icon::new(if open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .xsmall(),
                )
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.sidebar.archived_open = !this.sidebar.archived_open;
                    // Nothing picked stays out of sight.
                    if !this.sidebar.archived_open && this.sidebar.selection.archived {
                        this.end_selection(cx);
                    }
                    cx.notify();
                }))
        });
        let top_row = if self.sidebar.selection.keys.is_empty() {
            self.new_session_row(chip_hover, cx)
        } else {
            self.selection_bar(chip_hover, cx)
        };
        div()
            .w(self.sidebar.width)
            .flex_none()
            .h_full()
            .relative()
            .flex()
            .flex_col()
            .text_color(foreground)
            .child(self.sidebar_edge(cx))
            .child(
                // Nothing goes here: the traffic lights and the sidebar button are drawn
                // over this row.
                div()
                    .flex_none()
                    .h(self.title_bar_height)
                    .when(title_bar, as_title_bar),
            )
            .child(self.render_tabs(cx))
            .map(|sidebar| match self.sidebar.tab {
                SidebarTab::Sessions => sidebar.child(
                    div()
                        .flex_1()
                        .min_h(px(0.))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .p_2()
                        .child(top_row)
                        .child(
                            div()
                                .id("sessions")
                                .flex_1()
                                .min_h(px(0.))
                                .mt_2()
                                .flex()
                                .flex_col()
                                .gap_0p5()
                                .overflow_y_scroll()
                                .children(cards)
                                .children(archived_list)
                                .when(self.sidebar.archived_open, |list| list.children(archived))
                                .child(
                                    // The list's space below its cards, where a click lets
                                    // go of those picked.
                                    div().id("sessions-space").flex_1().on_click(cx.listener(
                                        |this, _: &ClickEvent, _, cx| this.end_selection(cx),
                                    )),
                                ),
                        )
                        .child(self.settings_row(cx)),
                ),
                SidebarTab::Stuff => sidebar
                    .child(self.render_stuff(window, cx))
                    .child(div().flex_none().p_2().child(self.settings_row(cx))),
            })
    }

    /// The row over the cards that shows a session with no prompt yet.
    fn new_session_row(&self, chip_hover: Hsla, cx: &mut Context<Self>) -> Stateful<Div> {
        let hover = cx.theme().secondary;
        // A plain div: the component Button always centers its label.
        div()
            .id("new-session")
            .group(NEW_SESSION_GROUP)
            .role(Role::Button)
            .aria_label("New session")
            .h(px(32.))
            .flex_none()
            // Centers the square over the gear at the foot, which puts its left edge 3.5px
            // out from the cards' titles.
            .pl(px(8.5))
            .pr_3()
            .flex()
            .items_center()
            .gap_1p5()
            .rounded_md()
            .text_sm()
            .cursor_pointer()
            .hover(|style| style.bg(hover))
            .child(
                div()
                    .size(NEW_SESSION_CHIP_SIZE)
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .bg(hover)
                    // The row's hover fill is the square's own color, so the square steps
                    // toward the text color to stay visible.
                    .group_hover(NEW_SESSION_GROUP, move |style| style.bg(chip_hover))
                    .child(Icon::new(IconName::Plus).small()),
            )
            .child("New session")
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.new_session(window, cx)))
    }

    /// The tabs over the sidebar's two lists, in a track a step off the sidebar's color.
    fn render_tabs(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = cx.theme();
        let (foreground, muted, sidebar) =
            (theme.foreground, theme.muted_foreground, theme.sidebar);
        // #1d1d1d in the dark theme and #ebebeb in the light one. The tab on show is a
        // step further in dark mode (#2b2b2b), and white with a hairline in light mode.
        let track = sidebar.blend(foreground.opacity(0.05));
        let (raised, raised_shadow) = if theme.is_dark() {
            (sidebar.blend(foreground.opacity(0.11)), Vec::new())
        } else {
            (
                white(),
                vec![
                    BoxShadow::new(px(0.), px(0.), black().opacity(0.06)).spread_radius(px(1.)),
                    BoxShadow::new(px(0.), px(1.), black().opacity(0.08)).blur_radius(px(1.)),
                ],
            )
        };
        let tab = |tab: SidebarTab, label: &'static str, icon: gpui_kit::assets::IconName| {
            let shown = self.sidebar.tab == tab;
            div()
                .id(label)
                .role(Role::Tab)
                .aria_label(label)
                .aria_selected(shown)
                .flex_1()
                .min_w(px(0.))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .gap_1p5()
                .rounded(TAB_RADIUS)
                .text_size(TAB_TEXT_SIZE)
                .child(Icon::new(icon).size(TAB_ICON_SIZE))
                .child(label)
                .map(|el| {
                    if shown {
                        el.bg(raised)
                            .shadow(raised_shadow.clone())
                            .text_color(foreground)
                            .font_weight(FontWeight::MEDIUM)
                    } else {
                        el.text_color(muted)
                            .cursor_pointer()
                            .hover(move |style| style.text_color(foreground))
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.show_tab(tab, window, cx)
                            }))
                    }
                })
        };
        div()
            .id("sidebar-tabs")
            .role(Role::TabList)
            .aria_label("Sidebar")
            .flex_none()
            .mx_2()
            .h(TABS_HEIGHT)
            .p(TABS_PAD)
            .flex()
            .gap(TABS_PAD)
            .rounded(TABS_RADIUS)
            .bg(track)
            .child(tab(
                SidebarTab::Sessions,
                "Sessions",
                gpui_kit::assets::IconName::MessagesSquare,
            ))
            .child(tab(
                SidebarTab::Stuff,
                "Stuff",
                gpui_kit::assets::IconName::StickyNote,
            ))
    }

    /// Shows one of the sidebar's lists, and gives the keys to what it is for: the note
    /// for Stuff, the prompt for the sessions.
    fn show_tab(&mut self, tab: SidebarTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.sidebar.tab == tab {
            return;
        }
        self.sidebar.tab = tab;
        // A name being typed on a session's card keeps what was typed, as it would if
        // the focus went elsewhere. Picked sessions are let go of, out of sight.
        self.finish_rename(true, cx);
        self.end_selection(cx);
        match tab {
            SidebarTab::Stuff => self
                .stuff
                .input
                .update(cx, |input, cx| input.focus(window, cx)),
            SidebarTab::Sessions => self.input.update(cx, |input, cx| input.focus(window, cx)),
        }
        cx.notify();
    }

    /// The row at the sidebar's foot that opens the settings, under either list.
    fn settings_row(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let hover = cx.theme().secondary;
        div()
            .id("open-settings")
            .role(Role::Button)
            .aria_label("Settings")
            .h(px(32.))
            .flex_none()
            // The gear nearly fills its 14px box, so the padding is 0.5px short of the
            // cards' to line its outline up with their titles.
            .pl(px(11.5))
            .pr_3()
            .flex()
            .items_center()
            .gap(px(7.))
            .rounded_md()
            .text_sm()
            .cursor_pointer()
            .hover(|style| style.bg(hover))
            .child(Icon::new(IconName::Settings).small())
            .child("Settings")
            .on_click(
                cx.listener(|this, _: &ClickEvent, window, cx| this.open_settings(window, cx)),
            )
    }

    /// The line between the sidebar, or the settings' list of pages, and what is right of
    /// it, and the band over it that drags the sidebar's width: the root follows the
    /// drag. A double-click puts the width back.
    pub(crate) fn sidebar_edge(&self, cx: &Context<Self>) -> impl IntoElement {
        resize_handle("sidebar-edge", Axis::Horizontal)
            .inside(HandleEdge::Trailing)
            .on_drag(ResizeSidebar, |_, _, _, cx| cx.new(|_| ResizeSidebar))
            .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                if event.click_count() == 2 {
                    this.sidebar.width = SIDEBAR_WIDTH;
                    cx.notify();
                }
            }))
    }

    /// Follows a drag of the sidebar's edge: the sidebar ends where the pointer is.
    pub(crate) fn resize_sidebar(
        &mut self,
        event: &DragMoveEvent<ResizeSidebar>,
        cx: &mut Context<Self>,
    ) {
        let width = event.event.position.x - event.bounds.left();
        self.sidebar.width = width.clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
        cx.notify();
    }
}

/// One of the selection bar's buttons: an icon, named by its tooltip.
fn bar_button(
    id: &'static str,
    icon: gpui_kit::assets::IconName,
    label: &'static str,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme();
    let (hover, muted, foreground) = (
        theme.secondary,
        theme.muted_foreground,
        theme.sidebar_foreground,
    );
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label)
        .size(BAR_BUTTON_SIZE)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .text_color(muted)
        .cursor_pointer()
        .hover(move |style| style.bg(hover).text_color(foreground))
        .child(Icon::new(icon).small())
        .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
}

/// "1 session", "4 sessions".
fn sessions(count: usize) -> String {
    if count == 1 {
        "1 session".to_string()
    } else {
        format!("{count} sessions")
    }
}

#[cfg(test)]
mod tests {
    // Not a glob: gpui's own `test` attribute would shadow the standard one.
    use super::{Selection, SessionKey, sessions};

    #[test]
    fn one_session_is_said_without_an_s() {
        assert_eq!(sessions(1), "1 session");
        assert_eq!(sessions(4), "4 sessions");
    }

    fn keys(ids: &[u64]) -> Vec<SessionKey> {
        ids.iter().map(|&id| SessionKey(id)).collect()
    }

    #[test]
    fn a_second_pick_lets_go() {
        let mut selection = Selection::default();
        selection.toggle(SessionKey(3), false);
        selection.toggle(SessionKey(5), false);
        selection.toggle(SessionKey(3), false);
        assert_eq!(selection.keys, keys(&[5]));
        assert_eq!(selection.anchor, Some(SessionKey(5)));
        selection.toggle(SessionKey(5), false);
        assert!(!selection.shown_in(false));
        assert_eq!(selection.anchor, None);
    }

    #[test]
    fn a_pick_in_the_other_list_starts_over() {
        let mut selection = Selection::default();
        selection.toggle(SessionKey(3), false);
        selection.toggle(SessionKey(8), true);
        assert_eq!(selection.keys, keys(&[8]));
        assert!(selection.shown_in(true));
        assert!(!selection.shown_in(false));
    }

    #[test]
    fn a_range_runs_from_the_last_pick() {
        let order = keys(&[9, 7, 5, 4, 2]);
        let mut selection = Selection::default();
        selection.toggle(SessionKey(7), false);
        selection.extend(SessionKey(4), false, &order, SessionKey(9));
        assert_eq!(selection.keys, keys(&[7, 5, 4]));
        // Upwards from the same card, adding to what is picked.
        selection.extend(SessionKey(9), false, &order, SessionKey(2));
        assert_eq!(selection.keys, keys(&[7, 5, 4, 9]));
    }

    #[test]
    fn a_first_range_runs_from_the_session_on_screen() {
        let order = keys(&[9, 7, 5, 4, 2]);
        let mut selection = Selection::default();
        selection.extend(SessionKey(5), false, &order, SessionKey(2));
        assert_eq!(selection.keys, keys(&[5, 4, 2]));
        assert_eq!(selection.anchor, Some(SessionKey(2)));
        // With the session on screen in the other list, only the card is picked.
        let mut selection = Selection::default();
        selection.extend(SessionKey(5), false, &order, SessionKey(1));
        assert_eq!(selection.keys, keys(&[5]));
    }
}

//! The sidebar of sessions: each session's card with its menu, renaming a session on its
//! card, asking before deleting one, the Archived list, and dragging the sidebar's edge.

use acp_core::SessionKey;
use gpui_kit::base::HandleEdge;
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, WindowExt as _, resize_handle,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::chat_view::ChatView;
use crate::chrome::as_title_bar;

/// A field on a session's sidebar card for changing its name.
pub(crate) struct Renaming {
    pub(crate) key: SessionKey,
    input: Entity<InputState>,
    _events: Subscription,
}

/// How wide the sidebar starts, and how narrow or wide a drag can make it. The minimum
/// keeps the sidebar button and the "New session" row whole.
pub(crate) const SIDEBAR_WIDTH: Pixels = px(240.);
const SIDEBAR_MIN_WIDTH: Pixels = px(180.);
const SIDEBAR_MAX_WIDTH: Pixels = px(480.);

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
        self.renaming = Some(Renaming {
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
        let Some(renaming) = self.renaming.take() else {
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
        session.title = Some(title.clone());
        self.change_kept(renaming.key, |saved| saved.title = title);
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

    /// A session's card in the sidebar: its name, or the field to change it. The session
    /// on screen gets a filled card.
    fn render_card(&self, ix: usize, title: String, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (accent, accent_foreground) = (theme.sidebar_accent, theme.sidebar_accent_foreground);
        let (ring, muted) = (theme.ring, theme.muted_foreground);
        let key = self.sessions[ix].key;
        let archived = self.sessions[ix].archived;
        let card = div()
            .id(ElementId::Name(format!("session-{}", key.0).into()))
            .h(px(32.))
            .px_3()
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
        card.role(Role::Button)
            .aria_label(title.clone())
            .cursor_pointer()
            .child(div().flex_1().min_w(px(0.)).truncate().child(title))
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                // The first click of the two has shown the session already.
                if event.click_count() == 2 {
                    this.start_rename(key, window, cx)
                } else {
                    this.select(key, window, cx)
                }
            }))
            .context_menu(move |menu, _, _| {
                let (rename, archive, delete) = (view.clone(), view.clone(), view.clone());
                let archive_label = if archived { "Unarchive" } else { "Archive" };
                menu.item(PopupMenuItem::new("Rename").on_click(move |_, window, cx| {
                    let _ = rename.update(cx, |this, cx| this.start_rename(key, window, cx));
                }))
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

    pub(crate) fn render_sidebar(&self, title_bar: bool, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let (foreground, hover, muted) = (
            theme.sidebar_foreground,
            theme.secondary,
            theme.muted_foreground,
        );
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
            let open = self.archived_open;
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
                    this.archived_open = !this.archived_open;
                    cx.notify();
                }))
        });
        div()
            .w(self.sidebar_width)
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
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_2()
                    .pb_2()
                    .child(
                        // A plain div: the component Button always centers its label.
                        div()
                            .id("new-session")
                            .role(Role::Button)
                            .aria_label("New session")
                            .h(px(32.))
                            // The plus starts 1.5px further into its icon than a title's
                            // first letter does into its text, so the left padding is
                            // 1.5px short of the cards' to line the plus up with them.
                            .pl(px(10.5))
                            .pr_3()
                            .flex()
                            .items_center()
                            .gap_2()
                            .rounded_md()
                            .text_sm()
                            .cursor_pointer()
                            .hover(|style| style.bg(hover))
                            .child(Icon::new(IconName::Plus).small())
                            .child("New session")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.new_session(window, cx)
                            })),
                    )
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
                            .when(self.archived_open, |list| list.children(archived)),
                    )
                    .child(
                        div()
                            .id("open-settings")
                            .role(Role::Button)
                            .aria_label("Settings")
                            .h(px(32.))
                            .flex_none()
                            // The gear reaches about 1px further out than the plus above
                            // it, so it starts 1px further in, and 1px closer to its label.
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
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.open_settings(window, cx)
                            })),
                    ),
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
                    this.sidebar_width = SIDEBAR_WIDTH;
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
        self.sidebar_width = width.clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
        cx.notify();
    }
}

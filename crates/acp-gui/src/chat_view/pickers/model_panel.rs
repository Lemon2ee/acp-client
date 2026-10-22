//! The model panel that opens from the model's button: a tab for the favorites and one for
//! each agent, a search box over the tab's models with their stars, and the session's
//! switches along the bottom.

use std::ops::Range;

use acp_core::{SessionKey, Setting, SettingChoice, SettingKind, SettingValue};
use gpui_kit::component::input::{Input, InputGroupButton, InputState};
use gpui_kit::component::popover::{Popover, PopoverState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::chat_view::ChatView;
use crate::chat_view::pickers::{
    HEADING_ICON_SIZE, LEAD_GAP, LEAD_ICON_SIZE, LIST_HEADER_GAP, LIST_HEADER_HEIGHT,
    MENU_HEADING_SIZE, MENU_OFFSET, MENU_TEXT_SIZE, MODEL_ROW_GROUP, MenuLook, PANEL_HEIGHT,
    PANEL_WIDTH, PICKER_ICON_SIZE, ROW_PAD_LEFT, ROW_RADIUS, SEARCH_ROW_HEIGHT, STAR_BUTTON_SIZE,
    SURFACE_PADDING, SURFACE_RADIUS, SWITCH_ROW_HEIGHT, TAB_COLUMN_WIDTH, TAB_ICON_SIZE, TAB_SIZE,
    agent_logo, check, lead, listed_choices, picker_button, rise_in, shown_choice,
};
use crate::config::{Favorite, change_favorites, config_folder, kept_name};
use crate::session::Session;

/// A model's name as its row shows it: a trailing note in parentheses, such as
/// "(1M context)", in `muted`, and the part `query` matched in semibold.
fn model_name(name: &str, query: &str, muted: Hsla) -> StyledText {
    let note = name
        .rfind(" (")
        .filter(|_| name.ends_with(')'))
        .unwrap_or(name.len());
    // The list matches the lowercased name, whose letters can take another number of
    // bytes than the name's own. Then the match is left unmarked.
    let found = (!query.is_empty())
        .then(|| name.to_lowercase())
        .filter(|lower| lower.len() == name.len())
        .and_then(|lower| lower.find(query))
        .map(|start| start..start + query.len())
        .filter(|found| name.is_char_boundary(found.start) && name.is_char_boundary(found.end));
    let mut edges = vec![0, note, name.len()];
    if let Some(found) = &found {
        edges.extend([found.start, found.end]);
    }
    edges.sort_unstable();
    edges.dedup();
    let highlights: Vec<(Range<usize>, HighlightStyle)> = edges
        .windows(2)
        .filter_map(|pair| {
            let range = pair[0]..pair[1];
            let in_note = range.start >= note;
            let matched = found
                .as_ref()
                .is_some_and(|found| range.start >= found.start && range.end <= found.end);
            (in_note || matched).then(|| {
                let style = HighlightStyle {
                    color: in_note.then_some(muted),
                    font_weight: matched.then_some(FontWeight::SEMIBOLD),
                    ..Default::default()
                };
                (range, style)
            })
        })
        .collect();
    StyledText::new(name.to_string()).with_highlights(highlights)
}

/// What the model panel keeps while it is open.
pub(crate) struct ModelPanelState {
    /// The tab on show.
    pub(crate) tab: ModelTab,
    /// The search box, which narrows the tab to the models named like its text.
    pub(crate) search: Entity<InputState>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModelTab {
    /// The starred models of every agent.
    Favorites,
    /// The models of the agent at this index.
    Agent(usize),
}

/// One agent's models in the model panel.
pub(crate) struct ModelGroup {
    agent: usize,
    name: String,
    /// The name the agent's favorites go by, from `kept_name`. None for a command,
    /// whose models can't be starred: the favorites file would name the command.
    kept: Option<&'static str>,
    setting: Setting,
    /// Whether this is the session's own agent, whose model is set in place. Another
    /// agent's model goes on to a session on that agent.
    own: bool,
}

/// What the parts of the model panel share.
pub(crate) struct ModelPanel<'a> {
    view: &'a WeakEntity<ChatView>,
    key: SessionKey,
    popover: &'a Entity<PopoverState>,
    search: &'a Entity<InputState>,
    look: MenuLook,
}

impl ModelPanel<'_> {
    /// A tab in the panel's left column, which shows `tab` when pressed.
    fn tab(&self, tab: ModelTab, icon: Icon, name: SharedString, selected: bool) -> Stateful<Div> {
        let view = self.view.clone();
        let look = self.look;
        let tooltip = name.clone();
        div()
            .id(ElementId::Name(format!("model-tab-{name}").into()))
            .size(TAB_SIZE)
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(ROW_RADIUS)
            .cursor_pointer()
            .map(|tab| {
                if selected {
                    tab.bg(look.pressed).text_color(look.foreground)
                } else {
                    tab.text_color(look.muted)
                        .hover(move |style| style.bg(look.hover).text_color(look.foreground))
                }
            })
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .child(icon.size(TAB_ICON_SIZE))
            .on_click(move |_, _, cx| {
                let _ = view.update(cx, |this, cx| this.show_model_tab(tab, cx));
            })
    }

    /// A model's row: a check before the session's model, the name, and its star. A
    /// press sets the session's model, or goes on to a session on the model's agent,
    /// and closes the panel. `query` is the search, which the name shows in bold.
    fn model_row(
        &self,
        group: &ModelGroup,
        choice: &SettingChoice,
        current: bool,
        starred: bool,
        query: &str,
    ) -> Stateful<Div> {
        let (view, popover, key) = (self.view.clone(), self.popover.clone(), self.key);
        let (agent, own) = (group.agent, group.own);
        let (setting, model) = (group.setting.id.clone(), choice.id.clone());
        self.look
            .row(ElementId::Name(
                format!("model-row-{agent}-{}", choice.id).into(),
            ))
            .group(MODEL_ROW_GROUP)
            .child(lead().when(current, |lead| lead.child(check())))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(model_name(&choice.name, query, self.look.muted)),
            )
            .children(
                group
                    .kept
                    .map(|kept| self.star(kept, group.agent, choice, starred)),
            )
            .on_click(move |_, window, cx| {
                let _ = view.update(cx, |this, cx| {
                    if own {
                        let value = SettingValue::Choice(model.clone());
                        this.change_setting(key, setting.clone(), value, cx);
                    } else {
                        this.pick_model(key, agent, setting.clone(), model.clone(), window, cx);
                    }
                });
                popover.update(cx, |state, cx| state.dismiss(window, cx));
            })
    }

    /// The star at the end of a model's row, which stars the model or takes its star
    /// off. A starred model's star is filled in amber and always shows; the others show
    /// while their row is pointed at. `kept` is the agent's name in the favorites.
    fn star(
        &self,
        kept: &'static str,
        agent: usize,
        choice: &SettingChoice,
        starred: bool,
    ) -> Stateful<Div> {
        let view = self.view.clone();
        let look = self.look;
        let favorite = Favorite {
            agent: kept.to_string(),
            model: choice.id.clone(),
        };
        let label = if starred {
            "Remove from favorites"
        } else {
            "Add to favorites"
        };
        div()
            .id(ElementId::Name(
                format!("model-star-{agent}-{}", choice.id).into(),
            ))
            .role(Role::Button)
            .aria_label(label)
            .size(STAR_BUTTON_SIZE)
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(ROW_RADIUS - px(1.))
            .map(|star| {
                if starred {
                    star.text_color(look.star)
                        .hover(move |style| style.bg(look.pressed))
                } else {
                    star.text_color(look.muted)
                        .hover(move |style| style.bg(look.pressed).text_color(look.foreground))
                        .opacity(0.)
                        .group_hover(MODEL_ROW_GROUP, |style| style.opacity(1.))
                }
            })
            .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
            .child(
                Icon::new(if starred {
                    IconName::StarFill
                } else {
                    IconName::Star
                })
                .size(LEAD_ICON_SIZE),
            )
            .on_click(move |_, _, cx| {
                // Doesn't pick the model, as a press elsewhere on its row does.
                cx.stop_propagation();
                let _ = view.update(cx, |this, cx| {
                    this.set_favorite(favorite.clone(), !starred, cx)
                });
            })
    }

    /// A line in the favorites naming the agent whose models follow it, with its mark
    /// where the models' checks go.
    fn group_header(&self, name: &str, first: bool) -> AnyElement {
        div()
            .h(LIST_HEADER_HEIGHT)
            .flex_none()
            .when(!first, |header| header.mt(LIST_HEADER_GAP))
            .pl(ROW_PAD_LEFT)
            .flex()
            .items_center()
            .gap(LEAD_GAP)
            .text_size(MENU_HEADING_SIZE)
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(self.look.muted)
            .child(lead().child(agent_logo(name).size(HEADING_ICON_SIZE)))
            .child(SharedString::from(name.to_string()))
            .into_any_element()
    }

    /// A row along the panel's bottom for a setting of the model that is on or off.
    /// The switch only shows the value: a press anywhere on the row flips it.
    fn switch_row(&self, setting: &Setting, on: bool) -> Stateful<Div> {
        let (view, key, id) = (self.view.clone(), self.key, setting.id.clone());
        self.look
            .row(ElementId::Name(
                format!("model-switch-{}", setting.id).into(),
            ))
            .h(SWITCH_ROW_HEIGHT)
            .pr(ROW_PAD_LEFT)
            // Empty, so the name starts where the models' names do.
            .child(lead())
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(SharedString::from(setting.name.clone())),
            )
            .child(
                Switch::new(ElementId::Name(
                    format!("model-switch-value-{}", setting.id).into(),
                ))
                .checked(on)
                .small()
                .tab_stop(false),
            )
            .on_click(move |_, _, cx| {
                let _ = view.update(cx, |this, cx| this.flip_switch(key, id.clone(), cx));
            })
    }
}

/// The models of `group` that pass `only` and are named like `query`, with whether
/// each is the session's model. `only` goes first, as it is the cheaper test.
fn listed_models<'a>(
    group: &'a ModelGroup,
    only: impl Fn(&SettingChoice) -> bool,
    query: &str,
) -> Vec<(&'a SettingChoice, bool)> {
    let current = group
        .own
        .then(|| shown_choice(&group.setting))
        .flatten()
        .map(|choice| choice.id.as_str());
    listed_choices(&group.setting)
        .filter(|choice| only(choice))
        .filter(|choice| query.is_empty() || choice.name.to_lowercase().contains(query))
        .map(|choice| (choice, current == Some(choice.id.as_str())))
        .collect()
}

/// What the panel shows in place of its list when the tab has no models to show: an
/// icon, what is missing, and for the empty favorites, how to fill them.
fn empty_list(icon: Icon, title: String, hint: Option<&str>, look: MenuLook) -> Div {
    div()
        .flex_1()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .p(px(24.))
        .whitespace_normal()
        .text_center()
        .child(icon.size(px(18.)).text_color(look.muted).mb(px(2.)))
        .child(title)
        .children(hint.map(|hint| {
            div()
                .text_xs()
                .line_height(px(16.))
                .text_color(look.muted)
                .child(SharedString::from(hint.to_string()))
        }))
}

/// The model panel: a column of tabs on the left, the favorites and then one for each
/// agent, and on the right a search box over the tab's models. The session's settings
/// of the model that are on or off, such as a fast mode, are switches along the bottom.
fn model_panel(
    panel: &ModelPanel,
    groups: &[ModelGroup],
    switches: &[Setting],
    tab: ModelTab,
    favorites: &[Favorite],
    window: &Window,
    cx: &App,
) -> AnimationElement<Div> {
    let look = panel.look;
    let typed = panel.search.read(cx).value().trim().to_string();
    let query = typed.to_lowercase();
    let is_starred = |group: &ModelGroup, choice: &SettingChoice| {
        group.kept.is_some_and(|kept| {
            favorites
                .iter()
                .any(|favorite| favorite.agent == kept && favorite.model == choice.id)
        })
    };
    let mut rows: Vec<AnyElement> = Vec::new();
    let empty = match tab {
        ModelTab::Favorites => {
            // Each agent's starred models under its name.
            for group in groups {
                let starred = listed_models(group, |choice| is_starred(group, choice), &query);
                if starred.is_empty() {
                    continue;
                }
                rows.push(panel.group_header(&group.name, rows.is_empty()));
                rows.extend(starred.into_iter().map(|(choice, current)| {
                    panel
                        .model_row(group, choice, current, true, &query)
                        .into_any_element()
                }));
            }
            if query.is_empty() {
                (
                    Icon::new(IconName::Star),
                    "No favorites yet".to_string(),
                    Some("Star a model to keep it here."),
                )
            } else {
                (
                    Icon::new(gpui_kit::assets::IconName::SearchX),
                    format!("No favorites match \u{201c}{typed}\u{201d}"),
                    None,
                )
            }
        }
        ModelTab::Agent(agent) => {
            for group in groups.iter().filter(|group| group.agent == agent) {
                rows.extend(listed_models(group, |_| true, &query).into_iter().map(
                    |(choice, current)| {
                        let starred = is_starred(group, choice);
                        panel
                            .model_row(group, choice, current, starred, &query)
                            .into_any_element()
                    },
                ));
            }
            let title = if query.is_empty() {
                "No models".to_string()
            } else {
                format!("No models match \u{201c}{typed}\u{201d}")
            };
            (Icon::new(gpui_kit::assets::IconName::SearchX), title, None)
        }
    };

    // Set apart from the list by its gray alone, with no line between them.
    let tabs = div()
        .w(TAB_COLUMN_WIDTH)
        .flex_none()
        .py(px(6.))
        .flex()
        .flex_col()
        .items_center()
        .gap(SURFACE_PADDING)
        .bg(look.rail)
        // GPUI clips the panel's content to its box, not its rounded corners.
        .rounded_l(SURFACE_RADIUS)
        .child(panel.tab(
            ModelTab::Favorites,
            Icon::new(IconName::Star),
            "Favorites".into(),
            tab == ModelTab::Favorites,
        ))
        .children(groups.iter().map(|group| {
            panel.tab(
                ModelTab::Agent(group.agent),
                agent_logo(&group.name),
                group.name.clone().into(),
                tab == ModelTab::Agent(group.agent),
            )
        }));
    // The magnifier sits in the rows' lead column and the text where their names start.
    let search = div()
        .h(SEARCH_ROW_HEIGHT)
        .flex_none()
        .pl(SURFACE_PADDING + ROW_PAD_LEFT)
        .pr(px(12.))
        .flex()
        .items_center()
        .gap(LEAD_GAP)
        .border_b_1()
        .border_color(look.line)
        .child(
            lead()
                .text_color(look.muted)
                .child(Icon::new(IconName::Search).size(LEAD_ICON_SIZE)),
        )
        .child(
            Input::new(panel.search)
                .appearance(false)
                .bordered(false)
                .focus_bordered(false)
                .small()
                .px_0()
                .text_size(MENU_TEXT_SIZE)
                .flex_1(),
        );
    let list = if rows.is_empty() {
        let (icon, title, hint) = empty;
        empty_list(icon, title, hint, look).into_any_element()
    } else {
        div()
            .id("model-panel-rows")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .p(SURFACE_PADDING)
            .flex()
            .flex_col()
            .children(rows)
            .into_any_element()
    };
    let footer = (!switches.is_empty()).then(|| {
        div()
            .flex_none()
            .p(SURFACE_PADDING)
            .border_t_1()
            .border_color(look.line)
            .flex()
            .flex_col()
            .children(switches.iter().map(|setting| {
                let on = matches!(setting.value, SettingValue::Switch(true));
                panel.switch_row(setting, on)
            }))
    });

    let surface = look
        .surface()
        .w(PANEL_WIDTH)
        .h(PANEL_HEIGHT)
        .max_h(window.viewport_size().height * 0.8)
        .overflow_hidden()
        .flex()
        // Rows are one line tall; a long name is cut short instead of wrapping.
        .whitespace_nowrap()
        .child(tabs)
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .child(search)
                .child(list)
                .children(footer),
        )
        .children(look.edge());
    rise_in(surface, "model-panel-in", window, cx)
}

/// The model's button, named for the model, with the mark of the session's agent. An
/// agent that offers no model still gets the button, for the other agents' models and
/// its switches.
fn model_button(settings: &[Setting], agent: &str, cx: &App) -> InputGroupButton {
    let name = settings
        .iter()
        .find(|setting| setting.kind == SettingKind::Model)
        .and_then(shown_choice)
        .map_or_else(|| "Model".to_string(), |choice| choice.name.clone());
    let icon = agent_logo(agent).size(PICKER_ICON_SIZE).into_any_element();
    let label = format!("Model: {name}");
    picker_button("model-picker", Some(icon), name, label, cx)
}

impl ChatView {
    /// Stars a model, or takes its star off again. Another window may have changed the
    /// favorites since this one read them, so the change is made to the list on disk,
    /// and this window takes that list up.
    fn set_favorite(&mut self, favorite: Favorite, starred: bool, cx: &mut Context<Self>) {
        let change = |favorites: &mut Vec<Favorite>| {
            favorites.retain(|known| *known != favorite);
            if starred {
                favorites.push(favorite.clone());
            }
        };
        let changed = config_folder()
            .ok_or_else(|| std::io::Error::other("there is no config folder"))
            .and_then(|folder| change_favorites(&folder, change));
        match changed {
            Ok(favorites) => self.favorites = favorites,
            Err(err) => {
                eprintln!("couldn't save the favorite models: {err}");
                // The list still changes for this run.
                change(&mut self.favorites);
            }
        }
        cx.notify();
    }

    /// Shows `tab` of the model panel.
    fn show_model_tab(&mut self, tab: ModelTab, cx: &mut Context<Self>) {
        self.model_panel.tab = tab;
        cx.notify();
    }

    /// The model's button, named for the session's model, and the panel of models that
    /// opens upward from it: the favorites, the session's own agent's models, and the
    /// other agents'. The session's settings come as `shown`.
    pub(crate) fn render_model_picker(
        &self,
        session: &Session,
        shown: &[Setting],
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        // Each agent's models, in the agents' order: the session's own agent's as
        // `shown` has them, the others' as a session on each has them, else as the
        // agent last created a session with them. An agent that is gone can't take a
        // session, so its models aren't offered.
        let groups: Vec<ModelGroup> = self
            .agents
            .iter()
            .enumerate()
            .filter_map(|(ix, slot)| {
                let own = ix == session.agent;
                if !own && slot.commands.is_closed() {
                    return None;
                }
                let settings: &[Setting] = if own {
                    shown
                } else {
                    self.model_source(ix)
                        .map_or(&slot.last_settings, |source| &source.settings)
                };
                let setting = settings
                    .iter()
                    .find(|setting| setting.kind == SettingKind::Model)?;
                Some(ModelGroup {
                    agent: ix,
                    name: slot.name.clone(),
                    kept: kept_name(&slot.name),
                    setting: setting.clone(),
                    own,
                })
            })
            .collect();
        let switches: Vec<Setting> = shown
            .iter()
            .filter(|setting| setting.kind == SettingKind::ModelConfig)
            .cloned()
            .collect();
        if groups.is_empty() && switches.is_empty() {
            return None;
        }
        let own = groups.iter().find(|group| group.own);
        // The panel opens on the session's own agent's tab, so its model is in view, or
        // on the first agent's when the session has no model to pick.
        let first_tab = own
            .or(groups.first())
            .map_or(ModelTab::Favorites, |group| ModelTab::Agent(group.agent));
        let view = cx.entity().downgrade();
        let key = session.key;
        // The tab on show may be an agent's that went away while the panel was open.
        let tab = match self.model_panel.tab {
            ModelTab::Agent(agent) if !groups.iter().any(|group| group.agent == agent) => first_tab,
            tab => tab,
        };
        let search = self.model_panel.search.clone();
        let favorites = self.favorites.clone();
        let trigger = model_button(shown, &self.agents[session.agent].name, cx).cursor_pointer();
        let picker = Popover::new("model-panel")
            .appearance(false)
            .anchor(Anchor::BottomLeft)
            .offset(MENU_OFFSET)
            .trigger(trigger)
            // Opening the panel focuses the search box, so typing narrows the list at once.
            .track_focus(&search.read(cx).focus_handle(cx))
            .on_open_change({
                let (view, search) = (view.clone(), search.clone());
                move |open, window, cx| {
                    if !*open {
                        return;
                    }
                    // It opens on `first_tab` with an empty search, each time.
                    search.update(cx, |search, cx| search.set_value(String::new(), window, cx));
                    let _ = view.update(cx, |this, cx| this.show_model_tab(first_tab, cx));
                }
            })
            .content(move |_, window, cx| {
                let popover = cx.entity();
                let panel = ModelPanel {
                    view: &view,
                    key,
                    popover: &popover,
                    search: &search,
                    look: MenuLook::new(cx),
                };
                model_panel(&panel, &groups, &switches, tab, &favorites, window, cx)
            });
        Some(picker.into_any_element())
    }
}

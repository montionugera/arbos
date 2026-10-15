//! Root view: rail · center column (tab bar, banner, transcript, composer) ·
//! right panel (browser live view, S16) · Details drawer. Layout per
//! `arbos-design.md` §3.2 and the mock.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    actions, div, prelude::*, px, Animation, AnimationExt, App, Context, Div, ElementId, Entity,
    FocusHandle, Focusable, FontWeight, Render, RenderImage, SharedString, Stateful, Window,
};

use crate::app::SignInMode;
use crate::app::{AppState, DetailsTarget};
use crate::model::{now_ms, BoxState, Chat, Id, Link, MachineState, Project};
use crate::ui::avatars;
use crate::ui::channels::{ChannelsEvent, ChannelsPanel};
use crate::ui::composer::{Composer, ComposerEvent, ComposerInput};
use crate::ui::connectors::{ConnectorsEvent, ConnectorsPanel};
use crate::ui::frost::{edge_faded, frosted, rounded_clip, MENU_BLUR, PANEL_BLUR};
use crate::ui::groups;
use crate::ui::local::{LocalEvent, LocalPanel};
use crate::ui::motion::{ease_out_cubic, PulseClock, COMPOSER_DROP_MS, SHAKE_MS};
use crate::ui::people;
use crate::ui::routines::{RoutinesEvent, RoutinesPanel};
use crate::ui::skills::SkillsPanel;
use crate::ui::theme::{
    reapply_window_background, Appearance, Surface, Theme, COMPOSER_HEIGHT, COMPOSER_WIDTH,
    DRAWER_WIDTH, PANEL_RADIUS, RAIL_TILE, RAIL_WIDTH, RAIL_WIDTH_COLLAPSED, RIGHT_PANEL_WIDTH,
    TAB_BAR_HEIGHT, TAB_MIN_WIDTH,
};
use crate::ui::transcript::{Transcript, COLUMN_MAX_WIDTH};
use crate::ui::vault::{VaultEvent, VaultPanel};
use crate::ui::voice::Voice;

actions!(shell, [CloseTab, NewTab, ToggleRail, TogglePanel, Quit]);

/// Members of a space with their decoded avatars (rail tiles, headers).
type PeopleWithAvatars = Vec<(crate::model::Person, Option<Arc<RenderImage>>)>;

pub struct Shell {
    pub(crate) state: Entity<AppState>,
    transcript: Entity<Transcript>,
    pub(crate) composer: Entity<Composer>,
    /// F11: call strip + mic/TTS owner.
    voice: Entity<Voice>,
    pairing_input: Entity<ComposerInput>,
    pub(crate) project_input: Entity<ComposerInput>,
    connectors: Entity<ConnectorsPanel>,
    skills: Entity<SkillsPanel>,
    local: Entity<LocalPanel>,
    focus_handle: FocusHandle,
    settings_open: bool,
    routines: Entity<RoutinesPanel>,
    routines_open: bool,
    channels: Entity<ChannelsPanel>,
    /// F6: the "+ project" picker under the tab header.
    group_picker_open: bool,
    vault: Entity<VaultPanel>,
    shake_started_at: Option<u64>,
    tab_scroll: gpui::ScrollHandle,
    /// Tab index to scroll into view on the next frame (new/selected tab).
    reveal_tab: Option<usize>,
    /// True while the sign-in screen is up; its clearing focuses the composer.
    signing_in: bool,
    /// People chats (D72): the `+` menu, the @search field, the header pickers.
    pub new_menu_open: bool,
    pub people_input: Entity<ComposerInput>,
    pub add_agent_open: bool,
    pub members_open: bool,
}

impl Shell {
    pub(crate) fn app_state(&self) -> &Entity<AppState> {
        &self.state
    }

    pub(crate) fn set_group_picker(&mut self, open: bool, cx: &mut Context<Self>) {
        self.group_picker_open = open;
        cx.notify();
    }

    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
        let voice = cx.new(|cx| Voice::new(state.clone(), cx));
        let composer = cx.new(|cx| Composer::new(state.clone(), voice.clone(), cx));
        {
            // Keystrokes in a people chat → presence "typing" (throttled in AppState).
            let composer_input = composer.read(cx).input.clone();
            cx.observe(&composer_input, |this, input, cx| {
                if input.read(cx).text().trim().is_empty() {
                    return;
                }
                let chat = this.state.read(cx).active_chat_id();
                if let Some(chat) = chat {
                    this.state.update(cx, |s, _| s.typed_in(&chat));
                }
            })
            .detach();
        }
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        cx.observe(&voice, |_, _, cx| cx.notify()).detach();
        cx.observe_window_activation(window, |this, window, cx| {
            let active = window.is_window_active();
            this.state
                .update(cx, |s, cx| s.set_window_active(active, cx));
        })
        .detach();
        cx.subscribe(
            &composer,
            |this, _, event: &ComposerEvent, cx| match event {
                ComposerEvent::Send(text, attachments) => {
                    let text = text.clone();
                    let attachments = attachments.clone();
                    this.state.update(cx, |s, cx| s.send(text, attachments, cx));
                }
                ComposerEvent::Stop => this.state.update(cx, |s, cx| s.stop(cx)),
            },
        )
        .detach();
        Composer::focus(&composer, window, cx);

        let pairing_input = cx.new(ComposerInput::new);
        {
            let weak = cx.weak_entity();
            pairing_input.update(cx, |input, _| {
                input.set_placeholder("One-time pairing code");
                input.on_submit = Some(Rc::new(move |_window, cx| {
                    let _ = weak.update(cx, |this, cx| this.submit_pairing(cx));
                }));
            });
        }
        let project_input = cx.new(ComposerInput::new);
        {
            let weak = cx.weak_entity();
            project_input.update(cx, |input, _| {
                input.set_placeholder("Project name");
                input.on_submit = Some(Rc::new(move |window, cx| {
                    let _ = weak.update(cx, |this, cx| this.submit_project(window, cx));
                }));
            });
        }
        cx.observe(&pairing_input, |_, _, cx| cx.notify()).detach();
        cx.observe(&project_input, |_, _, cx| cx.notify()).detach();
        let people_input = cx.new(ComposerInput::new);
        {
            let weak = cx.entity().downgrade();
            people_input.update(cx, |input, _| {
                input.set_placeholder("username");
                input.on_submit = Some(Rc::new(move |_window, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        let typed = this.people_input.read(cx).text().trim().to_string();
                        let pick = {
                            let s = this.state.read(cx);
                            s.people_results
                                .first()
                                .map(|p| p.handle.clone())
                                .or_else(|| (!typed.is_empty()).then_some(typed.clone()))
                        };
                        if let Some(handle) = pick {
                            this.state.update(cx, |s, cx| s.pick_person(handle, cx));
                        }
                    });
                }));
            });
        }
        cx.observe(&people_input, |this, input, cx| {
            let text = input.read(cx).text().trim().to_string();
            if this.state.read(cx).new_chat_open && this.state.read(cx).people_query != text {
                this.state.update(cx, |s, cx| s.search_people(text, cx));
            }
            cx.notify();
        })
        .detach();
        let connectors = cx.new(|cx| ConnectorsPanel::new(state.clone(), cx));
        cx.observe(&connectors, |_, _, cx| cx.notify()).detach();
        cx.subscribe(
            &connectors,
            |this, _, event: &ConnectorsEvent, cx| match event {
                ConnectorsEvent::Back => {
                    this.settings_open = true;
                    cx.notify();
                }
            },
        )
        .detach();
        let local = cx.new(|cx| LocalPanel::new(state.clone(), cx));
        cx.observe(&local, |_, _, cx| cx.notify()).detach();
        cx.subscribe(&local, |this, _, event: &LocalEvent, cx| match event {
            LocalEvent::Back => {
                this.settings_open = true;
                cx.notify();
            }
        })
        .detach();
        let vault = cx.new(|cx| VaultPanel::new(state.clone(), cx));
        cx.observe(&vault, |_, _, cx| cx.notify()).detach();
        cx.subscribe(&vault, |this, _, event: &VaultEvent, cx| match event {
            VaultEvent::Back => {
                this.settings_open = true;
                cx.notify();
            }
        })
        .detach();
        let routines = cx.new(|cx| RoutinesPanel::new(state.clone(), cx));
        cx.subscribe(
            &routines,
            |this, _, event: &RoutinesEvent, cx| match event {
                RoutinesEvent::Close => {
                    this.routines_open = false;
                    cx.notify();
                }
            },
        )
        .detach();
        let channels = cx.new(|cx| ChannelsPanel::new(state.clone(), cx));
        cx.observe(&channels, |_, _, cx| cx.notify()).detach();
        cx.subscribe(
            &channels,
            |this, _, event: &ChannelsEvent, cx| match event {
                ChannelsEvent::Back => {
                    this.settings_open = true;
                    cx.notify();
                }
            },
        )
        .detach();
        let skills = cx.new(|cx| SkillsPanel::new(state.clone(), cx));
        cx.observe(&skills, |_, _, cx| cx.notify()).detach();

        Self {
            state,
            transcript,
            composer,
            voice,
            pairing_input,
            project_input,
            connectors,
            skills,
            local,
            focus_handle: cx.focus_handle(),
            settings_open: false,
            group_picker_open: false,
            routines,
            routines_open: false,
            channels,
            vault,
            shake_started_at: None,
            tab_scroll: gpui::ScrollHandle::new(),
            reveal_tab: None,
            signing_in: false,
            new_menu_open: false,
            people_input,
            add_agent_open: false,
            members_open: false,
        }
    }

    fn submit_pairing(&mut self, cx: &mut Context<Self>) {
        let code = self.pairing_input.read(cx).text().trim().to_string();
        if code.is_empty() {
            return;
        }
        self.state.update(cx, |s, cx| s.pair(code, cx));
    }

    fn submit_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.project_input.read(cx).text().trim().to_string();
        if name.is_empty() {
            return;
        }
        self.project_input.update(cx, |input, cx| input.clear(cx));
        self.state.update(cx, |s, cx| s.create_project(name, cx));
        Composer::focus(&self.composer, window, cx);
    }

    fn close_tab(&mut self, _: &CloseTab, _: &mut Window, cx: &mut Context<Self>) {
        let active = self.state.read(cx).active_chat_id();
        if let Some(chat_id) = active {
            let closed = self.state.update(cx, |s, cx| s.close_tab(chat_id, cx));
            if !closed {
                self.shake_started_at = Some(now_ms());
                cx.notify();
            }
        }
    }

    fn new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        self.state.update(cx, |s, cx| s.new_tab(cx));
        Composer::focus(&self.composer, window, cx);
    }

    fn toggle_rail(&mut self, _: &ToggleRail, _: &mut Window, cx: &mut Context<Self>) {
        self.state.update(cx, |s, cx| s.toggle_rail(cx));
    }

    fn toggle_panel(&mut self, _: &TogglePanel, _: &mut Window, cx: &mut Context<Self>) {
        self.state.update(cx, |s, cx| s.toggle_right_panel(cx));
    }

    fn set_theme(&mut self, appearance: Appearance, surface: Surface, cx: &mut Context<Self>) {
        cx.set_global(Theme::with(appearance, surface));
        reapply_window_background(cx);
        cx.notify();
    }

    // ----- rail ------------------------------------------------------------

    fn render_rail(&self, collapsed: bool, cx: &mut Context<Self>) -> Div {
        let theme = Theme::of(cx).clone();
        let state = self.state.read(cx);
        let selected = state.selected_project.clone();
        let people_tiles: Vec<(Project, Option<Arc<RenderImage>>, usize, bool)> = state
            .people_spaces()
            .map(|p| {
                (
                    p.clone(),
                    p.person().and_then(|x| state.avatar(&x.avatar_url)),
                    state.project_unread(&p.id),
                    state.typing_in(&p.id).is_some(),
                )
            })
            .collect();
        let projects: Vec<(Project, bool, usize)> = state
            .projects
            .iter()
            .filter(|p| !p.is_people())
            .map(|p| {
                (
                    p.clone(),
                    state.project_is_streaming(&p.id),
                    state.project_unread(&p.id),
                )
            })
            .collect();
        // Tree-character avatars (own choice or hash pick) and what they are doing.
        let avatar_slugs: HashMap<Id, Option<String>> = projects
            .iter()
            .map(|(p, _, _)| (p.id.clone(), state.avatar_slug(p)))
            .collect();
        let avatar_states: HashMap<Id, avatars::AvatarState> = projects
            .iter()
            .map(|(p, _, _)| (p.id.clone(), state.avatar_state(&p.id)))
            .collect();
        // People in each project (D72 step 3) with their decoded avatars.
        let project_people: HashMap<Id, PeopleWithAvatars> = projects
            .iter()
            .map(|(p, _, _)| {
                let shown: Vec<_> = if p.is_guest() {
                    p.owner().cloned().into_iter().collect()
                } else {
                    p.guests().cloned().collect()
                };
                (
                    p.id.clone(),
                    shown
                        .into_iter()
                        .map(|person| {
                            let img = state.avatar(&person.avatar_url);
                            (person, img)
                        })
                        .collect(),
                )
            })
            .collect();
        let width = if collapsed {
            RAIL_WIDTH_COLLAPSED
        } else {
            RAIL_WIDTH
        };

        let mut tiles = div()
            .id("rail-tiles")
            .flex()
            .flex_col()
            .items_center()
            .gap(px(10.0))
            .pt(px(if cfg!(target_os = "macos") {
                44.0
            } else {
                10.0
            }))
            .flex_1()
            .overflow_y_scroll();
        for (project, streaming, unread) in projects {
            let is_selected = selected.as_deref() == Some(project.id.as_str());
            let id = project.id.clone();
            if streaming {
                PulseClock::request(cx);
            }
            let tile_size = if collapsed { 36.0 } else { RAIL_TILE };
            let mut tile = div()
                .id(ElementId::Name(SharedString::from(format!(
                    "tile-{}",
                    project.id
                ))))
                .relative()
                .w(px(tile_size))
                .h(px(if collapsed { 36.0 } else { RAIL_TILE - 6.0 }))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(4.0))
                .rounded(px(PANEL_RADIUS))
                .border_1()
                .border_color(if is_selected {
                    theme.accent
                } else {
                    theme.border
                })
                .bg(if is_selected {
                    theme.surface_2
                } else {
                    theme.surface_1
                })
                .cursor_pointer()
                .hover(|s| s.border_color(theme.border_strong))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.state
                        .update(cx, |s, cx| s.select_project(id.clone(), cx));
                    Composer::focus(&this.composer, window, cx);
                }));
            match avatar_slugs.get(&project.id).cloned().flatten() {
                Some(slug) => {
                    let avatar_state = avatar_states
                        .get(&project.id)
                        .copied()
                        .unwrap_or(avatars::AvatarState::Idle);
                    if avatar_state != avatars::AvatarState::Idle {
                        PulseClock::request(cx);
                    }
                    tile = tile.child(avatars::element(
                        Some(&slug),
                        avatar_state,
                        if collapsed { 26.0 } else { 40.0 },
                        &project.icon,
                        groups::project_color(&project.color, theme.accent),
                        &theme,
                    ));
                }
                None => {
                    tile = tile.child(
                        div()
                            .text_size(px(if collapsed { 16.0 } else { 20.0 }))
                            .text_color(theme.text)
                            .child(project.icon.clone()),
                    );
                }
            }
            if !collapsed {
                tile = tile.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.text)
                        .child(project.name.clone()),
                );
            }
            // People in this project (D72 step 3): a small avatar cluster.
            if let Some(shown) = project_people.get(&project.id).filter(|v| !v.is_empty()) {
                tile = tile.child(
                    div()
                        .absolute()
                        .bottom(px(-4.0))
                        .right(px(-4.0))
                        .child(people::avatar_stack(shown, 14.0, 2, &theme)),
                );
            }
            // Workspace state: a tiny dot only (the machine has its own indicator).
            let ws_dot = match project.state {
                BoxState::Awake => None,
                BoxState::Waking => Some(theme.warn),
                BoxState::HibernatedLight | BoxState::HibernatedDeep => Some(theme.text_faint),
                BoxState::StoppedByUser => Some(theme.text_muted),
                BoxState::Error => Some(theme.danger),
            };
            if let (Some(c), false) = (ws_dot, streaming) {
                tile = tile.child(
                    div()
                        .absolute()
                        .top(px(5.0))
                        .right(px(6.0))
                        .size(px(5.0))
                        .rounded_full()
                        .bg(c),
                );
            }
            if streaming {
                let phase = PulseClock::phase(cx, 1200);
                tile = tile.child(
                    div()
                        .absolute()
                        .top(px(5.0))
                        .left(px(6.0))
                        .size(px(6.0))
                        .rounded_full()
                        .bg(Theme::with_alpha(theme.accent, 0.4 + 0.6 * phase)),
                );
            } else if unread > 0 {
                tile = tile.child(
                    div()
                        .absolute()
                        .top(px(-4.0))
                        .right(px(-4.0))
                        .min_w(px(16.0))
                        .h(px(16.0))
                        .px(px(4.0))
                        .rounded_full()
                        .bg(theme.accent)
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(10.0))
                        .text_color(theme.bg)
                        .font_weight(FontWeight::BOLD)
                        .child(unread.to_string()),
                );
            }
            tiles = tiles.child(tile);
        }
        if !people_tiles.is_empty() {
            tiles = tiles.child(
                div()
                    .w(px(if collapsed { 24.0 } else { RAIL_TILE - 20.0 }))
                    .h(px(1.0))
                    .my(px(2.0))
                    .bg(theme.border),
            );
        }
        for (space, image, unread, typing) in people_tiles {
            let is_selected = selected.as_deref() == Some(space.id.as_str());
            if typing {
                PulseClock::request(cx);
            }
            tiles = tiles.child(people::person_tile(
                &space,
                image,
                is_selected,
                collapsed,
                unread,
                typing,
                &theme,
                cx,
            ));
        }

        // Bottom controls: same footprint and spacing as the tiles, centred on
        // the same axis (§3.3).
        let bottom_button = |id: &'static str, glyph: &'static str, size_px: f32, theme: &Theme| {
            div()
                .id(id)
                .w(px(if collapsed { 36.0 } else { RAIL_TILE }))
                .h(px(32.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(PANEL_RADIUS))
                .text_size(px(size_px))
                .text_color(theme.text_muted)
                .hover(|s| s.bg(theme.surface_2).text_color(theme.text))
                .cursor_pointer()
                .child(glyph)
        };
        let bottom = div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(10.0))
            .pt(px(6.0))
            .pb(px(12.0))
            .child(self.render_machine_indicator(collapsed, cx))
            .child(
                bottom_button("rail-new-project", "+", 20.0, &theme).on_click(cx.listener(
                    |this, _, _, cx| {
                        let any_open = this.new_menu_open
                            || this.state.read(cx).new_project_open
                            || this.state.read(cx).new_chat_open;
                        if any_open {
                            this.new_menu_open = false;
                            this.state.update(cx, |s, cx| {
                                s.new_project_open = false;
                                s.close_new_chat(cx);
                            });
                        } else {
                            this.new_menu_open = true;
                        }
                        cx.notify();
                    },
                )),
            )
            .children(self.render_update_button(collapsed, cx))
            .child(
                // Toggle on the press itself: one event per press on every
                // platform, so a click can never open and shut in one go.
                bottom_button("rail-settings", "⚙", 17.0, &theme).on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.settings_open = !this.settings_open;
                        cx.notify();
                    }),
                ),
            );

        div()
            .relative()
            .h_full()
            .w(px(width))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .bg(theme.chrome())
            .border_r_1()
            .border_color(theme.border)
            .child(edge_faded(18.0, true, true, tiles))
            .child(bottom)
    }

    /// The Update button (D42): absent while up to date; appears the moment
    /// the feed has a newer build; one click downloads, verifies, swaps the
    /// bundle, and relaunches. It sits just above the settings gear.
    fn render_update_button(
        &self,
        collapsed: bool,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Stateful<Div>> {
        let theme = Theme::of(cx).clone();
        let update = self.state.read(cx).update.clone();
        if !update.button_visible() {
            return None;
        }
        let busy = update.busy();
        let failed = matches!(update.phase, crate::update::UpdatePhase::Failed { .. });
        if busy {
            PulseClock::request(cx);
        }
        let phase = if busy {
            PulseClock::phase(cx, 1400)
        } else {
            1.0
        };
        let bg = if failed {
            theme.danger_soft
        } else {
            Theme::with_alpha(theme.accent, 0.55 + 0.45 * phase)
        };
        let fg = if failed { theme.text } else { theme.bg };
        let label = update.button_label();
        let mut button = div()
            .id("rail-update")
            .w(px(if collapsed { 36.0 } else { RAIL_TILE }))
            .h(px(32.0))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(5.0))
            .rounded(px(PANEL_RADIUS))
            .bg(bg)
            .text_color(fg)
            .text_size(px(12.0))
            .font_weight(FontWeight::SEMIBOLD)
            .when(!busy, |el| {
                el.cursor_pointer()
                    .hover(|s| s.bg(Theme::with_alpha(theme.accent, 0.85)))
            })
            .on_click(cx.listener(|this, _, _, cx| {
                this.state.update(cx, |s, cx| s.install_update(cx));
            }));
        button = button.child(
            div()
                .text_size(px(14.0))
                .child(if failed { "!" } else { "↑" }),
        );
        if !collapsed {
            button = button.child(div().child(label));
        }
        Some(button)
    }

    // ----- tab bar -----------------------------------------------------------

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::of(cx).clone();
        let state = self.state.read(cx);
        let project = state.selected().cloned();
        let tabs: Vec<(Chat, bool, bool)> = project
            .as_ref()
            .map(|p| {
                state
                    .tabs(&p.id)
                    .iter()
                    .map(|c| {
                        (
                            c.clone(),
                            state.active_tab.get(&p.id) == Some(&c.id),
                            state.chat_is_streaming(&c.id),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let rail_collapsed = state.rail_collapsed;
        let panel_open = state.right_panel_open;
        let in_call = self.voice.read(cx).in_call();
        let projects = state.projects.clone();
        let pinned_avatar = project.as_ref().filter(|p| !p.is_people()).and_then(|p| {
            state
                .avatar_slug(p)
                .map(|slug| (slug, state.avatar_state(&p.id)))
        });
        let can_group =
            project.as_ref().map(|p| !p.is_guest()).unwrap_or(false) && projects.len() > 1;
        let picker_open = self.group_picker_open;

        let shake = self
            .shake_started_at
            .map(|t| now_ms().saturating_sub(t))
            .filter(|dt| *dt < SHAKE_MS);
        if shake.is_some() {
            PulseClock::request(cx);
        }

        let tab_count = tabs.len();
        // The strip holds only the closable tabs; the pinned tab stays put.
        let active_ix = tabs
            .iter()
            .position(|(_, active, _)| *active)
            .map(|ix| ix.saturating_sub(1));
        let mut pinned_tab = None;
        let mut strip = div()
            .id("tab-strip")
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap(px(2.0))
            .overflow_x_scroll()
            .track_scroll(&self.tab_scroll);
        if let Some(ix) = self.reveal_tab.map(|ix| ix.saturating_sub(1)).or(active_ix) {
            // Items in the strip: closable tabs then "+"; clamp to the last tab.
            self.tab_scroll
                .scroll_to_item(ix.min(tab_count.saturating_sub(2)));
        }
        for (chat, active, streaming) in tabs {
            let id = chat.id.clone();
            let close_id = chat.id.clone();
            if streaming {
                PulseClock::request(cx);
            }
            let mut tab = div()
                .id(ElementId::Name(SharedString::from(format!(
                    "tab-{}",
                    chat.id
                ))))
                .group("tab")
                .relative()
                .flex()
                .items_center()
                .gap(px(6.0))
                .h(px(28.0))
                .when(!chat.is_pinned, |el| el.w(px(176.0)).flex_shrink(1.0))
                .when(chat.is_pinned, |el| el.flex_shrink_0())
                .min_w(px(if chat.is_pinned { 0.0 } else { TAB_MIN_WIDTH }))
                .px(px(10.0))
                .rounded(px(7.0))
                .bg(if active {
                    theme.surface_2
                } else {
                    gpui::transparent_black()
                })
                .text_size(px(12.5))
                .text_color(if active { theme.text } else { theme.text_muted })
                .cursor_pointer()
                .hover(|s| s.bg(theme.surface_1))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.state.update(cx, |s, cx| s.select_tab(id.clone(), cx));
                    Composer::focus(&this.composer, window, cx);
                }));
            if chat.is_pinned {
                let shake_offset = shake
                    .map(|dt| {
                        ((dt as f32 / SHAKE_MS as f32) * std::f32::consts::PI * 4.0).sin() * 3.0
                    })
                    .unwrap_or(0.0);
                tab =
                    tab.left(px(shake_offset))
                        .child(match (&project, pinned_avatar.clone()) {
                            (Some(p), Some((slug, st))) => {
                                if st != avatars::AvatarState::Idle {
                                    PulseClock::request(cx);
                                }
                                avatars::element(
                                    Some(&slug),
                                    st,
                                    20.0,
                                    &p.icon,
                                    groups::project_color(&p.color, theme.accent),
                                    &theme,
                                )
                            }
                            _ => div().text_size(px(14.0)).text_color(theme.text).child(
                                project.as_ref().map(|p| p.icon.clone()).unwrap_or_default(),
                            ),
                        });
            }
            if streaming {
                let phase = PulseClock::phase(cx, 1200);
                tab = tab.child(
                    div()
                        .size(px(6.0))
                        .rounded_full()
                        .bg(Theme::with_alpha(theme.accent, 0.4 + 0.6 * phase)),
                );
            }
            let title = if chat.is_pinned {
                project
                    .as_ref()
                    .map(|p| p.name.clone())
                    .unwrap_or(chat.title.clone())
            } else {
                chat.title.clone()
            };
            if let Some(glyphs) = groups::tab_glyphs(&chat, &projects, &theme) {
                tab = tab.child(glyphs);
            }
            tab = tab.child(
                div()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .max_w(px(180.0))
                    .when(chat.unread && !active, |el| {
                        el.font_weight(FontWeight::SEMIBOLD).text_color(theme.text)
                    })
                    .child(title),
            );
            if !chat.is_pinned {
                tab = tab.child(
                    div()
                        .id(ElementId::Name(SharedString::from(format!(
                            "close-{}",
                            chat.id
                        ))))
                        .size(px(16.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.0))
                        .text_size(px(12.0))
                        .text_color(theme.text_faint)
                        .invisible()
                        .group_hover("tab", |s| s.visible())
                        .hover(|s| s.bg(theme.border_strong).text_color(theme.text))
                        .child("×")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.state.update(cx, |s, cx| {
                                s.close_tab(close_id.clone(), cx);
                            });
                        })),
                );
            }
            if chat.is_pinned {
                pinned_tab = Some(tab);
            } else {
                strip = strip.child(tab);
            }
        }
        // "+" sits right after the last tab (§3.4), inside the scrolling strip.
        strip = strip.child(
            div()
                .id("new-tab")
                .flex_shrink_0()
                .size(px(26.0))
                .ml(px(2.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.0))
                .text_size(px(18.0))
                .text_color(theme.text_muted)
                .hover(|s| s.bg(theme.surface_2))
                .cursor_pointer()
                .child("+")
                .on_click(cx.listener(|this, _, window, cx| {
                    this.state.update(cx, |s, cx| s.new_tab(cx));
                    let n = this
                        .state
                        .read(cx)
                        .selected_project
                        .as_deref()
                        .map(|p| this.state.read(cx).tabs(p).len())
                        .unwrap_or(1);
                    this.reveal_tab = Some(n.saturating_sub(1));
                    Composer::focus(&this.composer, window, cx);
                })),
        );

        let icon_button = |id: &'static str, glyph: &'static str, active: bool, theme: &Theme| {
            div()
                .id(id)
                .size(px(26.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.0))
                .text_size(px(14.0))
                .text_color(if active { theme.text } else { theme.text_muted })
                .hover(|s| s.bg(theme.surface_2))
                .cursor_pointer()
                .child(glyph)
        };

        div()
            .h(px(TAB_BAR_HEIGHT))
            .flex_shrink_0()
            .w_full()
            .flex()
            .items_center()
            .gap(px(6.0))
            .px(px(8.0))
            .bg(theme.chrome())
            .border_b_1()
            .border_color(theme.border)
            .window_control_area(gpui::WindowControlArea::Drag)
            .child(
                icon_button("rail-toggle", "☰", !rail_collapsed, &theme).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.state.update(cx, |s, cx| s.toggle_rail(cx));
                    },
                )),
            )
            .children(pinned_tab)
            .child(strip)
            .when_some(project.as_ref().filter(|p| !p.is_people()), |el, p| {
                el.child(people::project_controls(
                    p,
                    &self.state,
                    self.members_open,
                    cx,
                ))
            })
            .when(can_group, |el| {
                el.child(
                    groups::header_button(picker_open, &theme).on_click(cx.listener(
                        |this, _, _, cx| {
                            let open = !this.group_picker_open;
                            this.set_group_picker(open, cx);
                        },
                    )),
                )
            })
            .child(
                icon_button("call-toggle", "☎", in_call, &theme).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.voice.update(cx, |v, cx| v.toggle_call(cx));
                    },
                )),
            )
            .child(
                icon_button("panel-toggle", "▣", panel_open, &theme).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.state.update(cx, |s, cx| s.toggle_right_panel(cx));
                    },
                )),
            )
    }

    // ----- banners (S11–S14) ------------------------------------------------

    /// One machine-level banner, only while the user's machine is waking or
    /// errored (and S14 while the relay link is down). Workspace state never
    /// banners: it is the tiny dot on the tab/rail.
    fn render_banner(&self, cx: &mut Context<Self>) -> Option<Div> {
        let theme = Theme::of(cx).clone();
        let state = self.state.read(cx);
        let link = state.link;
        let machine = state.machine.clone();
        state.selected()?;

        let banner = |bg: gpui::Hsla, fg: gpui::Hsla| {
            div()
                .w_full()
                .flex()
                .items_center()
                .gap(px(10.0))
                .px(px(16.0))
                .py(px(6.0))
                .bg(bg)
                .text_size(px(12.0))
                .text_color(fg)
        };

        if link == Link::Reconnecting {
            return Some(
                banner(theme.surface_1, theme.text_muted)
                    .child(cards_spinner(&theme, cx))
                    .child("Reconnecting… the transcript stays readable from your local replica."),
            );
        }

        match machine.shown_state() {
            MachineState::Waking => {
                PulseClock::request(cx);
                let detail = machine
                    .detail
                    .clone()
                    .unwrap_or_else(|| "Starting your machine…".to_string());
                let mut b = banner(theme.surface_1, theme.text)
                    .child(cards_spinner(&theme, cx))
                    .child(detail);
                if let Some(progress) = machine.progress {
                    b = b.child(
                        div()
                            .flex_1()
                            .h(px(4.0))
                            .rounded_full()
                            .bg(theme.border)
                            .child(
                                div()
                                    .h_full()
                                    .rounded_full()
                                    .bg(theme.accent)
                                    .w(gpui::relative(progress.clamp(0.02, 1.0))),
                            ),
                    );
                } else {
                    b = b.child(div().flex_1());
                }
                Some(
                    b.child(
                        div()
                            .text_color(theme.text_faint)
                            .child("messages are queued"),
                    ),
                )
            }
            MachineState::Error => Some(
                banner(theme.danger_soft, theme.text)
                    .child(div().text_color(theme.danger).child("✗"))
                    .child(
                        machine
                            .detail
                            .clone()
                            .unwrap_or_else(|| "Your machine could not start.".to_string()),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .id("banner-retry")
                            .text_color(theme.accent)
                            .cursor_pointer()
                            .child("Retry")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.state.update(cx, |s, cx| s.machine_start(cx));
                            })),
                    ),
            ),
            MachineState::Asleep
            | MachineState::Awake
            | MachineState::Stopped
            | MachineState::Unknown => None,
        }
    }

    /// Rail footer: the machine indicator (dot + state word); hover shows
    /// spec, uptime and spend; click opens Settings (Machine section).
    fn render_machine_indicator(&self, collapsed: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = Theme::of(cx).clone();
        let state = self.state.read(cx);
        let machine = state.machine.clone();
        let hover = state.machine_hover;
        let shown = machine.shown_state();
        let color = match shown {
            MachineState::Awake => theme.accent,
            MachineState::Waking => theme.warn,
            MachineState::Asleep | MachineState::Unknown => theme.text_faint,
            MachineState::Stopped => theme.text_muted,
            MachineState::Error => theme.danger,
        };
        if shown == MachineState::Waking {
            PulseClock::request(cx);
        }
        let phase = PulseClock::phase(cx, 1100);
        let dot_alpha = if shown == MachineState::Waking {
            0.35 + 0.65 * phase
        } else {
            1.0
        };
        let mut tile = div()
            .id("machine-indicator")
            .relative()
            .w(px(if collapsed { 36.0 } else { RAIL_TILE }))
            .h(px(22.0))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(6.0))
            .rounded(px(6.0))
            .cursor_pointer()
            .hover(|s| s.bg(theme.surface_2))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                this.state.update(cx, |s, cx| {
                    s.machine_hover = *hovered;
                    cx.notify();
                });
            }))
            .on_click(cx.listener(|this, _, _, cx| {
                this.settings_open = true;
                cx.notify();
            }))
            .child(
                div()
                    .size(px(7.0))
                    .rounded_full()
                    .bg(Theme::with_alpha(color, dot_alpha)),
            );
        if !collapsed {
            tile = tile.child(
                div()
                    .text_size(px(10.5))
                    .text_color(theme.text_faint)
                    .child(shown.label()),
            );
        }
        if hover {
            let uptime = machine
                .since
                .filter(|_| shown == MachineState::Awake)
                .map(|since| fmt_uptime(now_ms().saturating_sub(since)))
                .unwrap_or_else(|| "—".into());
            let card = div()
                .absolute()
                .left(px(if collapsed { 44.0 } else { RAIL_TILE + 8.0 }))
                .bottom(px(0.0))
                .w(px(230.0))
                .flex()
                .flex_col()
                .gap(px(4.0))
                .p(px(10.0))
                .rounded(px(PANEL_RADIUS))
                .bg(theme.card())
                .border_1()
                .border_color(theme.border_strong)
                .shadow_lg()
                .text_size(px(11.5))
                .text_color(theme.text)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .child(div().size(px(7.0)).rounded_full().bg(color))
                        .child(format!("Machine · {}", shown.label())),
                )
                .child(
                    div().text_color(theme.text_muted).child(
                        machine
                            .spec
                            .clone()
                            .unwrap_or_else(|| "spec unknown".into()),
                    ),
                )
                .when_some(machine.class.clone(), |el, c| {
                    el.child(div().text_color(theme.text_faint).child(c))
                })
                .child(
                    div()
                        .text_color(theme.text_muted)
                        .child(format!("Uptime {uptime}")),
                )
                .child(
                    div()
                        .text_color(theme.text_muted)
                        .child(machine_spend_line(&machine)),
                )
                .when_some(machine.detail.clone(), |el, d| {
                    el.child(div().text_color(theme.text_faint).child(d))
                });
            tile = tile.child(frosted(PANEL_RADIUS, MENU_BLUR, card));
        }
        tile
    }

    // ----- center column ------------------------------------------------------

    fn render_center(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::of(cx).clone();
        let state = self.state.read(cx);
        let project = state.selected().cloned();
        let chat_id = state.active_chat_id();
        let ui = chat_id
            .as_deref()
            .map(|id| state.chat_ui(id))
            .unwrap_or_default();
        let has_messages = chat_id
            .as_deref()
            .map(|id| !state.messages_of(id).is_empty())
            .unwrap_or(false);

        let dropped = ui.composer_dropped || has_messages;
        let drop_elapsed = ui
            .drop_started_at
            .map(|t| now_ms().saturating_sub(t))
            .unwrap_or(COMPOSER_DROP_MS + 1);
        let animating = dropped && drop_elapsed < COMPOSER_DROP_MS;
        if animating {
            PulseClock::request(cx);
        }

        let composer_block = div()
            .w_full()
            .max_w(px(COMPOSER_WIDTH.max(COLUMN_MAX_WIDTH - 56.0)))
            .child(self.composer.clone());

        let mut column = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(frosted(0.0, PANEL_BLUR, self.render_tab_bar(cx)));
        if let Some(space) = project.as_ref().filter(|p| p.is_people()) {
            column = column.child(people::header(
                space,
                &self.state,
                self.add_agent_open,
                self.members_open,
                cx,
            ));
        } else if let Some(banner) = self.render_banner(cx) {
            column = column.child(banner);
        }

        if project.is_none() {
            return column.child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(10.0))
                    .px(px(28.0))
                    .child(
                        div()
                            .text_size(px(22.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child("No projects yet"),
                    )
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(theme.text_muted)
                            .child(
                                "Press + in the rail. Each project is a workspace on your machine.",
                            ),
                    ),
            );
        }
        if !dropped {
            // S1/S2: empty chat, composer centered with the project name and a hint.
            let is_people = project.as_ref().map(|p| p.is_people()).unwrap_or(false);
            let name = project
                .as_ref()
                .map(|p| {
                    if p.is_people() {
                        format!("@{}", p.name)
                    } else {
                        p.name.clone()
                    }
                })
                .unwrap_or_default();
            let hint = if is_people {
                "Say hi. It lands for both of you at once; + Add agent brings one of yours in."
                    .to_string()
            } else {
                format!("Ask {name} anything. It will plan, delegate, or answer.")
            };
            column = column.child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(14.0))
                    .px(px(28.0))
                    .child(
                        div()
                            .text_size(px(22.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(name.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(theme.text_muted)
                            .child(hint),
                    )
                    .child(
                        div()
                            .w(px(COMPOSER_WIDTH))
                            .max_w_full()
                            .pt(px(8.0))
                            .child(self.composer.clone()),
                    ),
            );
        } else {
            // Transcript above, composer pinned at the bottom. On the first
            // send the composer slides down from the center over 220 ms using
            // a relative `top` inset (GPUI has no transform).
            let body = div()
                .flex_1()
                .min_h_0()
                .relative()
                .child(self.transcript.clone());
            let typing = self
                .state
                .read(cx)
                .active_chat_id()
                .and_then(|id| self.state.read(cx).typing_in(&id));
            if typing.is_some() {
                PulseClock::request(cx);
            }
            let composer_row = div()
                .w_full()
                .flex()
                .flex_col()
                .items_center()
                .px(px(28.0))
                .pb(px(18.0))
                .pt(px(8.0))
                .when_some(typing, |el, handle| {
                    el.child(people::typing_line(&handle, &theme))
                })
                .child(composer_block);
            let composer_row = if animating {
                composer_row
                    .relative()
                    .with_animation(
                        "composer-drop",
                        Animation::new(Duration::from_millis(COMPOSER_DROP_MS))
                            .with_easing(ease_out_cubic),
                        |el, t| el.top(px(-240.0 * (1.0 - t))),
                    )
                    .into_any_element()
            } else {
                composer_row.into_any_element()
            };
            // F11 call strip renders above the composer only during a call.
            column = column
                .child(body)
                .child(self.voice.clone())
                .child(composer_row);
        }
        column
    }

    // ----- right panel (S16) and Details drawer --------------------------------

    fn render_right_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let state = self.state.read(cx);
        let live = state.live_browser();
        let frame = state.browser_frame.clone();
        let frames_seen = state.browser_frames_seen;
        if frame.is_some() || live.is_some() {
            PulseClock::request(cx);
        }
        let mut panel = div()
            .w(px(RIGHT_PANEL_WIDTH))
            .flex_shrink_0()
            .h_full()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(theme.border)
            .bg(theme.chrome());
        panel = panel.child(
            div()
                .h(px(TAB_BAR_HEIGHT))
                .flex()
                .items_center()
                .justify_between()
                .px(px(12.0))
                .border_b_1()
                .border_color(theme.border)
                .text_size(px(12.5))
                .text_color(theme.text)
                .child("Browser live view")
                .child(
                    div()
                        .id("panel-close")
                        .text_color(theme.text_faint)
                        .cursor_pointer()
                        .child("×")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.state.update(cx, |s, cx| s.toggle_right_panel(cx));
                        })),
                ),
        );
        let (title, url, action) = live.unwrap_or_default();
        let body = match frame {
            Some(image) => div()
                .flex()
                .flex_col()
                .gap(px(10.0))
                .p(px(12.0))
                .child(rounded_clip(
                    8.0,
                    div()
                        .w_full()
                        .h(px(220.0))
                        .rounded(px(8.0))
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.code_bg)
                        .child(gpui::img(image).w_full().h_full().object_fit(gpui::ObjectFit::Contain)),
                ))
                .child(div().text_size(px(12.5)).text_color(theme.text).child(if title.is_empty() { "Live".to_string() } else { title }))
                .child(div().text_size(px(11.0)).text_color(theme.text_faint).child(url))
                .child(
                    div()
                        .text_size(px(11.5))
                        .text_color(theme.text_muted)
                        .child(format!("frames: {frames_seen} · last action: {action}")),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.text_faint)
                        .child("Watch only in v1 — the agent drives the page."),
                ),
            None => div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(8.0))
                .p(px(16.0))
                .text_size(px(12.5))
                .text_color(theme.text_muted)
                .child(if title.is_empty() {
                    "No browser session is running.".to_string()
                } else {
                    format!("Connecting to the box stream for “{title}”…")
                })
                .child(
                    div()
                        .text_size(px(11.5))
                        .text_color(theme.text_faint)
                        .text_center()
                        .child("When a subagent opens the browser, agentd's /browser/stream frames show here."),
                ),
        };
        frosted(0.0, PANEL_BLUR, panel.child(body))
    }

    fn render_details(&self, target: &DetailsTarget, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let state = self.state.read(cx);
        let asleep = state.box_state().map(|s| s.is_asleep()).unwrap_or(false);
        let mut sections: Vec<(String, Vec<(String, String)>)> = Vec::new();
        let mut preview: Option<String> = None;
        let mut blob_key: Option<String> = None;
        let title = match target {
            DetailsTarget::Message {
                chat_id,
                message_id,
            } => {
                if let Some(m) = state
                    .messages_of(chat_id)
                    .iter()
                    .find(|m| &m.id == message_id)
                {
                    let mut rows = Vec::new();
                    rows.push(("Message".into(), m.id.clone()));
                    rows.push(("Status".into(), format!("{:?}", m.status)));
                    rows.push((
                        "Routing decision".into(),
                        m.decision
                            .clone()
                            .or(m.turn.map(|t| format!("{t:?}").to_lowercase()))
                            .unwrap_or_else(|| "—".into()),
                    ));
                    if let Some(model) = &m.model {
                        rows.push(("Model".into(), model.clone()));
                    }
                    if let Some(s) = &m.subagent_id {
                        rows.push(("Subagent".into(), s.clone()));
                    }
                    if let Some(r) = &m.in_reply_to {
                        rows.push(("In reply to".into(), r.clone()));
                    }
                    sections.push(("Turn".into(), rows));
                    let usage = m.usage.clone().unwrap_or(serde_json::Value::Null);
                    let mut tokens = json_rows(&usage);
                    if tokens.is_empty() {
                        tokens.push(("Tokens".into(), "not reported yet".into()));
                    }
                    sections.push(("Context pack · tokens".into(), tokens));
                    let pulls: Vec<(String, String)> = m
                        .tool_parts()
                        .filter_map(|p| match p {
                            crate::model::Part::Tool {
                                call,
                                output_summary,
                                output_ref,
                                ..
                            } => Some((
                                format!("{} {}", call.name, call.call_input_short()),
                                match output_ref {
                                    Some(r) => format!("{output_summary} · blob {r}"),
                                    None => output_summary.clone(),
                                },
                            )),
                            _ => None,
                        })
                        .collect();
                    sections.push(("Pull log · tool calls".into(), pulls));
                    let steps: Vec<(String, String)> = m
                        .parts
                        .iter()
                        .chain(m.hidden_parts.iter())
                        .filter_map(|p| match p {
                            crate::model::Part::Status { code, message, .. } => {
                                Some((format!("{code:?}").to_lowercase(), message.clone()))
                            }
                            _ => None,
                        })
                        .collect();
                    if !steps.is_empty() {
                        sections.push(("Steps · status events".into(), steps));
                    }
                    if let Some(name) = &m.subagent_name {
                        sections.push(("Subagent".into(), vec![("Name".into(), name.clone())]));
                    }
                    // S18: one audit row per card, requested → decided (feature `approve`).
                    let approvals: Vec<(String, String)> = m
                        .parts
                        .iter()
                        .chain(m.hidden_parts.iter())
                        .filter_map(|p| match p {
                            crate::model::Part::Approval {
                                approval_id,
                                title,
                                risk,
                                state,
                                decided_by,
                                decided_at,
                                reason,
                                ..
                            } => {
                                let mut detail = format!(
                                    "approval_requested · {} · {} · {}",
                                    risk.label(),
                                    title,
                                    approval_id
                                );
                                if *state != crate::model::ApprovalState::Pending {
                                    detail.push_str(&format!(
                                        "\napproval_decided · {}{}{}",
                                        state.label().to_lowercase(),
                                        decided_by
                                            .as_deref()
                                            .map(|d| format!(" · by {d}"))
                                            .unwrap_or_default(),
                                        reason
                                            .as_deref()
                                            .filter(|r| !r.trim().is_empty())
                                            .map(|r| format!(" · {r}"))
                                            .unwrap_or_default()
                                    ));
                                    if let Some(at) = decided_at {
                                        let mins =
                                            crate::model::now_ms().saturating_sub(*at) / 60_000;
                                        detail.push_str(&format!(" · {mins} min ago"));
                                    }
                                }
                                Some((state.label().to_string(), detail))
                            }
                            _ => None,
                        })
                        .collect();
                    if !approvals.is_empty() {
                        sections.push(("Approvals · audit".into(), approvals));
                    }
                    if let Some(c) = &m.return_contract {
                        sections.push(("Return contract".into(), json_rows(c)));
                    }
                    if let Some((rows, text)) = crate::ui::skills::details_section(&m.hidden_parts)
                    {
                        sections.push(("Relevant skills (injected)".into(), rows));
                        preview = Some(text);
                    }
                    for p in m.parts.iter().chain(m.hidden_parts.iter()) {
                        if let crate::model::Part::SubagentResult {
                            text,
                            status,
                            duration_ms,
                            ..
                        } = p
                        {
                            sections.push((
                                "Subagent result (raw)".into(),
                                vec![
                                    (
                                        "Status".into(),
                                        status.clone().unwrap_or_else(|| "—".into()),
                                    ),
                                    (
                                        "Duration".into(),
                                        duration_ms
                                            .map(|d| format!("{:.1} s", d as f64 / 1000.0))
                                            .unwrap_or_else(|| "—".into()),
                                    ),
                                ],
                            ));
                            let mut head: String = text.chars().take(4_000).collect();
                            if text.len() > head.len() {
                                head.push_str("\n…");
                            }
                            preview = Some(head);
                        }
                    }
                }
                if let Some(doc) = state.selected_doc() {
                    if !doc.threads.is_empty() {
                        sections.push((
                            "Threads · T1 index".into(),
                            doc.threads
                                .iter()
                                .take(6)
                                .map(|t| (format!("{} · {}", t.name, t.status), t.summary.clone()))
                                .collect(),
                        ));
                    }
                }
                "Turn details".to_string()
            }
            DetailsTarget::Blob(key) => {
                blob_key = Some(key.clone());
                let mut rows = vec![("Blob".into(), key.clone())];
                match state.blobs.get(key) {
                    Some(blob) => {
                        rows.push(("Type".into(), blob.mime.clone()));
                        rows.push(("Size".into(), format!("{} bytes", blob.bytes.len())));
                        if let Some(p) = &blob.saved_to {
                            rows.push(("Saved to".into(), p.display().to_string()));
                        }
                        if blob.mime.starts_with("text/")
                            || blob.mime.contains("json")
                            || std::str::from_utf8(&blob.bytes).is_ok()
                        {
                            let text = String::from_utf8_lossy(&blob.bytes);
                            let mut head: String = text.chars().take(4_000).collect();
                            if text.len() > head.len() {
                                head.push_str("\n…");
                            }
                            preview = Some(head);
                        } else {
                            preview = Some("(binary — use Save to download)".into());
                        }
                    }
                    None => {
                        rows.push((
                            "Full output".into(),
                            if asleep {
                                "needs the box awake; the relay mirror is tried first".into()
                            } else {
                                "fetching from GET /blob/…".into()
                            },
                        ));
                    }
                }
                sections.push(("Blob".into(), rows));
                "Details".to_string()
            }
        };

        let mut drawer = div()
            .w(px(DRAWER_WIDTH))
            .flex_shrink_0()
            .h_full()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(theme.border)
            .bg(theme.card())
            .child(
                div()
                    .h(px(TAB_BAR_HEIGHT))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(12.0))
                    .border_b_1()
                    .border_color(theme.border)
                    .text_size(px(12.5))
                    .text_color(theme.text)
                    .child(title)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .when_some(blob_key.clone(), |el, key| {
                                let key2 = key.clone();
                                el.child(
                                    div()
                                        .id("details-save")
                                        .text_size(px(12.0))
                                        .text_color(theme.accent)
                                        .cursor_pointer()
                                        .child("Save")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.state.update(cx, |s, cx| {
                                                s.fetch_blob(key2.clone(), true, cx)
                                            });
                                        })),
                                )
                            })
                            .child(
                                div()
                                    .id("details-close")
                                    .text_color(theme.text_faint)
                                    .cursor_pointer()
                                    .child("×")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.state.update(cx, |s, cx| s.close_details(cx));
                                    })),
                            ),
                    ),
            );
        let mut body = div()
            .id("details-body")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .p(px(12.0));
        for (heading, rows) in sections {
            let mut sec = div().flex().flex_col().gap(px(8.0)).child(
                div()
                    .text_size(px(10.5))
                    .text_color(theme.text_faint)
                    .child(heading.to_uppercase()),
            );
            if rows.is_empty() {
                sec = sec.child(
                    div()
                        .text_size(px(12.0))
                        .text_color(theme.text_faint)
                        .child("—"),
                );
            }
            for (k, v) in rows {
                sec = sec.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(theme.text_muted)
                                .child(k),
                        )
                        .child(div().text_size(px(12.5)).text_color(theme.text).child(v)),
                );
            }
            body = body.child(sec);
        }
        if let Some(text) = preview {
            body = body.child(
                div()
                    .p(px(10.0))
                    .rounded(px(8.0))
                    .bg(theme.code_bg)
                    .border_1()
                    .border_color(theme.border)
                    .font_family(theme.font_mono)
                    .text_size(px(11.5))
                    .line_height(px(16.0))
                    .text_color(theme.text)
                    .child(text),
            );
        }
        drawer = drawer.child(body);
        frosted(0.0, PANEL_BLUR, drawer)
    }

    fn render_settings(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let state = self.state.read(cx);
        let source = state.source_name;
        let link = state.link;
        let device = state.device_id.clone();
        let show_details = state.show_details;
        let account = state.account.clone();
        let notify_on = state.notifier.enabled;
        let skill_count = state.skills.len();
        let update = state.update.clone();
        let update_action: Option<(&'static str, &'static str, bool)> = match &update.phase {
            crate::update::UpdatePhase::Available(_)
            | crate::update::UpdatePhase::Failed {
                available: Some(_), ..
            } => Some(("update-install", "Install and relaunch", true)),
            crate::update::UpdatePhase::Idle
            | crate::update::UpdatePhase::UpToDate
            | crate::update::UpdatePhase::Failed {
                available: None, ..
            } => Some(("update-check", "Check now", false)),
            crate::update::UpdatePhase::Checking
            | crate::update::UpdatePhase::Downloading { .. }
            | crate::update::UpdatePhase::Installing(_)
            | crate::update::UpdatePhase::Restarting(_) => None,
        };
        let machine = state.machine.clone();
        let glass_ok = Theme::glass_available();

        let option = |id: &'static str,
                      label: &'static str,
                      selected: bool,
                      enabled: bool,
                      theme: &Theme| {
            div()
                .id(id)
                .px(px(10.0))
                .py(px(5.0))
                .rounded(px(6.0))
                .text_size(px(12.5))
                .bg(if selected {
                    theme.surface_2
                } else {
                    gpui::transparent_black()
                })
                .text_color(if enabled {
                    theme.text
                } else {
                    theme.text_faint
                })
                .when(enabled, |el| {
                    el.cursor_pointer().hover(|s| s.bg(theme.surface_2))
                })
                .child(label)
        };

        let card = div()
            .id("settings-popover")
            .w(px(272.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .p(px(10.0))
            .rounded(px(PANEL_RADIUS))
            .bg(theme.card())
            .border_1()
            .border_color(theme.border_strong)
            .shadow_lg()
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme.text_faint)
                    .child("APPEARANCE · SURFACE"),
            )
            .child(
                div()
                    .flex()
                    .gap(px(4.0))
                    .child(
                        option(
                            "surface-matte",
                            "Matte",
                            theme.surface == Surface::Matte,
                            true,
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            let a = Theme::of(cx).appearance;
                            this.set_theme(a, Surface::Matte, cx);
                        })),
                    )
                    .child(
                        option(
                            "surface-glass",
                            "Glass",
                            theme.surface == Surface::Glass,
                            true,
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            let a = Theme::of(cx).appearance;
                            this.set_theme(a, Surface::Glass, cx);
                        })),
                    ),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme.text_faint)
                    .child(if glass_ok {
                        "Glass: window vibrancy + frosted panels (0.80 shell alpha)."
                    } else {
                        "Glass here: frosted panels only; window vibrancy needs macOS."
                    }),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme.text_faint)
                    .child("APPEARANCE · MODE"),
            )
            .child(
                div()
                    .flex()
                    .gap(px(4.0))
                    .child(
                        option(
                            "mode-dark",
                            "Dark",
                            theme.appearance == Appearance::Dark,
                            true,
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            let s = Theme::of(cx).surface;
                            this.set_theme(Appearance::Dark, s, cx);
                        })),
                    )
                    .child(
                        option(
                            "mode-light",
                            "Light",
                            theme.appearance == Appearance::Light,
                            true,
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            let s = Theme::of(cx).surface;
                            this.set_theme(Appearance::Light, s, cx);
                        })),
                    ),
            )
            .child(div().h(px(1.0)).bg(theme.border))
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme.text_faint)
                    .child("MACHINE"),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .px(px(2.0))
                    .text_size(px(12.0))
                    .text_color(theme.text)
                    .child(format!(
                        "State: {}{}",
                        machine.shown_state().label(),
                        machine
                            .detail
                            .as_deref()
                            .map(|d| format!(" · {d}"))
                            .unwrap_or_default()
                    ))
                    .child(div().text_color(theme.text_muted).child(format!(
                            "{}{}",
                            machine.spec.clone().unwrap_or_else(|| "spec unknown".into()),
                            machine
                                .class
                                .as_deref()
                                .map(|c| format!(" · {c}"))
                                .unwrap_or_default()
                        )))
                    .child(
                        div()
                            .text_color(theme.text_muted)
                            .child(machine_spend_line(&machine)),
                    ),
            )
            .when(machine.always_on, |el| {
                el.child(div().flex().gap(px(4.0)).child(
                    option("machine-restart", "Restart machine", false, true, &theme).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.state.update(cx, |s, cx| s.machine_restart(cx));
                        }),
                    ),
                ))
            })
            .when(!machine.always_on, |el| {
                el.child(
                    div()
                        .flex()
                        .gap(px(4.0))
                        .child(
                            option(
                                "machine-start",
                                "Start",
                                false,
                                machine.state.is_down(),
                                &theme,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.state.read(cx).machine.state.is_down() {
                                    this.state.update(cx, |s, cx| s.machine_start(cx));
                                }
                            })),
                        )
                        .child(
                            option(
                                "machine-stop",
                                "Stop",
                                false,
                                !machine.state.is_down(),
                                &theme,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                if !this.state.read(cx).machine.state.is_down() {
                                    this.state.update(cx, |s, cx| s.machine_stop(cx));
                                }
                            })),
                        ),
                )
            })
            .child(div().h(px(1.0)).bg(theme.border))
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme.text_faint)
                    .child("DEBUG"),
            )
            .child(
                div()
                    .flex()
                    .gap(px(4.0))
                    .child(
                        option("details-off", "Details off", !show_details, true, &theme).on_click(
                            cx.listener(|this, _, _, cx| {
                                if this.state.read(cx).show_details {
                                    this.state.update(cx, |s, cx| s.toggle_show_details(cx));
                                }
                            }),
                        ),
                    )
                    .child(
                        option("details-on", "Show details", show_details, true, &theme).on_click(
                            cx.listener(|this, _, _, cx| {
                                if !this.state.read(cx).show_details {
                                    this.state.update(cx, |s, cx| s.toggle_show_details(cx));
                                }
                            }),
                        ),
                    ),
            )
            .child(div().h(px(1.0)).bg(theme.border))
            .child(
                div()
                    .id("settings-connectors")
                    .flex()
                    .items_center()
                    .px(px(10.0))
                    .py(px(6.0))
                    .rounded(px(6.0))
                    .text_size(px(12.5))
                    .text_color(theme.text)
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.surface_2))
                    .child(div().flex_1().child("Connectors"))
                    .child(div().text_color(theme.text_faint).child("›"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.settings_open = false;
                        this.connectors.update(cx, |p, cx| p.show(cx));
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .id("settings-routines")
                    .flex()
                    .items_center()
                    .px(px(10.0))
                    .py(px(6.0))
                    .rounded(px(6.0))
                    .text_size(px(12.5))
                    .text_color(theme.text)
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.surface_2))
                    .child(div().flex_1().child("Routines"))
                    .child(div().text_color(theme.text_faint).child("›"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.settings_open = false;
                        this.routines_open = true;
                        this.routines.update(cx, |p, cx| p.open(cx));
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .id("settings-channels")
                    .flex()
                    .items_center()
                    .px(px(10.0))
                    .py(px(6.0))
                    .rounded(px(6.0))
                    .text_size(px(12.5))
                    .text_color(theme.text)
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.surface_2))
                    .child(div().flex_1().child("Channels"))
                    .child(div().text_color(theme.text_faint).child("›"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.settings_open = false;
                        this.channels.update(cx, |p, cx| p.show(cx));
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .id("settings-skills")
                    .flex()
                    .items_center()
                    .px(px(10.0))
                    .py(px(6.0))
                    .rounded(px(6.0))
                    .text_size(px(12.5))
                    .text_color(theme.text)
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.surface_2))
                    .child(div().flex_1().child("Skills"))
                    .child(
                        div()
                            .text_color(theme.text_faint)
                            .child(format!("{skill_count} in library ›")),
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.settings_open = false;
                        this.skills.update(cx, |p, cx| p.open(window, cx));
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .id("settings-local")
                    .flex()
                    .items_center()
                    .px(px(10.0))
                    .py(px(6.0))
                    .rounded(px(6.0))
                    .text_size(px(12.5))
                    .text_color(theme.text)
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.surface_2))
                    .child(div().flex_1().child("This Mac"))
                    .child(div().text_color(theme.text_faint).child("›"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.settings_open = false;
                        this.local.update(cx, |p, cx| p.show(cx));
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .id("settings-vault")
                    .flex()
                    .items_center()
                    .px(px(10.0))
                    .py(px(6.0))
                    .rounded(px(6.0))
                    .text_size(px(12.5))
                    .text_color(theme.text)
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.surface_2))
                    .child(div().flex_1().child("Vault"))
                    .child(div().text_color(theme.text_faint).child("›"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.settings_open = false;
                        this.vault.update(cx, |p, cx| p.show(cx));
                        cx.notify();
                    })),
            )
            .child(div().h(px(1.0)).bg(theme.border))
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme.text_faint)
                    .child("NOTIFICATIONS"),
            )
            .child(
                div()
                    .flex()
                    .gap(px(4.0))
                    .child(option("notify-on", "On", notify_on, true, &theme).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.state
                                .update(cx, |s, cx| s.set_notifications_enabled(true, cx));
                        }),
                    ))
                    .child(
                        option("notify-off", "Off", !notify_on, true, &theme).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.state
                                    .update(cx, |s, cx| s.set_notifications_enabled(false, cx));
                            }),
                        ),
                    ),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme.text_faint)
                    .child("One per project per minute, only while you are not looking."),
            )
            .child(div().h(px(1.0)).bg(theme.border))
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme.text_faint)
                    .child("UPDATES"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(11.5))
                            .text_color(
                                if matches!(update.phase, crate::update::UpdatePhase::Failed { .. })
                                {
                                    theme.danger
                                } else {
                                    theme.text_muted
                                },
                            )
                            .child(update.status_line()),
                    )
                    .when_some(update_action, |el, (id, label, installs)| {
                        el.child(
                            div()
                                .id(id)
                                .flex_shrink_0()
                                .px(px(8.0))
                                .py(px(3.0))
                                .rounded(px(6.0))
                                .text_size(px(11.5))
                                .text_color(theme.accent)
                                .cursor_pointer()
                                .hover(|s| s.bg(theme.surface_2))
                                .child(label)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.state.update(cx, |s, cx| {
                                        if installs {
                                            s.install_update(cx);
                                        } else {
                                            s.check_for_updates(cx);
                                        }
                                    });
                                })),
                        )
                    }),
            )
            .child(div().h(px(1.0)).bg(theme.border))
            .when_some(account.clone(), |el, account| {
                let who = if account.login.is_empty() {
                    "Signed in".to_string()
                } else {
                    format!("@{}", account.login)
                };
                let credits = if account.balance_usd > 0.0 || account.daily_cap_usd.is_some() {
                    match account.daily_cap_usd {
                        Some(cap) => format!(
                            " · ${:.2} left · ${:.2} of ${cap:.0} today",
                            account.balance_usd, account.spent_today_usd
                        ),
                        None => format!(" · ${:.2} left", account.balance_usd),
                    }
                } else {
                    String::new()
                };
                el.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.text_faint)
                        .child("ACCOUNT"),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .text_size(px(12.0))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_color(theme.text)
                                .child(format!("{who}{credits}")),
                        )
                        .child(
                            div()
                                .id("sign-out")
                                .text_color(theme.accent)
                                .cursor_pointer()
                                .child("Sign out")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.settings_open = false;
                                    this.state.update(cx, |s, cx| s.sign_out(cx));
                                })),
                        ),
                )
                .child(div().h(px(1.0)).bg(theme.border))
            })
            .child(
                div()
                    .text_size(px(11.5))
                    .text_color(theme.text_muted)
                    .child(format!(
                        "Source: {source} · link: {link:?}{}",
                        device.map(|d| format!(" · device {d}")).unwrap_or_default()
                    )),
            );
        // Pinned to the bottom, never taller than the window: the card
        // scrolls once the sections outgrow the height.
        div()
            .absolute()
            .left(px(RAIL_WIDTH + 8.0))
            .top(px(TAB_BAR_HEIGHT + 8.0))
            .bottom(px(14.0))
            .flex()
            .flex_col()
            .justify_end()
            .child(frosted(
                PANEL_RADIUS,
                MENU_BLUR,
                card.min_h_0().flex_shrink(1.0).overflow_y_scroll(),
            ))
    }

    /// First run against a relay: enter the one-time code the relay printed
    /// (`arbos-relay` logs it; `POST /pair/new` mints more).
    /// First launch: the mark, one sentence, "Sign in with GitHub". The
    /// pairing-code entry only appears when the relay runs in paircode mode.
    fn render_pairing(&self, pairing: &crate::app::Pairing, cx: &mut Context<Self>) -> Div {
        let theme = Theme::of(cx).clone();
        let mark_image = self.state.read(cx).mark.clone();
        let mark = div()
            .size(px(72.0))
            .flex()
            .items_center()
            .justify_center()
            .map(|el| match mark_image {
                Some(image) => el.child(
                    gpui::img(image)
                        .size(px(72.0))
                        .object_fit(gpui::ObjectFit::Contain),
                ),
                None => el
                    .rounded(px(18.0))
                    .bg(theme.text)
                    .text_size(px(34.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.bg)
                    .child("Y"),
            });
        let screen = div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(14.0))
            .px(px(28.0))
            .child(mark)
            .child(
                div()
                    .pt(px(6.0))
                    .text_size(px(22.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child("Arbos"),
            )
            .child(
                div()
                    .text_size(px(13.0))
                    .text_color(theme.text_muted)
                    .text_center()
                    .child(
                        "Your agents work from one machine in the cloud; sign in and start a chat.",
                    ),
            );
        match pairing.mode {
            SignInMode::GitHub => {
                let busy = pairing.busy;
                let button = div()
                    .id("sign-in-github")
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .mt(px(10.0))
                    .px(px(18.0))
                    .h(px(40.0))
                    .rounded_full()
                    .bg(theme.text)
                    .text_color(theme.bg)
                    .text_size(px(13.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .cursor_pointer()
                    .when(busy, |el| el.opacity(0.7))
                    .child(if busy {
                        "Waiting for GitHub…"
                    } else {
                        "Sign in with GitHub"
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.update(cx, |s, cx| s.sign_in(cx));
                    }));
                screen.child(frosted(20.0, PANEL_BLUR, button)).when_some(
                    pairing.error.clone(),
                    |el, err| {
                        el.child(
                            div()
                                .max_w(px(420.0))
                                .text_center()
                                .text_size(px(12.0))
                                .text_color(theme.danger)
                                .child(err),
                        )
                    },
                )
            }
            SignInMode::PairCode => {
                let input_row = div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .h(px(COMPOSER_HEIGHT))
                    .px(px(16.0))
                    .rounded(px(COMPOSER_HEIGHT / 2.0))
                    .bg(theme.card())
                    .border_1()
                    .border_color(if pairing.error.is_some() {
                        theme.danger
                    } else {
                        theme.border_strong
                    })
                    .child(self.pairing_input.clone())
                    .child(
                        div()
                            .id("pair-submit")
                            .px(px(12.0))
                            .py(px(6.0))
                            .rounded_full()
                            .bg(theme.text)
                            .text_color(theme.bg)
                            .text_size(px(12.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .cursor_pointer()
                            .child(if pairing.busy { "Pairing…" } else { "Pair" })
                            .on_click(cx.listener(|this, _, _, cx| this.submit_pairing(cx))),
                    );
                screen
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(theme.text_faint)
                            .text_center()
                            .child(format!(
                                "{} uses pairing codes. Enter the one-time code from its log (or `arbosctl pair-code`).",
                                pairing.relay_url
                            )),
                    )
                    .child(
                        div()
                            .w(px(COMPOSER_WIDTH))
                            .max_w_full()
                            .pt(px(4.0))
                            .child(frosted(COMPOSER_HEIGHT / 2.0, PANEL_BLUR, input_row)),
                    )
                    .when_some(pairing.error.clone(), |el, err| {
                        el.child(div().text_size(px(12.0)).text_color(theme.danger).child(err))
                    })
            }
        }
    }

    fn render_new_project(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let card = div()
            .w(px(300.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .p(px(12.0))
            .rounded(px(PANEL_RADIUS))
            .bg(theme.card())
            .border_1()
            .border_color(theme.border_strong)
            .shadow_lg()
            .child(
                div()
                    .text_size(px(12.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child("New project"),
            )
            .child(
                div()
                    .text_size(px(11.5))
                    .text_color(theme.text_muted)
                    .child("It becomes a workspace on your machine. Enter to create."),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .h(px(36.0))
                    .px(px(12.0))
                    .rounded(px(8.0))
                    .bg(theme.surface_2)
                    .border_1()
                    .border_color(theme.border)
                    .child(self.project_input.clone()),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(6.0))
                    .child(
                        div()
                            .id("new-project-cancel")
                            .px(px(10.0))
                            .py(px(5.0))
                            .rounded(px(6.0))
                            .text_size(px(12.0))
                            .text_color(theme.text_muted)
                            .cursor_pointer()
                            .hover(|s| s.bg(theme.surface_2))
                            .child("Cancel")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.state.update(cx, |s, cx| {
                                    s.new_project_open = false;
                                    cx.notify();
                                });
                                Composer::focus(&this.composer, window, cx);
                            })),
                    )
                    .child(
                        div()
                            .id("new-project-create")
                            .px(px(10.0))
                            .py(px(5.0))
                            .rounded(px(6.0))
                            .text_size(px(12.0))
                            .bg(theme.text)
                            .text_color(theme.bg)
                            .cursor_pointer()
                            .child("Create")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.submit_project(window, cx)),
                            ),
                    ),
            );
        div()
            .absolute()
            .left(px(RAIL_WIDTH + 8.0))
            .bottom(px(48.0))
            .child(frosted(PANEL_RADIUS, MENU_BLUR, card))
    }

    fn render_notices(&self, cx: &mut Context<Self>) -> Option<Div> {
        let theme = Theme::of(cx).clone();
        let notices = self.state.read(cx).live_notices();
        if notices.is_empty() {
            return None;
        }
        PulseClock::request(cx);
        let mut stack = div()
            .absolute()
            .right(px(16.0))
            .bottom(px(16.0))
            .flex()
            .flex_col()
            .gap(px(6.0));
        for n in notices {
            stack = stack.child(
                div()
                    .max_w(px(360.0))
                    .px(px(12.0))
                    .py(px(8.0))
                    .rounded(px(8.0))
                    .bg(theme.surface_2)
                    .border_1()
                    .border_color(theme.border_strong)
                    .shadow_md()
                    .text_size(px(12.0))
                    .text_color(theme.text)
                    .child(n.text),
            );
        }
        Some(stack)
    }
}

/// Flatten a JSON object one level ("cost.total") for the drawer.
fn json_rows(v: &serde_json::Value) -> Vec<(String, String)> {
    fn scalar(v: &serde_json::Value) -> String {
        match v {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Null => "—".into(),
            serde_json::Value::Number(n) => match n.as_f64() {
                Some(f) if f.fract() != 0.0 => format!("{f:.6}").trim_end_matches('0').to_string(),
                _ => n.to_string(),
            },
            other => other.to_string(),
        }
    }
    match v {
        serde_json::Value::Object(map) => {
            let mut rows = Vec::new();
            for (k, v) in map {
                match v {
                    serde_json::Value::Object(inner) => {
                        for (ik, iv) in inner {
                            rows.push((format!("{k}.{ik}"), scalar(iv)));
                        }
                    }
                    other => rows.push((k.clone(), scalar(other))),
                }
            }
            rows
        }
        serde_json::Value::Null => Vec::new(),
        other => vec![("value".into(), scalar(other))],
    }
}

/// Hosted machines are included; self-hosted ones count against the cap.
fn machine_spend_line(machine: &crate::model::Machine) -> String {
    if machine.always_on {
        format!("Machine today ${:.2} (included)", machine.spend_today_usd)
    } else {
        format!(
            "Spend today ${:.2} of ${:.2}",
            machine.spend_today_usd, machine.spend_cap_usd
        )
    }
}

fn fmt_uptime(ms: u64) -> String {
    let secs = ms / 1000;
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60)
    }
}

fn cards_spinner(theme: &Theme, cx: &App) -> Div {
    crate::ui::cards::spinner(theme, cx)
}

impl Focusable for Shell {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let width: f32 = window.viewport_size().width.into();
        let state = self.state.read(cx);
        let collapsed = state.rail_collapsed || state.window_is_narrow(width);
        let panel_open = state.right_panel_open;
        let details = state.details_open.clone();
        let pairing = state.pairing.clone();
        let new_project_open = state.new_project_open;
        let new_chat_open = state.new_chat_open;
        let people_space = state.selected().filter(|p| p.is_people()).cloned();
        let project_space = state.selected().filter(|p| !p.is_people()).cloned();
        let avatar_picker = state.avatar_picker.clone();

        let mut root = div()
            .id("shell-root")
            .key_context("Shell")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::close_tab))
            .on_action(cx.listener(Self::new_tab))
            .on_action(cx.listener(Self::toggle_rail))
            .on_action(cx.listener(Self::toggle_panel))
            // Clicks on chrome must not strand keyboard focus: unless another
            // text field took it, the composer keeps it.
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    let pairing = this.pairing_input.read(cx).focus_handle(cx);
                    let project = this.project_input.read(cx).focus_handle(cx);
                    let shown = this.state.read(cx).new_project_open;
                    let people = this.people_input.read(cx).focus_handle(cx);
                    if pairing.is_focused(window)
                        || (shown && project.is_focused(window))
                        || (this.state.read(cx).new_chat_open && people.is_focused(window))
                        || this.connectors.read(cx).input_focused(window, cx)
                        || this.skills.read(cx).has_focus(window, cx)
                        || this.vault.read(cx).input_focused(window, cx)
                    {
                        return;
                    }
                    if this.routines_open && this.routines.read(cx).has_focus(window, cx) {
                        return;
                    }
                    if this.state.read(cx).pairing.is_none() {
                        Composer::focus(&this.composer, window, cx);
                    }
                }),
            )
            .relative()
            .flex()
            .flex_row()
            .size_full()
            .bg(theme.shell())
            .text_color(theme.text)
            .font_family(theme.font_sans)
            .text_size(px(13.0));

        if let Some(pairing) = pairing {
            self.signing_in = true;
            return root
                .child(self.render_pairing(&pairing, cx))
                .into_any_element();
        }
        if self.signing_in {
            // Landed after sign-in: the composer takes the keyboard.
            self.signing_in = false;
            Composer::focus(&self.composer, window, cx);
        }

        root = root
            .child(frosted(0.0, PANEL_BLUR, self.render_rail(collapsed, cx)))
            .child(self.render_center(cx));
        self.reveal_tab = None;
        if panel_open {
            root = root.child(self.render_right_panel(cx));
        }
        if let Some(target) = details {
            root = root.child(self.render_details(&target, cx));
        }
        if self.settings_open {
            root = root.child(self.render_settings(cx));
        }
        if self.group_picker_open {
            let right = if panel_open {
                RIGHT_PANEL_WIDTH + 44.0
            } else {
                44.0
            };
            root = root.child(groups::picker(&self.state, TAB_BAR_HEIGHT + 4.0, right, cx));
        }
        if self.connectors.read(cx).is_open() {
            root = root.child(self.connectors.clone());
        }
        if self.local.read(cx).is_open() {
            root = root.child(self.local.clone());
        }
        if self.vault.read(cx).is_open() {
            root = root.child(self.vault.clone());
        }
        if self.routines_open {
            root = root.child(self.routines.clone());
        }
        if self.channels.read(cx).is_open() {
            root = root.child(self.channels.clone());
        }
        if self.skills.read(cx).open {
            root = root.child(self.skills.clone());
        }
        if new_project_open {
            root = root.child(self.render_new_project(cx));
        }
        if self.new_menu_open {
            root = root.child(people::new_menu(cx));
        }
        if let Some(space) = people_space {
            let right = if panel_open {
                RIGHT_PANEL_WIDTH + 16.0
            } else {
                16.0
            };
            if self.add_agent_open {
                root = root.child(people::add_agent_picker(&space, &self.state, right, cx));
            }
            if self.members_open {
                root = root.child(people::members_popover(&space, &self.state, right, cx));
            }
        } else if let Some(project) = project_space {
            let right = if panel_open {
                RIGHT_PANEL_WIDTH + 90.0
            } else {
                90.0
            };
            if avatar_picker.as_deref() == Some(project.id.as_str()) {
                root = root.child(avatars::picker(&project, &self.state, right, cx));
            }
            if self.members_open {
                root = root.child(people::project_members_popover(
                    &project,
                    &self.state,
                    right,
                    cx,
                ));
            }
        }
        if new_chat_open {
            root = root.child(people::new_chat_sheet(&self.state, &self.people_input, cx));
        }
        if let Some(notices) = self.render_notices(cx) {
            root = root.child(notices);
        }
        root.into_any_element()
    }
}

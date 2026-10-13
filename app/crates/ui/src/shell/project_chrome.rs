//! Projects rail, the tab strip, the Context browser, and the status checklist.
//! The rail replaces the session list. Each icon is one folder.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{
    div, prelude::*, px, Animation, AnimationExt, AnyElement, Context, Entity, Focusable,
    InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Task, Window,
};

use crate::composer::ComposerInput;
use crate::image_media::{decode_project_icon, MediaImage};
use crate::theme::Theme;
use zeron_engine::arbos_layout::status_is_live;

use super::listening_pill::{listening_rows, ListeningRow};
use super::Shell;

pub(super) const RAIL_WIDTH: f32 = 88.0;
/// The project file list beside Main. A new chat hides it, so that chat
/// stays centered.
pub(super) const PROJECT_FILES_WIDTH: f32 = 252.0;
/// Tall enough to read a file, short enough that the file list stays on screen.
const SIDE_FILE_EDITOR_HEIGHT: f32 = 300.0;
#[allow(dead_code)]
pub(super) const TAB_HEIGHT: f32 = 36.0;
const EXTRA_VISIBLE: usize = 2;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Surface {
    Chat,
    Context,
}

pub(super) struct ProjectChrome {
    pub surface: Surface,
    pub icon_for: Option<String>,
    pub context_relative: Option<String>,
    editor: Option<Entity<ComposerInput>>,
    emoji_input: Option<Entity<ComposerInput>>,
    image_input: Option<Entity<ComposerInput>>,
    pub status: Vec<(bool, String)>,
    /// Objective from `.arbos/context/goal.toml`. Empty when the project has no record.
    pub goal_objective: Option<String>,
    /// `active`, `paused`, or `complete`.
    pub goal_state: Option<String>,
    /// Seconds since the goal started. A complete goal stops this clock.
    pub goal_elapsed: u64,
    pub skill: Option<String>,
    /// Main finished a turn that showed text, and the person has not looked.
    pub unread: bool,
    /// The live step while Main is working. Empty when Main is idle.
    pub step: String,
    /// Each worker's live line, by chat id, while its model is being
    /// retried or has paused. A worker whose model answers is not listed.
    pub worker_steps: HashMap<String, String>,
    /// What Main waits on now: the rows of the Listening pill.
    pub listening: Vec<ListeningRow>,
    /// The pill was clicked open; it stays open until clicked again.
    pub listening_open: bool,
    /// The pointer is over the pill, so its rows show.
    pub listening_hover: bool,
    pub listening_task: Option<Task<()>>,
    /// The file opened from the list beside Main. This does not leave the chat.
    side_file: Option<String>,
    side_editor: Option<Entity<ComposerInput>>,
    watched: Option<String>,
    watch: Option<Task<()>>,
    icons: HashMap<String, MediaImage>,
    icon_loading: HashSet<String>,
}

impl Default for ProjectChrome {
    fn default() -> Self {
        Self {
            surface: Surface::Chat,
            icon_for: None,
            context_relative: None,
            editor: None,
            emoji_input: None,
            image_input: None,
            status: Vec::new(),
            goal_objective: None,
            goal_state: None,
            goal_elapsed: 0,
            skill: None,
            unread: false,
            step: String::new(),
            worker_steps: HashMap::new(),
            listening: Vec::new(),
            listening_open: false,
            listening_hover: false,
            listening_task: None,
            side_file: None,
            side_editor: None,
            watched: None,
            watch: None,
            icons: HashMap::new(),
            icon_loading: HashSet::new(),
        }
    }
}

pub(super) struct ProjectRow {
    id: String,
    pub(super) path: String,
    name: String,
    home: String,
}

struct ChatTab {
    id: String,
    title: String,
    main: bool,
    worker: bool,
    /// `working`, `waiting`, `done`, `error`, `stopped`, or empty.
    status: String,
}

/// The tab strip's status word for a chat: the engine's `waiting` wins over
/// a running indicator, since the session is still up while the model is
/// being retried; else a running chat or a stored `working` reads `working`.
fn tab_status(indicator_working: bool, stored: &str) -> String {
    if stored == "waiting" {
        "waiting".into()
    } else if indicator_working || stored == "working" {
        "working".into()
    } else {
        stored.to_owned()
    }
}

/// Each live worker's step, keyed by chat id, for the title of its tab.
fn worker_steps(agents: &[zeron_engine::arbos_layout::AgentView]) -> HashMap<String, String> {
    agents
        .iter()
        .filter(|agent| agent.role == "worker" && status_is_live(&agent.status))
        .filter(|agent| !agent.step.trim().is_empty())
        .map(|agent| (agent.chat_id.clone(), agent.step.trim().to_owned()))
        .collect()
}

impl Shell {
    pub(super) fn take_project_debug(&mut self, cx: &mut Context<Self>) {
        match self.debug_dialog.as_deref() {
            Some("context") => {
                if self.state.read(cx).spaces.is_empty() {
                    return;
                }
                self.debug_dialog = None;
                self.show_project_context(cx);
            }
            Some("icon") => {
                let id = self
                    .state
                    .read(cx)
                    .spaces
                    .first()
                    .map(|space| space.id.clone());
                if let Some(id) = id {
                    self.debug_dialog = None;
                    self.open_icon_menu(id, cx);
                }
            }
            _ => {}
        }
    }

    pub(super) fn render_project_rail(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let rows = self.project_rows(cx);
        let selected = self.state.read(cx).selected_space.clone();
        let mut column = div()
            .w(px(RAIL_WIDTH))
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .pt(px(8.0))
            .gap(px(6.0));
        for (index, row) in rows.iter().enumerate() {
            column = column.child(self.rail_button(index, row, selected.as_deref(), theme, cx));
        }
        column
            .child(div().flex_1())
            .child(self.rail_plus(theme, cx))
            .child(div().h(px(8.0)))
            .into_any_element()
    }

    pub(super) fn render_project_tabs(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let space = self.selected_project(cx);
        if let Some(project) = &space {
            self.ensure_status_watch(project.path.clone(), cx);
        }
        let tabs = space
            .as_ref()
            .map(|project| self.chat_tabs(&project.id, cx))
            .unwrap_or_default();
        let selected = self.state.read(cx).selected_chat.clone();
        let surface = self.project.surface;
        let (main, extras): (Option<ChatTab>, Vec<ChatTab>) =
            tabs.into_iter().partition(|tab| tab.main).let_one();
        let shown_ix = visible_extra_indexes(&extras, selected.as_deref());
        let hidden = extras.len().saturating_sub(shown_ix.len());
        let mut strip = div()
            .min_w_0()
            .flex_1()
            .h_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .pl(px(12.0))
            .overflow_hidden()
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation());
        if let Some(tab) = main {
            strip = strip.child(self.chat_tab(&tab, selected.as_deref(), surface, &theme, cx));
            strip = strip.child(div().w(px(1.0)).h(px(16.0)).bg(theme.border).mx(px(4.0)));
        }
        for index in &shown_ix {
            let tab = &extras[*index];
            strip = strip.child(self.chat_tab(tab, selected.as_deref(), surface, &theme, cx));
        }
        if hidden > 0 {
            let next = extras
                .iter()
                .enumerate()
                .find(|(index, _)| !shown_ix.contains(index))
                .map(|(_, tab)| tab.id.clone());
            strip = strip.child(self.overflow_tab(hidden, next, &theme, cx));
        }
        strip = strip.child(self.new_chat_tab(&theme, cx));
        strip.child(div().flex_1()).into_any_element()
    }

    pub(super) fn render_icon_menu(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let space_id = self.project.icon_for.clone()?;
        let project = self
            .project_rows(cx)
            .into_iter()
            .find(|row| row.id == space_id)?;
        let theme = Theme::of(cx).clone();
        let emoji = self.project.emoji_input.clone()?;
        let image = self.project.image_input.clone()?;
        let path = project.path.clone();
        let path_emoji = path.clone();
        let path_image = path.clone();
        let path_repo = path.clone();
        Some(
            div()
                .absolute()
                .left(px(RAIL_WIDTH + 8.0))
                .top(px(Theme::TITLEBAR_HEIGHT + 8.0))
                .w(px(240.0))
                .p(px(10.0))
                .flex()
                .flex_col()
                .gap(px(8.0))
                .rounded(px(8.0))
                .border_1()
                .border_color(theme.border)
                .bg(theme.surface_raised)
                .child(menu_label("Icon", &theme))
                .child(emoji)
                .child(
                    div()
                        .id("icon-menu-emoji")
                        .px(px(8.0))
                        .py(px(4.0))
                        .rounded(px(6.0))
                        .cursor_pointer()
                        .text_size(px(12.0))
                        .text_color(theme.text)
                        .bg(theme.element_hover)
                        .child("Use emoji")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let glyph = this
                                .project
                                .emoji_input
                                .as_ref()
                                .map(|input| input.read(cx).text().trim().to_owned())
                                .unwrap_or_default();
                            if !glyph.is_empty() {
                                this.set_project_icon(
                                    path_emoji.clone(),
                                    format!("emoji:{glyph}"),
                                    cx,
                                );
                            }
                        })),
                )
                .child(image)
                .child(
                    div()
                        .id("icon-menu-image")
                        .px(px(8.0))
                        .py(px(4.0))
                        .rounded(px(6.0))
                        .cursor_pointer()
                        .text_size(px(12.0))
                        .text_color(theme.text)
                        .bg(theme.element_hover)
                        .child("Use image file")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let file = this
                                .project
                                .image_input
                                .as_ref()
                                .map(|input| input.read(cx).text().trim().to_owned())
                                .unwrap_or_default();
                            if !file.is_empty() {
                                this.set_project_icon(
                                    path_image.clone(),
                                    format!("image:{file}"),
                                    cx,
                                );
                            }
                        })),
                )
                .child(
                    div()
                        .id("icon-menu-repo")
                        .px(px(8.0))
                        .py(px(4.0))
                        .rounded(px(6.0))
                        .cursor_pointer()
                        .text_size(px(12.0))
                        .text_color(theme.text)
                        .bg(theme.element_hover)
                        .child("Use repo icon")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_project_icon(path_repo.clone(), "repo".into(), cx);
                        })),
                )
                .into_any_element(),
        )
    }

    pub(super) fn render_project_checklist(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let show = self.project.surface == Surface::Chat && self.selected_is_main(cx);
        if !show || self.project.status.is_empty() {
            return div().into_any_element();
        }
        let theme = Theme::of(cx).clone();
        let mut list = div()
            .w_full()
            .max_w(px(720.0))
            .mx_auto()
            .mb(px(8.0))
            .px(px(12.0))
            .py(px(8.0))
            .rounded(px(10.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface)
            .flex()
            .flex_col()
            .gap(px(4.0));
        for (done, text) in self.project.status.clone() {
            let mark = if done { "☑" } else { "☐" };
            list = list.child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(8.0))
                    .items_center()
                    .text_size(px(13.0))
                    .text_color(if done { theme.text_muted } else { theme.text })
                    .child(mark)
                    .child(text),
            );
        }
        list.into_any_element()
    }

    /// The durable goal from `goal.toml`: objective, a running clock, state,
    /// Pause, Complete, and Clear. Stop on the composer ends the turn and leaves this
    /// record in place. A finished checklist step does not remove the row.
    pub(super) fn render_active_goal(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let show = self.project.surface == Surface::Chat && self.selected_is_main(cx);
        let Some(objective) = self.project.goal_objective.clone() else {
            return div().into_any_element();
        };
        if !show {
            return div().into_any_element();
        }
        let state = self
            .project
            .goal_state
            .clone()
            .unwrap_or_else(|| "active".to_owned());
        let theme = Theme::of(cx).clone();
        let mut card = div()
            .w_full()
            .max_w(px(720.0))
            .mx_auto()
            .mb(px(8.0))
            .px(px(12.0))
            .py(px(8.0))
            .rounded(px(10.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface)
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .overflow_hidden()
                            .text_size(px(13.0))
                            .text_color(theme.text)
                            .child(objective),
                    )
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.0))
                            .text_size(px(12.0))
                            .text_color(theme.text_muted)
                            .child(state.clone())
                            .child(format_goal_clock(self.project.goal_elapsed)),
                    ),
            );
        let mut actions = div().flex().flex_row().items_center().gap(px(6.0));
        if let Some(label) = goal_pause_label(&state) {
            actions = actions.child(
                div()
                    .id("goal-pause")
                    .px(px(8.0))
                    .py(px(3.0))
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .bg(theme.element_hover)
                    .text_size(px(12.0))
                    .text_color(theme.text)
                    .child(label)
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_goal_pause(cx))),
            );
        }
        if goal_can_complete(&state) {
            actions = actions.child(
                div()
                    .id("goal-complete")
                    .px(px(8.0))
                    .py(px(3.0))
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .bg(theme.element_hover)
                    .text_size(px(12.0))
                    .text_color(theme.text)
                    .child("Complete")
                    .on_click(cx.listener(|this, _, _, cx| this.complete_shown_goal(cx))),
            );
        }
        actions = actions.child(
            div()
                .id("goal-clear")
                .px(px(8.0))
                .py(px(3.0))
                .rounded(px(6.0))
                .cursor_pointer()
                .bg(theme.element_hover)
                .text_size(px(12.0))
                .text_color(theme.text)
                .child("Clear")
                .on_click(cx.listener(|this, _, _, cx| this.clear_shown_goal(cx))),
        );
        card = card.child(actions);
        if let Some(skill) = self.project.skill.clone() {
            card = card.child(
                div()
                    .text_size(px(13.0))
                    .text_color(theme.text)
                    .child(format!("Skill · {skill}")),
            );
        }
        card.into_any_element()
    }

    /// Pause an active goal, or resume a paused one. A complete goal stays complete.
    fn toggle_goal_pause(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.selected_project(cx) else {
            return;
        };
        let path = Path::new(&project.path);
        let Some(state) = self.project.goal_state.clone() else {
            return;
        };
        let result = match state.as_str() {
            "active" => zeron_engine::arbos_goal::pause_goal(path).map(|_| ()),
            "paused" => zeron_engine::arbos_goal::resume_goal(path).map(|_| ()),
            "complete" => Ok(()),
            _ => Ok(()),
        };
        if let Err(err) = result {
            tracing::warn!(error = %err, "goal pause was not changed");
        }
        self.apply_goal_row(path);
        cx.notify();
    }

    /// The person completes the goal. No checklist or reason is asked for;
    /// only Main's own call is held to those.
    fn complete_shown_goal(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.selected_project(cx) else {
            return;
        };
        let path = Path::new(&project.path);
        if let Err(err) = zeron_engine::arbos_goal::force_complete_goal(path) {
            tracing::warn!(error = %err, "goal was not completed");
        }
        self.apply_goal_row(path);
        cx.notify();
    }

    /// Remove the goal record. The checklist and `GOALS.md` stay.
    fn clear_shown_goal(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.selected_project(cx) else {
            return;
        };
        let path = Path::new(&project.path);
        if let Err(err) = zeron_engine::arbos_goal::clear_goal(path) {
            tracing::warn!(error = %err, "goal was not cleared");
        }
        self.apply_goal_row(path);
        cx.notify();
    }

    fn apply_goal_row(&mut self, project: &Path) {
        match zeron_engine::arbos_goal::goal_row(project) {
            Some(row) => {
                self.project.goal_objective = Some(row.objective);
                self.project.goal_state = Some(row.state);
                self.project.goal_elapsed = row.elapsed_seconds;
            }
            None => {
                self.project.goal_objective = None;
                self.project.goal_state = None;
                self.project.goal_elapsed = 0;
            }
        }
    }

    /// The file this worker wrote, read from the project. Main keeps the
    /// checklist. The worker tab shows the file.
    pub(super) fn render_worker_file(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.project.surface != Surface::Chat || self.selected_is_main(cx) {
            return div().into_any_element();
        }
        let Some(project) = self.selected_project(cx) else {
            return div().into_any_element();
        };
        let Some(chat_id) = self.state.read(cx).selected_chat.clone() else {
            return div().into_any_element();
        };
        let Some((path, body)) = worker_context_file(Path::new(&project.path), &chat_id) else {
            return div().into_any_element();
        };
        let theme = Theme::of(cx).clone();
        let mut lines = div().flex().flex_col().gap(px(2.0));
        for line in body.lines().take(12) {
            lines = lines.child(
                div()
                    .text_size(px(13.0))
                    .text_color(theme.text)
                    .child(line.to_owned()),
            );
        }
        div()
            .w_full()
            .max_w(px(720.0))
            .mx_auto()
            .mb(px(8.0))
            .px(px(12.0))
            .py(px(8.0))
            .rounded(px(10.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface)
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme.text_muted)
                    .child(path),
            )
            .child(lines)
            .into_any_element()
    }

    /// Width of the file list beside Main. Zero on any other chat, so a new
    /// chat keeps its centered composer.
    pub(super) fn project_files_width(&self, cx: &gpui::App) -> f32 {
        if self.project.surface != Surface::Chat || !self.selected_is_main(cx) {
            return 0.0;
        }
        let Some(project) = self.selected_project(cx) else {
            return 0.0;
        };
        if context_files(Path::new(&project.path)).is_empty() {
            return 0.0;
        }
        PROJECT_FILES_WIDTH
    }

    /// The project's files, beside the chat. Opening one shows that file and
    /// leaves Main on the goal.
    pub(super) fn render_project_files(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.project_files_width(cx) <= 0.0 {
            return div().into_any_element();
        }
        let Some(project) = self.selected_project(cx) else {
            return div().into_any_element();
        };
        let theme = Theme::of(cx).clone();
        let files = context_files(Path::new(&project.path));
        let current = self.project.side_file.clone();
        let editor = self.project.side_editor.clone();
        let mut list = div()
            .id("project-file-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(2.0));
        for file in files {
            let selected = current.as_deref() == Some(file.as_str());
            let open = file.clone();
            list = list.child(
                div()
                    .id(SharedString::from(format!("side-file-{file}")))
                    .px(px(8.0))
                    .py(px(3.0))
                    .rounded(px(6.0))
                    .text_size(px(12.0))
                    .cursor_pointer()
                    .text_color(if selected {
                        theme.text
                    } else {
                        theme.text_muted
                    })
                    .bg(if selected {
                        theme.element_hover
                    } else {
                        gpui::transparent_black()
                    })
                    .child(file)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_side_file(open.clone(), window, cx);
                    })),
            );
        }
        let mut column = div()
            .id("project-files")
            .w(px(PROJECT_FILES_WIDTH))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .pt(px(Theme::TITLEBAR_HEIGHT + 8.0))
            .px(px(8.0))
            .pb(px(12.0))
            .border_l_1()
            .border_color(theme.border)
            .bg(theme.panel_bg())
            .child(
                div()
                    .flex_none()
                    .pb(px(6.0))
                    .text_size(px(12.0))
                    .text_color(theme.text_muted)
                    .child("Files"),
            )
            .child(list);
        if let Some(name) = current {
            column = column.child(
                div()
                    .flex_none()
                    .mt(px(8.0))
                    .text_size(px(12.0))
                    .text_color(theme.text_muted)
                    .child(name),
            );
        }
        if let Some(editor) = editor {
            column = column
                .child(
                    div()
                        .id("side-file-editor")
                        .flex_none()
                        .h(px(SIDE_FILE_EDITOR_HEIGHT))
                        .mt(px(4.0))
                        .px(px(8.0))
                        .py(px(6.0))
                        .rounded(px(8.0))
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.surface)
                        .overflow_hidden()
                        .child(editor),
                )
                .child(
                    div()
                        .id("side-file-save")
                        .flex_none()
                        .mt(px(6.0))
                        .px(px(10.0))
                        .py(px(6.0))
                        .rounded(px(6.0))
                        .cursor_pointer()
                        .bg(theme.element_hover)
                        .text_size(px(12.0))
                        .child("Save")
                        .on_click(cx.listener(|this, _, _, cx| this.save_side_file(cx))),
                );
        }
        column.into_any_element()
    }

    fn open_side_file(&mut self, relative: String, window: &mut Window, cx: &mut Context<Self>) {
        let text = self
            .selected_project(cx)
            .and_then(|project| {
                zeron_engine::arbos_layout::read_context(Path::new(&project.path), &relative).ok()
            })
            .unwrap_or_default();
        let editor = cx.new(|cx| {
            let mut input = ComposerInput::with_context("", "ProjectContext", cx)
                .with_viewport_height(SIDE_FILE_EDITOR_HEIGHT - 12.0);
            input.set_text(text, cx);
            input.show_from_start(cx);
            input
        });
        window.focus(&editor.focus_handle(cx), cx);
        self.project.side_editor = Some(editor);
        self.project.side_file = Some(relative);
        self.project.surface = Surface::Chat;
        cx.notify();
    }

    fn save_side_file(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.selected_project(cx) else {
            return;
        };
        let Some(relative) = self.project.side_file.clone() else {
            return;
        };
        let Some(editor) = self.project.side_editor.clone() else {
            return;
        };
        let text = editor.read(cx).text().to_owned();
        if zeron_engine::arbos_layout::write_context(Path::new(&project.path), &relative, &text)
            .is_err()
        {
            return;
        }
        cx.notify();
    }

    pub(super) fn render_context_browser(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let Some(project) = self.selected_project(cx) else {
            return div()
                .size_full()
                .pt(px(Theme::TITLEBAR_HEIGHT))
                .child("Add a project to open Context")
                .into_any_element();
        };
        let files = context_files(Path::new(&project.path));
        let current = self.project.context_relative.clone();
        let mut list = div()
            .id("context-file-list")
            .w(px(220.0))
            .h_full()
            .flex_none()
            .border_r_1()
            .border_color(theme.border)
            .pt(px(Theme::TITLEBAR_HEIGHT + 8.0))
            .px(px(8.0))
            .flex()
            .flex_col()
            .gap(px(2.0))
            .overflow_y_scroll();
        for file in files {
            let selected = current.as_deref() == Some(&file);
            let open = file.clone();
            list = list.child(
                div()
                    .id(SharedString::from(format!("context-file-{file}")))
                    .px(px(8.0))
                    .py(px(4.0))
                    .rounded(px(6.0))
                    .text_size(px(12.0))
                    .cursor_pointer()
                    .text_color(if selected {
                        theme.text
                    } else {
                        theme.text_muted
                    })
                    .bg(if selected {
                        theme.element_hover
                    } else {
                        gpui::transparent_black()
                    })
                    .child(file)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_context_file(open.clone(), cx);
                    })),
            );
        }
        let editor = self.project.editor.clone();
        let save = current.clone();
        let status_page = current.as_deref() == Some("status.md");
        let page_text = if status_page {
            self.selected_project(cx).and_then(|project| {
                zeron_engine::arbos_layout::read_context(Path::new(&project.path), "status.md").ok()
            })
        } else {
            None
        };
        let title = if status_page {
            "Status".to_owned()
        } else {
            current.clone().unwrap_or_else(|| "Context".into())
        };
        div()
            .size_full()
            .flex()
            .flex_row()
            .child(list)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .pt(px(Theme::TITLEBAR_HEIGHT + 8.0))
                    .px(px(12.0))
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(12.0))
                            .text_color(theme.text_muted)
                            .child(title),
                    )
                    .when(status_page, |column| {
                        column.child(render_status_page(
                            page_text.as_deref().unwrap_or(""),
                            &theme,
                        ))
                    })
                    .when(!status_page, |column| {
                        column.children(editor.map(|editor| {
                            div().flex_1().min_h_0().child(editor).into_any_element()
                        }))
                    })
                    .when(!status_page && save.is_some(), |column| {
                        column.child(
                            div()
                                .id("context-save")
                                .flex_none()
                                .mb(px(12.0))
                                .px(px(10.0))
                                .py(px(6.0))
                                .rounded(px(6.0))
                                .cursor_pointer()
                                .bg(theme.element_hover)
                                .text_size(px(12.0))
                                .child("Save")
                                .on_click(cx.listener(|this, _, _, cx| this.save_context_file(cx))),
                        )
                    }),
            )
            .into_any_element()
    }

    pub(super) fn open_project(&mut self, space_id: &str, cx: &mut Context<Self>) {
        let path = self
            .project_rows(cx)
            .into_iter()
            .find(|row| row.id == space_id)
            .map(|row| row.path);
        if let Some(path) = path {
            let _ = zeron_engine::arbos_live::mark_seen(Path::new(&path));
        }
        self.project.unread = false;
        self.project.surface = Surface::Chat;
        self.project.icon_for = None;
        let main = self.main_chat_id(space_id, cx);
        self.state.update(cx, |state, cx| {
            state.select_space(Some(space_id.to_owned()), cx)
        });
        if let Some(chat_id) = main {
            self.open_chat(chat_id, cx);
        } else {
            self.open_new_session(Some(space_id.to_owned()), cx);
        }
    }

    pub(super) fn open_project_chat(&mut self, cx: &mut Context<Self>) {
        let space_id = self.state.read(cx).selected_space.clone().or_else(|| {
            self.state
                .read(cx)
                .spaces
                .first()
                .map(|space| space.id.clone())
        });
        let Some(space_id) = space_id else {
            self.open_add_space(cx);
            return;
        };
        let count = self
            .state
            .read(cx)
            .chats
            .iter()
            .filter(|chat| chat.space_id.as_deref() == Some(space_id.as_str()))
            .filter(|chat| chat.parent_chat_id.is_none())
            .count();
        let chat_id = uuid::Uuid::new_v4().to_string();
        let title = format!("Chat {}", count + 1);
        self.mutate_seq(
            vec![
                serde_json::json!({
                    "op": "createChat",
                    "chatId": chat_id,
                    "spaceId": space_id,
                }),
                serde_json::json!({
                    "op": "renameChat",
                    "chatId": chat_id,
                    "title": title,
                }),
            ],
            cx,
        );
        self.project.surface = Surface::Chat;
        self.open_chat(chat_id, cx);
    }

    pub(super) fn jump_to_project(&mut self, slot: usize, cx: &mut Context<Self>) {
        let id = self
            .state
            .read(cx)
            .spaces
            .get(slot)
            .map(|space| space.id.clone());
        if let Some(id) = id {
            self.open_project(&id, cx);
        }
    }

    pub(super) fn show_project_context(&mut self, cx: &mut Context<Self>) {
        self.project.icon_for = None;
        self.open_context_file("status.md".into(), cx);
    }

    pub(super) fn open_icon_menu(&mut self, space_id: String, cx: &mut Context<Self>) {
        if self.project.emoji_input.is_none() {
            self.project.emoji_input =
                Some(cx.new(|cx| ComposerInput::with_context("Emoji", "ProjectIcon", cx)));
        }
        if self.project.image_input.is_none() {
            self.project.image_input = Some(
                cx.new(|cx| ComposerInput::with_context("Image file path", "ProjectIcon", cx)),
            );
        }
        self.project.icon_for = Some(space_id);
        cx.notify();
    }

    fn set_project_icon(&mut self, path: String, icon: String, cx: &mut Context<Self>) {
        self.project.icons.clear();
        self.project.icon_loading.clear();
        self.project.icon_for = None;
        self.mutate(
            serde_json::json!({
                "op": "setProjectIcon",
                "path": path,
                "icon": icon,
            }),
            cx,
        );
    }

    fn open_context_file(&mut self, relative: String, cx: &mut Context<Self>) {
        if relative == "status.md" {
            if let Some(project) = self.selected_project(cx) {
                let _ = zeron_engine::arbos_layout::publish_status_page(Path::new(&project.path));
            }
        }
        let text = self
            .selected_project(cx)
            .and_then(|project| {
                zeron_engine::arbos_layout::read_context(Path::new(&project.path), &relative).ok()
            })
            .unwrap_or_default();
        self.project.context_relative = Some(relative.clone());
        self.project.surface = Surface::Context;
        if relative == "status.md" {
            cx.notify();
            return;
        }
        if let Some(editor) = self.project.editor.clone() {
            editor.update(cx, |input, cx| input.set_text(text, cx));
        } else {
            self.project.editor = Some(cx.new(|cx| {
                let mut input = ComposerInput::with_context("", "ProjectContext", cx);
                input.set_text(text, cx);
                input
            }));
        }
        cx.notify();
    }

    fn save_context_file(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.selected_project(cx) else {
            return;
        };
        let Some(relative) = self.project.context_relative.clone() else {
            return;
        };
        let Some(editor) = self.project.editor.clone() else {
            return;
        };
        let text = editor.read(cx).text().to_owned();
        self.mutate(
            serde_json::json!({
                "op": "writeContext",
                "path": project.path,
                "relativePath": relative,
                "text": text,
            }),
            cx,
        );
    }

    fn ensure_status_watch(&mut self, project: String, cx: &mut Context<Self>) {
        if self.project.watched.as_deref() == Some(project.as_str()) {
            return;
        }
        self.project.watched = Some(project.clone());
        self.project.status = zeron_engine::arbos_layout::status_items(Path::new(&project));
        let view = zeron_engine::arbos_layout::goal_view(Path::new(&project));
        self.apply_goal_row(Path::new(&project));
        self.project.skill = view.skill;
        let live = zeron_engine::arbos_live::live(Path::new(&project));
        self.project.unread = live.unread;
        self.project.step = live.step;
        self.project.listening_open = false;
        self.project.listening_hover = false;
        let subscriptions = zeron_engine::arbos_wakes::subscriptions(Path::new(&project));
        let agents = zeron_engine::arbos_layout::agents(Path::new(&project));
        self.project.worker_steps = worker_steps(&agents);
        self.project.listening = self.listening_rows_now(&subscriptions, &agents, cx);
        self.project.watch = Some(cx.spawn(async move |this, cx| {
            let root = PathBuf::from(&project).join(".arbos").join("context");
            let status_file = root.join("status.md");
            let goals_file = root.join("GOALS.md");
            let goal_file = root.join("goal.toml");
            let project_path = PathBuf::from(&project);
            let mut seen = String::new();
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(400))
                    .await;
                let status_text = std::fs::read_to_string(&status_file).unwrap_or_default();
                let goals_text = std::fs::read_to_string(&goals_file).unwrap_or_default();
                let goal_text = std::fs::read_to_string(&goal_file).unwrap_or_default();
                let live = zeron_engine::arbos_live::live(&project_path);
                let row = zeron_engine::arbos_goal::goal_row(&project_path);
                let elapsed = row.as_ref().map(|row| row.elapsed_seconds).unwrap_or(0);
                let subscriptions = zeron_engine::arbos_wakes::subscriptions(&project_path);
                let agents = zeron_engine::arbos_layout::agents(&project_path);
                let snap = format!(
                    "{status_text}\n---\n{goals_text}\n---\n{goal_text}\n---\n{}\n{}\n---\n{elapsed}\n---\n{subscriptions:?}\n{agents:?}",
                    live.unread, live.step
                );
                if snap == seen {
                    continue;
                }
                seen = snap;
                let items = zeron_engine::arbos_layout::status_items(&project_path);
                let view = zeron_engine::arbos_layout::goal_view(&project_path);
                let objective = row.as_ref().map(|row| row.objective.clone());
                let state = row.as_ref().map(|row| row.state.clone());
                let unread = live.unread;
                let step = live.step;
                let _ = this.update(cx, |shell, cx| {
                    shell.project.status = items;
                    shell.project.goal_objective = objective;
                    shell.project.goal_state = state;
                    shell.project.goal_elapsed = elapsed;
                    shell.project.skill = view.skill;
                    shell.project.unread = unread;
                    shell.project.step = step;
                    shell.project.worker_steps = worker_steps(&agents);
                    shell.project.listening =
                        shell.listening_rows_now(&subscriptions, &agents, cx);
                    cx.notify();
                });
            }
        }));
    }

    /// The pill's rows, with each working worker's start read from its chat.
    fn listening_rows_now(
        &self,
        subscriptions: &[zeron_engine::arbos_wakes::SubscriptionView],
        agents: &[zeron_engine::arbos_layout::AgentView],
        cx: &gpui::App,
    ) -> Vec<ListeningRow> {
        let state = self.state.read(cx);
        let since = |chat_id: &str| {
            state
                .chats
                .iter()
                .find(|chat| chat.id == chat_id)
                .map(|chat| chat.created_at)
        };
        listening_rows(subscriptions, agents, &since)
    }

    fn project_rows(&self, cx: &gpui::App) -> Vec<ProjectRow> {
        self.state
            .read(cx)
            .spaces
            .iter()
            .map(|space| ProjectRow {
                id: space.id.clone(),
                path: space.path.clone(),
                name: space.display_name().to_owned(),
                home: zeron_engine::arbos_layout::home_of(Path::new(&space.path)),
            })
            .collect()
    }

    pub(super) fn selected_project(&self, cx: &gpui::App) -> Option<ProjectRow> {
        let state = self.state.read(cx);
        let space = state
            .selected_space
            .as_ref()
            .and_then(|id| state.spaces.iter().find(|space| &space.id == id))
            .or_else(|| state.spaces.first())?;
        Some(ProjectRow {
            id: space.id.clone(),
            path: space.path.clone(),
            name: space.display_name().to_owned(),
            home: zeron_engine::arbos_layout::home_of(Path::new(&space.path)),
        })
    }

    fn chat_tabs(&self, space_id: &str, cx: &gpui::App) -> Vec<ChatTab> {
        let state = self.state.read(cx);
        let path = state
            .spaces
            .iter()
            .find(|space| space.id == space_id)
            .map(|space| space.path.clone());
        let agents = path
            .as_deref()
            .map(|path| zeron_engine::arbos_layout::agents(std::path::Path::new(path)))
            .unwrap_or_default();
        let now = chrono::Utc::now();
        let mut chats: Vec<_> = state
            .chats
            .iter()
            .filter(|chat| chat.space_id.as_deref() == Some(space_id))
            .filter(|chat| !chat.archived)
            .filter(|chat| chat.parent_chat_id.is_none())
            .cloned()
            .collect();
        chats.sort_by(|left, right| {
            // Workers sit ahead of other chats. A worker that is still going,
            // then the newest chat, takes the two visible slots.
            let rank = |id: &str| {
                let agent = agents.iter().find(|agent| agent.chat_id == id);
                let worker = agent.is_some_and(|agent| agent.role == "worker");
                let working = agent.is_some_and(|agent| status_is_live(&agent.status));
                (worker, working)
            };
            rank(&right.id)
                .cmp(&rank(&left.id))
                .then(right.created_at.cmp(&left.created_at))
        });
        chats
            .into_iter()
            .map(|chat| {
                let agent = agents.iter().find(|agent| agent.chat_id == chat.id);
                let working = matches!(
                    state.indicator_for(&chat.id, now),
                    crate::state::Indicator::Working | crate::state::Indicator::AwaitingInput
                );
                let stored = agent.map(|agent| agent.status.as_str()).unwrap_or("");
                let status = tab_status(working, stored);
                ChatTab {
                    main: chat.id.ends_with("-main"),
                    worker: agent.is_some_and(|agent| agent.role == "worker"),
                    title: chat.title.clone().unwrap_or_else(|| "Chat".into()),
                    id: chat.id,
                    status,
                }
            })
            .collect()
    }

    fn main_chat_id(&self, space_id: &str, cx: &gpui::App) -> Option<String> {
        self.chat_tabs(space_id, cx)
            .into_iter()
            .find(|tab| tab.main)
            .map(|tab| tab.id)
    }

    pub(super) fn selected_is_main(&self, cx: &gpui::App) -> bool {
        let state = self.state.read(cx);
        let Some(selected) = state.selected_chat.as_deref() else {
            return false;
        };
        let Some(space) = state.selected_space.as_deref() else {
            return false;
        };
        self.main_chat_id(space, cx).as_deref() == Some(selected)
    }

    fn rail_button(
        &mut self,
        index: usize,
        row: &ProjectRow,
        selected: Option<&str>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let active = selected == Some(row.id.as_str());
        let space_id = row.id.clone();
        let menu_id = row.id.clone();
        let icon = self.rail_icon(row, cx);
        let unread =
            self.project.unread && self.project.watched.as_deref() == Some(row.path.as_str());
        div()
            .id(SharedString::from(format!("project-rail-{}", row.id)))
            .w(px(80.0))
            .py(px(4.0))
            .rounded(px(8.0))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(2.0))
            .cursor_pointer()
            .border_1()
            .border_color(if active { theme.accent } else { theme.border })
            .bg(if active {
                theme.element_hover
            } else {
                gpui::transparent_black()
            })
            .aria_label(format!("{} {} {}", index + 1, row.name, row.home))
            .child(
                div().relative().child(icon).when(unread, |row| {
                    row.child(
                        div()
                            .absolute()
                            .top(px(0.0))
                            .right(px(0.0))
                            .size(px(9.0))
                            .rounded_full()
                            .border_1()
                            .border_color(theme.bg)
                            .bg(theme.warning),
                    )
                }),
            )
            .child(
                div()
                    .max_w(px(76.0))
                    .overflow_hidden()
                    .text_size(px(11.0))
                    .line_height(px(13.0))
                    .text_color(if active { theme.text } else { theme.text_muted })
                    .child(row.name.clone()),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.open_project(&space_id, cx);
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                    this.open_icon_menu(menu_id.clone(), cx);
                }),
            )
            .into_any_element()
    }

    fn rail_icon(&mut self, row: &ProjectRow, cx: &mut Context<Self>) -> AnyElement {
        let icon = zeron_engine::arbos_layout::icon_of(Path::new(&row.path));
        if let Some(emoji) = icon.strip_prefix("emoji:") {
            return div()
                .text_size(px(16.0))
                .child(emoji.to_owned())
                .into_any_element();
        }
        let key = format!("{}|{icon}", row.path);
        if let Some(media) = self.project.icons.get(&key) {
            return gpui::img(media.image.clone())
                .size(px(22.0))
                .object_fit(gpui::ObjectFit::Contain)
                .into_any_element();
        }
        self.request_icon(key, row.path.clone(), icon, cx);
        let letter = row.name.chars().next().unwrap_or('?').to_string();
        div().text_size(px(12.0)).child(letter).into_any_element()
    }

    fn request_icon(&mut self, key: String, project: String, icon: String, cx: &mut Context<Self>) {
        if self.project.icons.contains_key(&key) || !self.project.icon_loading.insert(key.clone()) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let media = cx
                .background_executor()
                .spawn(async move { load_media(Path::new(&project), &icon) })
                .await;
            // A miss stays in `icon_loading`: the rail renders every frame,
            // and forgetting the miss would re-probe the disk on each one and
            // repaint forever. `set_project_icon` clears the set on a change.
            let Some(media) = media else {
                return;
            };
            let _ = this.update(cx, |shell, cx| {
                shell.project.icon_loading.remove(&key);
                shell.project.icons.insert(key, media);
                cx.notify();
            });
        })
        .detach();
    }

    fn rail_plus(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("project-rail-add")
            .size(px(32.0))
            .rounded(px(8.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .text_size(px(18.0))
            .text_color(theme.text_muted)
            .aria_label("Add project")
            .child("+")
            .on_click(cx.listener(|this, _, _, cx| this.open_add_space(cx)))
            .into_any_element()
    }

    fn chat_tab(
        &self,
        tab: &ChatTab,
        selected: Option<&str>,
        surface: Surface,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let active = surface == Surface::Chat && selected == Some(tab.id.as_str());
        let chat_id = tab.id.clone();
        let mut label = div().flex().flex_row().items_center().gap(px(4.0));
        if tab.main {
            label = label.child("●");
        } else if tab.worker {
            label = label.child("⚙");
        }
        label = label.child(tab.title.clone());
        if tab.worker && status_is_live(&tab.status) {
            // The same pulse while the worker goes; amber while it waits on
            // its model.
            let dot_color = if tab.status == "waiting" {
                theme.warning
            } else {
                theme.accent
            };
            let pulse_id = SharedString::from(format!("worker-pulse-{}", tab.id));
            label = label.child(div().text_color(dot_color).child("●").with_animation(
                pulse_id,
                Animation::new(Duration::from_millis(900)).repeat(),
                |dot, t| {
                    let wave = (t * std::f32::consts::PI).sin().abs();
                    dot.opacity(1.0 - 0.65 * wave)
                },
            ));
        } else if tab.worker && tab.status == "error" {
            label = label.child("✕");
        } else if tab.worker && (tab.status == "done" || tab.status == "stopped") {
            label = label.child("✓");
        }
        div()
            .id(SharedString::from(format!("project-tab-{}", tab.id)))
            .h(px(26.0))
            .px(px(10.0))
            .flex()
            .items_center()
            .rounded(px(6.0))
            .cursor_pointer()
            .text_size(px(12.0))
            .text_color(if active { theme.text } else { theme.text_muted })
            .bg(if active {
                theme.element_hover
            } else {
                gpui::transparent_black()
            })
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.project.surface = Surface::Chat;
                this.open_chat(chat_id.clone(), cx);
            }))
            .into_any_element()
    }

    fn overflow_tab(
        &self,
        hidden: usize,
        next: Option<String>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id("project-tab-overflow")
            .h(px(26.0))
            .px(px(8.0))
            .flex()
            .items_center()
            .rounded(px(6.0))
            .cursor_pointer()
            .text_size(px(12.0))
            .text_color(theme.text_muted)
            .child(format!("+{hidden}"))
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(chat_id) = next.clone() {
                    this.project.surface = Surface::Chat;
                    this.open_chat(chat_id, cx);
                }
            }))
            .into_any_element()
    }

    fn new_chat_tab(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("project-tab-new")
            .size(px(26.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.0))
            .cursor_pointer()
            .text_size(px(16.0))
            .text_color(theme.text_muted)
            .aria_label("New chat")
            .child("+")
            .on_click(cx.listener(|this, _, _, cx| this.open_project_chat(cx)))
            .into_any_element()
    }

}

enum StatusBlock {
    Link(String),
    Tldr(Vec<String>),
    Heading(String),
    Item {
        done: bool,
        label: String,
        readout: String,
    },
}

fn parse_status_blocks(text: &str) -> Vec<StatusBlock> {
    let mut blocks = Vec::new();
    let mut tldr: Option<Vec<String>> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if line == "<tldr>" {
            tldr = Some(Vec::new());
            continue;
        }
        if line == "</tldr>" {
            if let Some(lines) = tldr.take() {
                blocks.push(StatusBlock::Tldr(lines));
            }
            continue;
        }
        if let Some(lines) = tldr.as_mut() {
            if let Some(text) = line.strip_prefix("- ") {
                lines.push(readable_item(text));
            }
            continue;
        }
        if let Some(heading) = line.strip_prefix("## ") {
            blocks.push(StatusBlock::Heading(heading.trim().to_owned()));
            continue;
        }
        if let Some(rest) = line.strip_prefix("- [x] ") {
            let (label, readout) = split_status_item(rest);
            blocks.push(StatusBlock::Item {
                done: true,
                label,
                readout,
            });
            continue;
        }
        if let Some(rest) = line.strip_prefix("- [ ] ") {
            let (label, readout) = split_status_item(rest);
            blocks.push(StatusBlock::Item {
                done: false,
                label,
                readout,
            });
            continue;
        }
        if line.starts_with('[') {
            if let Some(label) = line.split_once(']').map(|(label, _)| label.trim_start_matches('['))
            {
                blocks.push(StatusBlock::Link(label.to_owned()));
            }
        }
    }
    blocks
}

fn split_status_item(rest: &str) -> (String, String) {
    let rest = rest.trim();
    let (head, readout) = rest
        .split_once(" — ")
        .map(|(head, readout)| (head.trim(), readout.trim().to_owned()))
        .unwrap_or((rest, String::new()));
    let label = if let Some(body) = head.strip_prefix('[') {
        body.split_once(']')
            .map(|(label, _)| label.trim().to_owned())
            .unwrap_or_else(|| head.to_owned())
    } else {
        head.to_owned()
    };
    (label, readout)
}

fn readable_item(text: &str) -> String {
    let (label, readout) = split_status_item(text);
    if readout.is_empty() {
        label
    } else {
        format!("{label} — {readout}")
    }
}

fn render_status_page(text: &str, theme: &Theme) -> AnyElement {
    let mut page = div()
        .id("status-page")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .pb(px(16.0));
    for block in parse_status_blocks(text) {
        page = match block {
            StatusBlock::Link(label) => page.child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme.text_muted)
                    .child(label),
            ),
            StatusBlock::Tldr(lines) => {
                let mut card = div()
                    .p(px(10.0))
                    .rounded(px(8.0))
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.border)
                    .flex()
                    .flex_col()
                    .gap(px(4.0));
                for line in lines {
                    card = card.child(
                        div()
                            .text_size(px(13.0))
                            .text_color(theme.text)
                            .child(line),
                    );
                }
                page.child(card)
            }
            StatusBlock::Heading(text) => page.child(
                div()
                    .mt(px(8.0))
                    .text_size(px(16.0))
                    .text_color(theme.text)
                    .child(text),
            ),
            StatusBlock::Item {
                done,
                label,
                readout,
            } => {
                let mark = if done { "✓" } else { "○" };
                let line = if readout.is_empty() {
                    format!("{mark}  {label}")
                } else {
                    format!("{mark}  {label} — {readout}")
                };
                page.child(
                    div()
                        .text_size(px(13.0))
                        .text_color(theme.text)
                        .child(line),
                )
            }
        };
    }
    page.into_any_element()
}

trait SplitMain {
    fn let_one(self) -> (Option<ChatTab>, Vec<ChatTab>);
}

impl SplitMain for (Vec<ChatTab>, Vec<ChatTab>) {
    fn let_one(self) -> (Option<ChatTab>, Vec<ChatTab>) {
        let (mut main, extras) = self;
        (main.pop(), extras)
    }
}

fn format_goal_clock(seconds: u64) -> String {
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let secs = seconds % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{secs:02}")
    } else {
        format!("{minutes}:{secs:02}")
    }
}

fn goal_pause_label(state: &str) -> Option<&'static str> {
    match state {
        "active" => Some("Pause"),
        "paused" => Some("Resume"),
        "complete" => None,
        _ => None,
    }
}

/// A person can complete an active or paused goal; a complete one is done.
fn goal_can_complete(state: &str) -> bool {
    matches!(state, "active" | "paused")
}

fn menu_label(text: &str, theme: &Theme) -> AnyElement {
    div()
        .text_size(px(12.0))
        .text_color(theme.text)
        .child(text.to_owned())
        .into_any_element()
}

fn visible_extra_indexes(extras: &[ChatTab], selected: Option<&str>) -> Vec<usize> {
    let mut shown: Vec<usize> = (0..extras.len().min(EXTRA_VISIBLE)).collect();
    let Some(selected) = selected else {
        return shown;
    };
    let Some(pos) = extras.iter().position(|tab| tab.id == selected) else {
        return shown;
    };
    if pos >= EXTRA_VISIBLE && !shown.is_empty() {
        shown.pop();
        shown.push(pos);
    }
    shown
}

fn worker_context_file(project: &Path, chat_id: &str) -> Option<(String, String)> {
    let agent = zeron_engine::arbos_layout::agents(project)
        .into_iter()
        .find(|agent| agent.chat_id == chat_id && agent.role == "worker")?;
    let haystack = format!("{}\n{}", agent.brief, agent.result);
    let path = context_files(project).into_iter().find(|file| {
        file.starts_with("research/") && !file.ends_with('/') && haystack.contains(file)
    })?;
    let root = project.join(".arbos").join("context");
    let full = context_child(&root, &path)?;
    let text = std::fs::read_to_string(&full).ok()?;
    let body = text.trim();
    if body.is_empty() {
        return None;
    }
    let body = if body.chars().count() > 1200 {
        let end = body
            .char_indices()
            .nth(1200)
            .map(|(index, _)| index)
            .unwrap_or(body.len());
        format!("{}…", body[..end].trim_end())
    } else {
        body.to_owned()
    };
    Some((path, body))
}

fn context_child(root: &Path, relative: &str) -> Option<PathBuf> {
    if relative.is_empty() || relative.contains('\0') {
        return None;
    }
    let mut full = root.to_path_buf();
    for part in relative.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return None;
        }
        full.push(part);
    }
    full.is_file().then_some(full)
}

fn context_files(project: &Path) -> Vec<String> {
    let root = project.join(".arbos").join("context");
    let mut files = Vec::new();
    walk_context(&root, &root, &mut files);
    files
}

fn walk_context(root: &Path, dir: &Path, files: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        if path.file_name().is_some_and(|name| name == ".gitkeep") {
            continue;
        }
        if path.is_dir() {
            walk_context(root, &path, files);
        } else if let Ok(relative) = path.strip_prefix(root) {
            files.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
}

fn load_media(project: &Path, icon: &str) -> Option<MediaImage> {
    if icon == "repo" {
        return crate::shell::project_icon::load_repo_icon(project);
    }
    let path = icon.strip_prefix("image:")?;
    let bytes = std::fs::read(path).ok()?;
    let mime = if path.ends_with(".svg") {
        "image/svg+xml"
    } else if path.ends_with(".jpg") || path.ends_with(".jpeg") {
        "image/jpeg"
    } else if path.ends_with(".webp") {
        "image/webp"
    } else if path.ends_with(".gif") {
        "image/gif"
    } else {
        "image/png"
    };
    decode_project_icon(mime, bytes).ok()
}

#[cfg(test)]
mod tests {
    use zeron_engine::arbos_layout::AgentView;

    use super::*;

    fn agent(id: &str, status: &str, step: &str) -> AgentView {
        AgentView {
            id: id.into(),
            name: id.into(),
            role: "worker".into(),
            chat_id: format!("space-w-{id}"),
            status: status.into(),
            brief: String::new(),
            result: String::new(),
            machine: "local".into(),
            model: String::new(),
            step: step.into(),
        }
    }

    #[test]
    fn a_waiting_worker_reads_waiting_even_while_its_session_runs() {
        assert_eq!(tab_status(true, "waiting"), "waiting");
        assert_eq!(tab_status(false, "waiting"), "waiting");
        assert_eq!(tab_status(true, "done"), "working");
        assert_eq!(tab_status(false, "working"), "working");
        assert_eq!(tab_status(false, "error"), "error");
        assert_eq!(tab_status(false, ""), "");
    }

    #[test]
    fn worker_steps_keep_only_live_workers_with_a_line() {
        let steps = worker_steps(&[
            agent("a", "waiting", " Waiting on the model — try 3 of 8, next in 32 s "),
            agent("b", "working", ""),
            agent("c", "error", "The model gave up: no reason given"),
        ]);
        assert_eq!(
            steps.get("space-w-a").map(String::as_str),
            Some("Waiting on the model — try 3 of 8, next in 32 s")
        );
        assert!(!steps.contains_key("space-w-b"));
        assert!(!steps.contains_key("space-w-c"));
    }
}

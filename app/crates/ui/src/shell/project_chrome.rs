//! Projects rail, the tab strip, the Context browser, and the status checklist.
//! The rail replaces the session list. Each icon is one folder.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{
    Animation, AnimationExt, AnyElement, Context, Entity, InteractiveElement, IntoElement,
    MouseButton, MouseDownEvent, ParentElement, SharedString, StatefulInteractiveElement, Styled,
    Task, div, prelude::*, px,
};

use crate::composer::ComposerInput;
use crate::image_media::{MediaImage, decode_project_icon};
use crate::theme::Theme;

use super::Shell;

pub(super) const RAIL_WIDTH: f32 = 88.0;
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
            watched: None,
            watch: None,
            icons: HashMap::new(),
            icon_loading: HashSet::new(),
        }
    }
}

struct ProjectRow {
    id: String,
    path: String,
    name: String,
    home: String,
}

struct ChatTab {
    id: String,
    title: String,
    main: bool,
    worker: bool,
    /// `working`, `done`, `error`, `stopped`, or empty.
    status: String,
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
            .absolute()
            .top(px(Theme::TITLEBAR_HEIGHT))
            .left_0()
            .right_0()
            .h(px(TAB_HEIGHT))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .px(px(8.0))
            .bg(theme.surface)
            .border_b_1()
            .border_color(theme.border)
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation());
        if let Some(tab) = main {
            strip = strip.child(self.chat_tab(&tab, selected.as_deref(), surface, &theme, cx));
            strip = strip.child(
                div()
                    .w(px(1.0))
                    .h(px(16.0))
                    .bg(theme.border)
                    .mx(px(4.0)),
            );
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
        strip
            .child(div().flex_1())
            .child(self.context_tab(surface, &theme, cx))
            .into_any_element()
    }

    pub(super) fn render_icon_menu(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let space_id = self.project.icon_for.clone()?;
        let project = self.project_rows(cx).into_iter().find(|row| row.id == space_id)?;
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

    pub(super) fn render_context_browser(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let Some(project) = self.selected_project(cx) else {
            return div()
                .size_full()
                .pt(px(Theme::TITLEBAR_HEIGHT + TAB_HEIGHT))
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
            .pt(px(Theme::TITLEBAR_HEIGHT + TAB_HEIGHT + 8.0))
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
                    .text_color(if selected { theme.text } else { theme.text_muted })
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
                    .pt(px(Theme::TITLEBAR_HEIGHT + TAB_HEIGHT + 8.0))
                    .px(px(12.0))
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(12.0))
                            .text_color(theme.text_muted)
                            .child(current.unwrap_or_else(|| "Context".into())),
                    )
                    .children(editor.map(|editor| {
                        div().flex_1().min_h_0().child(editor).into_any_element()
                    }))
                    .when(save.is_some(), |column| {
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
        self.project.surface = Surface::Chat;
        self.project.icon_for = None;
        let main = self.main_chat_id(space_id, cx);
        self.state
            .update(cx, |state, cx| state.select_space(Some(space_id.to_owned()), cx));
        if let Some(chat_id) = main {
            self.open_chat(chat_id, cx);
        } else {
            self.open_new_session(Some(space_id.to_owned()), cx);
        }
    }

    pub(super) fn open_project_chat(&mut self, cx: &mut Context<Self>) {
        let space_id = self
            .state
            .read(cx)
            .selected_space
            .clone()
            .or_else(|| {
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
        if self.project.context_relative.is_none() {
            self.open_context_file("GOALS.md".into(), cx);
        } else {
            self.project.surface = Surface::Context;
            cx.notify();
        }
    }

    pub(super) fn open_icon_menu(&mut self, space_id: String, cx: &mut Context<Self>) {
        if self.project.emoji_input.is_none() {
            self.project.emoji_input = Some(cx.new(|cx| {
                ComposerInput::with_context("Emoji", "ProjectIcon", cx)
            }));
        }
        if self.project.image_input.is_none() {
            self.project.image_input = Some(cx.new(|cx| {
                ComposerInput::with_context("Image file path", "ProjectIcon", cx)
            }));
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
        let text = self
            .selected_project(cx)
            .and_then(|project| {
                zeron_engine::arbos_layout::read_context(Path::new(&project.path), &relative).ok()
            })
            .unwrap_or_default();
        self.project.context_relative = Some(relative);
        self.project.surface = Surface::Context;
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
        self.project.watch = Some(cx.spawn(async move |this, cx| {
            let file = PathBuf::from(&project)
                .join(".arbos")
                .join("context")
                .join("status.md");
            let mut seen = String::new();
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(400))
                    .await;
                let text = std::fs::read_to_string(&file).unwrap_or_default();
                if text == seen {
                    continue;
                }
                seen = text;
                let items = zeron_engine::arbos_layout::status_items(Path::new(&project));
                let _ = this.update(cx, |shell, cx| {
                    shell.project.status = items;
                    cx.notify();
                });
            }
        }));
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

    fn selected_project(&self, cx: &gpui::App) -> Option<ProjectRow> {
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
            let worker = |id: &str| {
                agents
                    .iter()
                    .any(|agent| agent.chat_id == id && agent.role == "worker")
            };
            worker(&right.id)
                .cmp(&worker(&left.id))
                .then(left.created_at.cmp(&right.created_at))
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
                let status = if working || stored == "working" {
                    "working".into()
                } else {
                    stored.to_owned()
                };
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

    fn selected_is_main(&self, cx: &gpui::App) -> bool {
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
            .child(icon)
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

    fn request_icon(
        &mut self,
        key: String,
        project: String,
        icon: String,
        cx: &mut Context<Self>,
    ) {
        if self.project.icons.contains_key(&key) || !self.project.icon_loading.insert(key.clone()) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let media = cx
                .background_executor()
                .spawn(async move { load_media(Path::new(&project), &icon) })
                .await;
            let _ = this.update(cx, |shell, cx| {
                shell.project.icon_loading.remove(&key);
                if let Some(media) = media {
                    shell.project.icons.insert(key, media);
                }
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
        let mut label = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0));
        if tab.main {
            label = label.child("●");
        } else if tab.worker {
            label = label.child("⚙");
        }
        label = label.child(tab.title.clone());
        if tab.worker && tab.status == "working" {
            let pulse_id = SharedString::from(format!("worker-pulse-{}", tab.id));
            label = label.child(
                div()
                    .text_color(theme.accent)
                    .child("●")
                    .with_animation(
                        pulse_id,
                        Animation::new(Duration::from_millis(900)).repeat(),
                        |dot, t| {
                            let wave = (t * std::f32::consts::PI).sin().abs();
                            dot.opacity(1.0 - 0.65 * wave)
                        },
                    ),
            );
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

    fn context_tab(
        &self,
        surface: Surface,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let active = surface == Surface::Context;
        div()
            .id("project-tab-context")
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
            .child("Context")
            .on_click(cx.listener(|this, _, _, cx| this.show_project_context(cx)))
            .into_any_element()
    }
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

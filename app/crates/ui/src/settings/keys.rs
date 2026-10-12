//! Settings → Keys. Saves a bring-your-own-key provider.
//!
//! The page asks the engine to write the fx connection and to put the secret
//! in the keychain (macOS) or a mode 600 file outside the project (Linux).
//! The secret is not written into a settings file.

use gpui::{Context, Entity, EventEmitter, IntoElement, Render, SharedString, Task, div, prelude::*, px};
use zeron_rpc::methods;

use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::popover;
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::Theme;

const PROVIDERS: &[(&str, &str, &str)] = &[
    ("engy", "engy", "https://api.engy.ai/v1"),
    ("ai-gateway", "AI Gateway", "https://ai-gateway.vercel.sh/v1"),
    ("openrouter", "OpenRouter", "https://openrouter.ai/api/v1"),
    ("ollama", "Ollama", "http://127.0.0.1:11434/v1"),
    ("codex", "Codex OAuth", ""),
    ("grok", "Grok OAuth", ""),
    ("github", "GitHub", ""),
    ("onepassword", "1Password", ""),
];

/// What a worker does with a tool token. Providers have no note.
fn tool_note(provider: &str) -> Option<&'static str> {
    match provider {
        "github" => Some(
            "Workers use this token to push branches and open pull requests. It goes to the keychain, never to a file in the project.",
        ),
        "onepassword" => Some(
            "A service account token. Workers can run op read for the items it can see. It goes to the keychain, never to a file in the project.",
        ),
        _ => None,
    }
}

pub struct KeysPage {
    state: Entity<AppState>,
    provider: &'static str,
    key_input: Entity<ComposerInput>,
    url_input: Entity<ComposerInput>,
    notice: Option<String>,
    error: Option<String>,
    task: Option<Task<()>>,
}

impl EventEmitter<ComposerInputEvent> for KeysPage {}

impl KeysPage {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let key_input = cx.new(|cx| ComposerInput::new("Key", cx));
        let url_input = cx.new(|cx| ComposerInput::new("Base URL", cx));
        url_input.update(cx, |input, cx| {
            input.set_text(PROVIDERS[0].2, cx);
        });
        Self {
            state,
            provider: PROVIDERS[0].0,
            key_input,
            url_input,
            notice: None,
            error: None,
            task: None,
        }
    }

    fn select(&mut self, provider: &'static str, cx: &mut Context<Self>) {
        self.provider = provider;
        self.notice = None;
        self.error = None;
        self.key_input.update(cx, |input, cx| input.set_text("", cx));
        let url = PROVIDERS
            .iter()
            .find(|(id, _, _)| *id == provider)
            .map(|(_, _, url)| *url)
            .unwrap_or("");
        self.url_input
            .update(cx, |input, cx| input.set_text(url, cx));
        cx.notify();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.project_path(cx) else {
            self.error = Some("Open a project first.".into());
            self.notice = None;
            cx.notify();
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.error = Some("Engine not connected".into());
            cx.notify();
            return;
        };
        let secret = self.key_input.read(cx).text().trim().to_owned();
        let base_url = self.url_input.read(cx).text().trim().to_owned();
        let provider = self.provider;
        let params = serde_json::json!({
            "op": "byok",
            "project": project,
            "provider": provider,
            "secret": secret,
            "baseUrl": base_url,
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call(methods::ARBOS_CALL, params).await;
            this.update(cx, |page, cx| {
                match result {
                    Ok(value) => {
                        let place = value
                            .get("place")
                            .and_then(|place| place.as_str())
                            .unwrap_or("store");
                        page.notice = Some(format!("Saved. The key is in the {place}, not in the project."));
                        page.error = None;
                        page.key_input.update(cx, |input, cx| input.set_text("", cx));
                    }
                    Err(err) => {
                        page.error = Some(err.to_string());
                        page.notice = None;
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn project_path(&self, cx: &Context<Self>) -> Option<String> {
        let cwd = self
            .state
            .read(cx)
            .selected_chat_row()
            .and_then(|chat| chat.cwd.clone())?;
        std::path::Path::new(&cwd)
            .join(".arbos")
            .join("project.toml")
            .is_file()
            .then_some(cwd)
    }
}

impl Render for KeysPage {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).for_settings_surface();
        let provider = self.provider;
        let rows = PROVIDERS.iter().map(|(id, label, _)| {
            let selected = *id == provider;
            let id = *id;
            div()
                .id(SharedString::from(format!("keys-provider-{id}")))
                .px(px(12.0))
                .py(px(8.0))
                .rounded(px(8.0))
                .cursor_pointer()
                .text_size(crate::typography::ui_rems(13.0))
                .text_color(if selected { theme.text } else { theme.text_muted })
                .bg(if selected {
                    theme.accent.opacity(0.12)
                } else {
                    gpui::transparent_black()
                })
                .on_click(cx.listener(move |page, _, _, cx| page.select(id, cx)))
                .child(SharedString::from(*label))
        });
        let show_url = matches!(provider, "ai-gateway" | "openrouter" | "ollama");
        let tool_note = tool_note(provider);
        div()
            .id("keys-page")
            .size_full()
            .overflow_y_scroll()
            .child(
                widgets::page_column()
                    .child(widgets::page_header(&theme, "Keys", None))
                    .child(
                        div()
                            .px(px(12.0))
                            .mt(px(8.0))
                            .text_size(crate::typography::ui_rems(12.5))
                            .text_color(theme.text_muted)
                            .child("A key goes to the system keychain on macOS, or to a private file on Linux. fx settings record the connection and the variable name. They do not record the key."),
                    )
                    .child(
                        widgets::section_card(&theme)
                            .mt(px(16.0))
                            .p(px(8.0))
                            .flex()
                            .flex_col()
                            .gap(px(4.0))
                            .children(rows),
                    )
                    .child(
                        div()
                            .mt(px(16.0))
                            .h(px(72.0))
                            .child(popover::dialog_field(self.key_input.clone().into_any_element())),
                    )
                    .when(show_url, |column| {
                        column.child(
                            div()
                                .mt(px(8.0))
                                .h(px(72.0))
                                .child(popover::dialog_field(
                                    self.url_input.clone().into_any_element(),
                                )),
                        )
                    })
                    .when_some(tool_note, |column, note| {
                        column.child(
                            div()
                                .mt(px(8.0))
                                .px(px(12.0))
                                .text_size(crate::typography::ui_rems(12.5))
                                .text_color(theme.text_muted)
                                .child(note),
                        )
                    })
                    .child(
                        widgets::text_action(&theme, widgets::ActionTone::Solid, "Save")
                            .id("keys-save")
                            .mt(px(16.0))
                            .w(px(120.0))
                            .on_click(cx.listener(|page, _, _, cx| page.save(cx))),
                    )
                    .when_some(self.notice.clone(), |column, notice| {
                        column.child(
                            div()
                                .mt(px(12.0))
                                .px(px(12.0))
                                .text_size(crate::typography::ui_rems(12.5))
                                .text_color(theme.text)
                                .child(notice),
                        )
                    })
                    .when_some(self.error.clone(), |column, error| {
                        column.child(widgets::error_strip(&theme, error))
                    }),
            )
    }
}

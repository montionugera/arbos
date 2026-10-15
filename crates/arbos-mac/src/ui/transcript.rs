//! The chat column body: a bottom-aligned `list()` with one row per markdown
//! block (zeron-study §4.1–§4.4). Rows are diffed by `(id, version)` and
//! spliced; streaming text fades in through the veil.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::time::Instant;

use gpui::{
    div, list, prelude::*, px, AnyElement, Context, Entity, FontWeight, ListAlignment,
    ListScrollEvent, ListState, Render, SharedString, StyledText, TextRun, Window,
};

use crate::app::{AppEvent, AppState, DetailsTarget};
use crate::model::{Attachment, Id, Message, MessageStatus, Part, Person, Role, StatusKind};
use crate::ui::approve::{approval_card, ApprovalView};
use crate::ui::attachments;
use crate::ui::cards;
use crate::ui::frost::edge_faded;
use crate::ui::groups;
use crate::ui::markdown::{inline_spans, split_blocks, Block};
use crate::ui::motion::PulseClock;
use crate::ui::people;
use crate::ui::theme::{Theme, BUBBLE_RADIUS};
use crate::ui::veil::Veil;
use crate::ui::widgets::QuestionCard;
use serde_json::Value;

const OVERDRAW_PX: f32 = 320.0;
pub const COLUMN_MAX_WIDTH: f32 = 680.0;
/// 72 ch at 13.5 px (§3.5).
pub const ASSISTANT_MAX_WIDTH: f32 = 540.0;

#[derive(Clone, Debug)]
pub enum RowKind {
    User {
        text: String,
        attachments: Vec<Attachment>,
        pending: bool,
        /// Caption for prompts not typed by the user (a routine, another project).
        sender_label: Option<String>,
        /// Another person in a people chat (D72): left bubble with their
        /// avatar and handle instead of my right bubble.
        from: Option<Person>,
    },
    Markdown {
        block: Block,
        live: bool,
    },
    /// A file the agent produced for the user.
    Attachment {
        attachment: Attachment,
    },
    /// S8: the agent asked a question.
    AskUser {
        text: String,
    },
    /// S18: draft-and-approve card.
    Approval {
        view: ApprovalView,
    },
    /// Error: one line, red glyph, Retry.
    /// A typed question widget (design §10): rendered by a [`QuestionCard`] view.
    Question {
        question_id: String,
    },
    Error {
        code: String,
        message: String,
    },
    /// Tiny inline glyph (■ stopped, ✗ failed) — no words.
    Glyph {
        glyph: &'static str,
    },
    /// The one working indicator for the chat: a pulsing dot under the last
    /// message while anything runs here. `queued` tints it amber.
    Indicator {
        queued: bool,
    },
    /// Debug-only "Show details" disclosure under a finished reply.
    Details,
    /// F6: who speaks next — another Project's glyph and name above its message.
    Sender {
        name: String,
        icon: Option<String>,
        /// Stable seed for the chip colour (the sender id).
        seed: String,
        /// "answering @sam · owned by @jacob" for a shared agent (D72).
        note: Option<String>,
    },
    /// F11: small speaker under a reply; click speaks it (again).
    Speaker {
        /// The speech is already cached (instant replay) vs. will be synthesised.
        cached: bool,
    },
}

/// Which replies get a speaker row (F11).
#[derive(Clone, Copy)]
pub struct SpeechRows<'a> {
    pub spoken: &'a HashSet<Id>,
    pub voice_mode: bool,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub id: String,
    pub version: u64,
    pub message_id: Id,
    pub kind: RowKind,
}

/// Stick-to-bottom velocity spring (zeron-study §4.3). Each frame it moves a
/// fraction of the remaining distance to the end, damped, so streamed text
/// glides instead of jumping; a user wheel-up breaks the pin.
#[derive(Default)]
struct StickSpring {
    velocity: f32,
}

const SPRING_STIFFNESS: f32 = 0.22;
const SPRING_DAMPING: f32 = 0.55;
pub const STICK_THRESHOLD_PX: f32 = 70.0;

pub struct Transcript {
    state: Entity<AppState>,
    chat_id: Option<Id>,
    list: ListState,
    rows: Vec<Row>,
    veils: HashMap<String, Veil>,
    /// One stateful card per typed question, keyed by question id; dropped with its row.
    cards: HashMap<String, Entity<QuestionCard>>,
    pinned_to_bottom: bool,
    spring: StickSpring,
    /// Set by the scroll callback; the next frame re-pins when the user
    /// stopped within `STICK_THRESHOLD_PX` of the end.
    repin_check_pending: bool,
}

impl Transcript {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let list = ListState::new(0, ListAlignment::Bottom, px(OVERDRAW_PX));
        let weak = cx.weak_entity();
        list.set_scroll_handler(move |event: &ListScrollEvent, _, cx| {
            let _ = weak.update(cx, |this, _| this.handle_scroll(event));
        });
        cx.observe(&state, |this, _, cx| this.sync(cx)).detach();
        cx.subscribe(&state, |this, _, event: &AppEvent, cx| match event {
            AppEvent::TranscriptText { chat_id, .. } | AppEvent::TranscriptChanged { chat_id } => {
                if this.chat_id.as_deref() == Some(chat_id.as_str()) {
                    this.sync(cx);
                }
            }
            AppEvent::VoiceTranscript { .. }
            | AppEvent::VoiceSpeech { .. }
            | AppEvent::ReplaySpeech { .. } => {}
        })
        .detach();
        Self {
            state,
            chat_id: None,
            list,
            rows: Vec::new(),
            veils: HashMap::new(),
            cards: HashMap::new(),
            pinned_to_bottom: true,
            spring: StickSpring::default(),
            repin_check_pending: false,
        }
    }

    /// Runs inside the list's own scroll callback while `ListState` is
    /// mutably borrowed, so it must not touch the list. Record the intent
    /// and let the next frame (`step_spring`) read pixel offsets.
    fn handle_scroll(&mut self, event: &ListScrollEvent) {
        // `is_scrolled == false` means the bottom-aligned list snapped to its
        // end: the user is back at the tail. Direction is handled by the
        // wheel listener; this callback never re-pins on its own otherwise.
        if !event.is_scrolled {
            self.pinned_to_bottom = true;
            self.spring.velocity = 0.0;
        }
    }

    /// Wheel/trackpad over the transcript: scrolling up (positive delta,
    /// toward older messages) breaks the pin at once; scrolling down asks the
    /// next frame to re-pin when within `STICK_THRESHOLD_PX` of the end.
    fn on_wheel(&mut self, event: &gpui::ScrollWheelEvent, cx: &mut Context<Self>) {
        let dy = f32::from(event.delta.pixel_delta(px(20.0)).y);
        if dy > 0.0 {
            self.pinned_to_bottom = false;
            self.spring.velocity = 0.0;
            self.repin_check_pending = false;
        } else if dy < 0.0 {
            self.repin_check_pending = true;
        }
        cx.notify();
    }

    /// One spring step per frame while pinned and not yet at the end.
    fn step_spring(&mut self, window: &mut Window, _cx: &mut Context<Self>) {
        if self.repin_check_pending {
            self.repin_check_pending = false;
            let max = f32::from(self.list.max_offset_for_scrollbar().y);
            let cur = -f32::from(self.list.scroll_px_offset_for_scrollbar().y);
            if max - cur <= STICK_THRESHOLD_PX {
                self.pinned_to_bottom = true;
            }
        }
        if !self.pinned_to_bottom {
            return;
        }
        let max = f32::from(self.list.max_offset_for_scrollbar().y);
        let cur = -f32::from(self.list.scroll_px_offset_for_scrollbar().y);
        let remaining = max - cur;
        if remaining.abs() < 0.5 && self.spring.velocity.abs() < 0.5 {
            self.spring.velocity = 0.0;
            return;
        }
        self.spring.velocity =
            self.spring.velocity * (1.0 - SPRING_DAMPING) + remaining * SPRING_STIFFNESS;
        let step = if self.spring.velocity.abs() > remaining.abs() {
            remaining
        } else {
            self.spring.velocity
        };
        if step.abs() >= 0.25 {
            self.list.scroll_by(px(step));
            window.request_animation_frame();
        }
    }

    pub fn sync(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let chat_id = state.active_chat_id();
        let chat_changed = chat_id != self.chat_id;
        let messages: Vec<Message> = chat_id
            .as_deref()
            .map(|id| state.messages_of(id).to_vec())
            .unwrap_or_default();
        self.chat_id = chat_id;

        let show_details = state.show_details;
        let speech = SpeechRows {
            spoken: &state.spoken,
            voice_mode: state.voice_mode,
        };
        let me_id = state.me_id();
        let new_rows = build_rows(&messages, show_details, speech, &me_id);

        // Veils: observe text growth of live markdown rows.
        for row in &new_rows {
            if let RowKind::Markdown { block, live: true } = &row.kind {
                let len = block_text_len(block);
                let veil = self
                    .veils
                    .entry(row.id.clone())
                    .or_insert_with(|| Veil::seeded(0));
                veil.observe(len);
            }
        }
        self.veils
            .retain(|id, _| new_rows.iter().any(|r| r.id == *id));
        self.cards.retain(|qid, _| {
            new_rows
                .iter()
                .any(|r| matches!(&r.kind, RowKind::Question { question_id } if question_id == qid))
        });

        if chat_changed {
            self.rows = new_rows;
            self.list.reset(self.rows.len());
            self.pinned_to_bottom = true;
            self.list.scroll_to_end();
            cx.notify();
            return;
        }

        let prefix = self
            .rows
            .iter()
            .zip(new_rows.iter())
            .take_while(|(a, b)| a.id == b.id && a.version == b.version)
            .count();
        if prefix == self.rows.len() && prefix == new_rows.len() {
            return;
        }
        let old_len = self.rows.len();
        self.rows = new_rows;
        self.list.splice(prefix..old_len, self.rows.len() - prefix);
        // While pinned the spring glides to the new end on the next frames.
        cx.notify();
    }

    fn render_row(
        &mut self,
        ix: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(row) = self.rows.get(ix).cloned() else {
            return div().into_any_element();
        };
        let theme = Theme::of(cx).clone();
        let now = Instant::now();
        let inner: AnyElement = match row.kind {
            RowKind::User {
                text,
                attachments,
                pending,
                sender_label,
                from,
            } => {
                let mut bubble = div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .max_w(relative_width(0.70))
                    .px(px(14.0))
                    .py(px(9.0))
                    .rounded(px(BUBBLE_RADIUS))
                    .bg(theme.bubble)
                    .text_color(theme.text)
                    .text_size(px(13.5))
                    .line_height(px(20.0));
                if !attachments.is_empty() {
                    let mut chips = div().flex().flex_wrap().gap(px(6.0));
                    for a in attachments {
                        // A voice memo chip (F11) is marked with a note glyph.
                        let label = if a.is_audio() {
                            format!("♪ {}", a.name)
                        } else {
                            a.name
                        };
                        chips = chips.child(
                            div()
                                .px(px(8.0))
                                .py(px(2.0))
                                .rounded(px(6.0))
                                .bg(theme.surface_2)
                                .text_size(px(11.0))
                                .text_color(theme.text_muted)
                                .child(label),
                        );
                    }
                    bubble = bubble.child(chips);
                }
                bubble = bubble.child(text);
                if let Some(person) = from {
                    // Someone else: left-aligned, their avatar and handle above.
                    let image = self.state.read(cx).avatar(&person.avatar_url);
                    let color = groups::sender_color(&person.handle, &theme);
                    return div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .items_start()
                        .gap(px(3.0))
                        .py(px(6.0))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .pl(px(2.0))
                                .child(people::avatar(image, &person, 18.0, &theme))
                                .child(
                                    div()
                                        .text_size(px(10.5))
                                        .text_color(theme.text_muted)
                                        .child(format!("@{}", person.handle)),
                                ),
                        )
                        .child(
                            bubble
                                .bg(Theme::with_alpha(color, 0.14))
                                .when(pending, |el| el.opacity(0.6)),
                        )
                        .into_any_element();
                }
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .items_end()
                    .gap(px(3.0))
                    .py(px(6.0))
                    .when_some(sender_label, |el, label| {
                        el.child(
                            div()
                                .pr(px(6.0))
                                .text_size(px(10.5))
                                .text_color(theme.text_faint)
                                .child(label),
                        )
                    })
                    .child(bubble.when(pending, |el| el.opacity(0.6)))
                    .into_any_element()
            }
            RowKind::Markdown { block, live } => {
                let veil = if live {
                    self.veils.get(&row.id).cloned()
                } else {
                    None
                };
                let fading = veil.as_ref().map(|v| v.is_active(now)).unwrap_or(false);
                if live || fading {
                    PulseClock::request(cx);
                }
                div()
                    .w_full()
                    .flex()
                    .justify_start()
                    .py(px(4.0))
                    .child(render_block(&row.id, &block, veil.as_ref(), now, &theme))
                    .into_any_element()
            }
            RowKind::Attachment { attachment } => {
                let thumbnail = attachment
                    .blob_ref
                    .as_ref()
                    .and_then(|k| self.state.read(cx).thumbnails.get(k).cloned());
                div()
                    .w_full()
                    .py(px(4.0))
                    .flex()
                    .child(attachments::card(
                        &row.id,
                        &attachment,
                        thumbnail,
                        self.state.clone(),
                        &theme,
                    ))
                    .into_any_element()
            }
            RowKind::AskUser { text } => {
                cards::status_row(StatusKind::AskUser, &text, false, &theme, cx).into_any_element()
            }
            RowKind::Question { question_id } => {
                let Some(chat_id) = self.chat_id.clone() else {
                    return div().into_any_element();
                };
                let widget = self
                    .state
                    .read(cx)
                    .messages_of(&chat_id)
                    .iter()
                    .find_map(|m| {
                        m.parts.iter().find_map(|p| match p {
                            Part::Question {
                                question_id: qid,
                                widget: Some(w),
                                ..
                            } if *qid == question_id => Some(w.clone()),
                            _ => None,
                        })
                    });
                let Some(widget) = widget else {
                    return div().into_any_element();
                };
                let state = self.state.clone();
                let card = self
                    .cards
                    .entry(question_id.clone())
                    .or_insert_with(|| {
                        cx.new(|cx| QuestionCard::new(state, chat_id, question_id, &widget, cx))
                    })
                    .clone();
                div()
                    .w_full()
                    .flex()
                    .justify_start()
                    .child(card)
                    .into_any_element()
            }
            RowKind::Approval { view } => {
                let chat_id = self.chat_id.clone().unwrap_or_default();
                let approval_id = view.approval_id.clone();
                let (state_a, chat_a, id_a) =
                    (self.state.clone(), chat_id.clone(), approval_id.clone());
                let (state_r, chat_r, id_r) = (self.state.clone(), chat_id, approval_id);
                approval_card(
                    &row.id,
                    &view,
                    &theme,
                    move |cx| {
                        state_a.update(cx, |s, cx| s.approve(chat_a.clone(), id_a.clone(), cx));
                    },
                    move |cx| {
                        state_r
                            .update(cx, |s, cx| s.reject(chat_r.clone(), id_r.clone(), None, cx));
                    },
                )
                .into_any_element()
            }
            RowKind::Error { code, message } => {
                let state = self.state.clone();
                let chat_id = self.chat_id.clone().unwrap_or_default();
                let message_id = row.message_id.clone();
                cards::error_chip(&row.id, &code, &message, &theme, move |cx| {
                    state.update(cx, |s, cx| s.retry(chat_id.clone(), message_id.clone(), cx));
                })
                .into_any_element()
            }
            RowKind::Glyph { glyph } => div()
                .w_full()
                .py(px(2.0))
                .text_size(px(10.0))
                .text_color(theme.text_faint)
                .child(glyph)
                .into_any_element(),
            RowKind::Indicator { queued } => {
                PulseClock::request(cx);
                cards::indicator(queued, &theme, cx).into_any_element()
            }
            RowKind::Sender {
                name,
                icon,
                seed,
                note,
            } => {
                let state = self.state.read(cx);
                // A shared agent's reply may carry only its handle: name, glyph
                // and colour come from the chat's member list or my projects.
                let agent = seed.strip_prefix("agent:").and_then(|pid| {
                    state
                        .selected()
                        .and_then(|space| space.agents.iter().find(|a| a.project_id == pid))
                        .map(|a| (a.name.clone(), a.icon.clone(), a.color.clone()))
                        .or_else(|| {
                            state
                                .projects
                                .iter()
                                .find(|p| p.id == pid)
                                .map(|p| (p.name.clone(), p.icon.clone(), p.color.clone()))
                        })
                });
                let (name, icon) = match &agent {
                    Some((n, i, _)) if name == "Agent" || icon.is_none() => {
                        (n.clone(), Some(i.clone()))
                    }
                    _ => (name, icon),
                };
                let color = agent
                    .as_ref()
                    .map(|(_, _, c)| groups::project_color(c, theme.accent))
                    .or_else(|| {
                        state
                            .projects
                            .iter()
                            .find(|p| format!("project:{}", p.id) == seed)
                            .map(|p| groups::project_color(&p.color, theme.accent))
                    })
                    .unwrap_or_else(|| groups::sender_color(&seed, &theme));
                // The sender's tree avatar: a shared agent's own slug, else my project's.
                let avatar_slug = seed
                    .strip_prefix("agent:")
                    .or_else(|| seed.strip_prefix("project:"))
                    .and_then(|pid| {
                        state
                            .selected()
                            .and_then(|space| space.agents.iter().find(|a| a.project_id == pid))
                            .and_then(|a| a.avatar.clone())
                            .or_else(|| {
                                state
                                    .projects
                                    .iter()
                                    .find(|p| p.id == pid)
                                    .and_then(|p| state.avatar_slug(p))
                            })
                    });
                let avatar_state = seed
                    .strip_prefix("agent:")
                    .or_else(|| seed.strip_prefix("project:"))
                    .map(|pid| state.avatar_state(pid))
                    .unwrap_or(crate::ui::avatars::AvatarState::Idle);
                let chip = groups::sender_chip_with(
                    &name,
                    icon.as_deref(),
                    color,
                    avatar_slug.as_deref(),
                    avatar_state,
                    &theme,
                );
                match note {
                    Some(note) => chip
                        .child(
                            div()
                                .text_size(px(10.5))
                                .text_color(theme.text_faint)
                                .child(format!("· {note}")),
                        )
                        .into_any_element(),
                    None => chip.into_any_element(),
                }
            }
            RowKind::Details => {
                let state = self.state.clone();
                let chat_id = self.chat_id.clone().unwrap_or_default();
                let message_id = row.message_id.clone();
                div()
                    .id(SharedString::from(row.id.clone()))
                    .w_full()
                    .pb(px(4.0))
                    .text_size(px(11.0))
                    .text_color(theme.text_faint)
                    .cursor_pointer()
                    .hover(|s| s.text_color(theme.text_muted))
                    .child("Show details ▸")
                    .on_click(move |_, _, cx| {
                        state.update(cx, |s, cx| {
                            s.open_details(
                                DetailsTarget::Message {
                                    chat_id: chat_id.clone(),
                                    message_id: message_id.clone(),
                                },
                                cx,
                            )
                        });
                    })
                    .into_any_element()
            }
            RowKind::Speaker { cached } => {
                let state = self.state.clone();
                let message_id = row.message_id.clone();
                div()
                    .id(SharedString::from(row.id.clone()))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .w(px(96.0))
                    .pb(px(6.0))
                    .text_size(px(11.0))
                    .text_color(theme.text_faint)
                    .cursor_pointer()
                    .hover(|s| s.text_color(theme.text_muted))
                    .child(div().text_size(px(12.0)).child("▶"))
                    .child(if cached { "Replay" } else { "Speak" })
                    .on_click(move |_, _, cx| {
                        state.update(cx, |s, cx| s.replay_speech(message_id.clone(), cx));
                    })
                    .into_any_element()
            }
        };
        div()
            .w_full()
            .flex()
            .justify_center()
            .px(px(28.0))
            .child(div().w_full().max_w(px(COLUMN_MAX_WIDTH)).child(inner))
            .into_any_element()
    }
}

fn relative_width(fraction: f32) -> gpui::DefiniteLength {
    gpui::relative(fraction)
}

impl Render for Transcript {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.step_spring(window, cx);
        div()
            .size_full()
            .on_scroll_wheel(cx.listener(|this, event, _, cx| this.on_wheel(event, cx)))
            .child(edge_faded(
                24.0,
                true,
                false,
                list(self.list.clone(), cx.processor(Self::render_row)).size_full(),
            ))
    }
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

fn hash_version<T: Hash>(value: &T, live: bool) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut h);
    (h.finish() << 1) | (live as u64)
}

fn block_text_len(block: &Block) -> usize {
    match block {
        Block::Paragraph(t) | Block::Heading { text: t, .. } => t.len(),
        Block::Bullets(items) | Block::Numbered(items) => items.iter().map(|s| s.len() + 1).sum(),
        Block::Code { code, .. } => code.len(),
        Block::Table { header, rows } => {
            header.iter().map(|s| s.len() + 1).sum::<usize>()
                + rows.iter().flatten().map(|s| s.len() + 1).sum::<usize>()
        }
    }
}

/// "Routine · Morning stand-up" for a prompt a routine injected; `None` for the
/// user's own messages (`sender` empty or `user`).
fn sender_caption(message: &Message) -> Option<String> {
    let sender = message.sender.as_str();
    if sender.is_empty() || sender == "user" || sender.starts_with("user:") {
        return None;
    }
    if let Some(id) = sender.strip_prefix("routine:") {
        let name = message.routine_name.as_deref().unwrap_or(id);
        return Some(format!("Routine · {name}"));
    }
    Some(sender.replace(':', " · "))
}

/// A text block that is really machine output (a JSON object/array) is not
/// prose; it belongs in the Details drawer, never in the transcript.
fn is_json_block(block: &Block) -> bool {
    let text = match block {
        Block::Paragraph(t) => t.as_str(),
        Block::Code { lang, code } if lang.is_empty() || lang.eq_ignore_ascii_case("json") => {
            code.as_str()
        }
        Block::Code { .. }
        | Block::Heading { .. }
        | Block::Bullets(_)
        | Block::Numbered(_)
        | Block::Table { .. } => return false,
    };
    let t = text.trim();
    (t.starts_with('{') || t.starts_with('['))
        && serde_json::from_str::<serde_json::Value>(t).is_ok()
}

/// The transcript shows only what the agent said and made: prose, files,
/// questions, errors. Everything the agent did (tool calls, status events,
/// JSON, usage) is hidden; the Details drawer has it. One indicator per chat
/// pulses under the last message while anything runs.
pub fn build_rows(
    messages: &[Message],
    show_details: bool,
    speech: SpeechRows<'_>,
    me_id: &str,
) -> Vec<Row> {
    let mine = |m: &Message| {
        m.sender == "user" || m.sender.is_empty() || m.sender == format!("user:{me_id}")
    };
    let mut rows = Vec::new();
    let mut running = false;
    let mut queued = false;
    let last_reply = messages
        .iter()
        .rposition(|m| m.role == Role::Assistant && !m.is_streaming());
    for (mix, message) in messages.iter().enumerate() {
        let streaming = message.is_streaming();
        match message.role {
            Role::User => {
                let text = message.plain_text();
                let attachments: Vec<Attachment> = message
                    .parts
                    .iter()
                    .filter_map(|p| match p {
                        Part::Attachment { attachment, .. } => Some(attachment.clone()),
                        _ => None,
                    })
                    .collect();
                let id = format!("{}#user", message.id);
                let from = if message.sender.starts_with("user:") && !mine(message) {
                    Some(Person {
                        id: message.sender.trim_start_matches("user:").to_string(),
                        handle: message
                            .sender_handle
                            .clone()
                            .unwrap_or_else(|| "someone".into()),
                        name: message.sender_name.clone(),
                        avatar_url: message.sender_avatar.clone().unwrap_or_default(),
                        status: "member".into(),
                        role: String::new(),
                    })
                } else {
                    None
                };
                let sender_label = if from.is_some() {
                    None
                } else {
                    sender_caption(message)
                };
                rows.push(Row {
                    version: hash_version(
                        &(
                            &text,
                            message.pending,
                            attachments.len(),
                            &sender_label,
                            from.as_ref().map(|p| &p.handle),
                        ),
                        false,
                    ),
                    id,
                    message_id: message.id.clone(),
                    kind: RowKind::User {
                        text,
                        attachments,
                        pending: message.pending,
                        sender_label,
                        from,
                    },
                });
                if message.status == MessageStatus::Queued {
                    queued = true;
                }
            }
            Role::Assistant | Role::System | Role::Project => {
                if streaming {
                    running = true;
                }
                if message.role == Role::Project {
                    let name = message
                        .sender_name
                        .clone()
                        .or_else(|| message.sender.strip_prefix("project:").map(str::to_owned))
                        .unwrap_or_else(|| "Project".to_owned());
                    rows.push(Row {
                        version: hash_version(&(&name, &message.sender_icon), false),
                        id: format!("{}#sender", message.id),
                        message_id: message.id.clone(),
                        kind: RowKind::Sender {
                            name,
                            icon: message.sender_icon.clone(),
                            seed: message.sender.clone(),
                            note: None,
                        },
                    });
                } else if message.sender.starts_with("agent:") && message.asked_by_handle.is_some()
                {
                    // A shared agent in a people chat: name it and whom it answers.
                    let name = message
                        .sender_name
                        .clone()
                        .unwrap_or_else(|| "Agent".to_owned());
                    let mut note = format!(
                        "answering @{}",
                        message.asked_by_handle.clone().unwrap_or_default()
                    );
                    if let Some(owner) = &message.owner_handle {
                        note.push_str(&format!(" · owned by @{owner}"));
                    }
                    rows.push(Row {
                        version: hash_version(&(&name, &message.sender_icon, &note), false),
                        id: format!("{}#sender", message.id),
                        message_id: message.id.clone(),
                        kind: RowKind::Sender {
                            name,
                            icon: message.sender_icon.clone(),
                            seed: message.sender.clone(),
                            note: Some(note),
                        },
                    });
                }
                let last_text_part = message
                    .parts
                    .iter()
                    .rposition(|p| matches!(p, Part::Text { .. }));
                let mut had_prose = false;
                for (pix, part) in message.parts.iter().enumerate() {
                    match part {
                        Part::Text { id, text } => {
                            let blocks: Vec<Block> = split_blocks(text)
                                .into_iter()
                                .filter(|b| !is_json_block(b))
                                .collect();
                            let is_last_text = Some(pix) == last_text_part;
                            let n = blocks.len();
                            for (bix, block) in blocks.into_iter().enumerate() {
                                had_prose = true;
                                let live = streaming && is_last_text && bix + 1 == n;
                                rows.push(Row {
                                    version: hash_version(&block, live),
                                    id: format!("{}#{}.{}", message.id, id, bix),
                                    message_id: message.id.clone(),
                                    kind: RowKind::Markdown { block, live },
                                });
                            }
                        }
                        Part::Attachment { id, attachment } => rows.push(Row {
                            version: hash_version(
                                &(&attachment.name, &attachment.title, attachment.bytes),
                                false,
                            ),
                            id: format!("{}#{}", message.id, id),
                            message_id: message.id.clone(),
                            kind: RowKind::Attachment {
                                attachment: attachment.clone(),
                            },
                        }),
                        // Legacy `waiting_user` row: only when no `question` part carries it.
                        Part::Status {
                            id,
                            code: StatusKind::AskUser,
                            message: text,
                        } if !message.has_question_part() => rows.push(Row {
                            version: hash_version(text, false),
                            id: format!("{}#{}", message.id, id),
                            message_id: message.id.clone(),
                            kind: RowKind::AskUser { text: text.clone() },
                        }),
                        Part::Question {
                            id,
                            question_id,
                            question,
                            widget,
                            answer,
                            state,
                            ..
                        } => match widget {
                            Some(_) => rows.push(Row {
                                version: hash_version(
                                    &(question, answer.as_ref().map(Value::to_string), *state),
                                    false,
                                ),
                                id: format!("{}#{}", message.id, id),
                                message_id: message.id.clone(),
                                kind: RowKind::Question {
                                    question_id: question_id.clone(),
                                },
                            }),
                            None => rows.push(Row {
                                version: hash_version(&(question, *state), false),
                                id: format!("{}#{}", message.id, id),
                                message_id: message.id.clone(),
                                kind: RowKind::AskUser {
                                    text: question.clone(),
                                },
                            }),
                        },
                        // S18: the card renders; its `waiting_approval` status row is implied.
                        Part::Approval {
                            id,
                            approval_id,
                            title,
                            summary,
                            risk,
                            payload,
                            state,
                            decided_by,
                            reason,
                            ..
                        } => {
                            let view = ApprovalView {
                                approval_id: approval_id.clone(),
                                title: title.clone(),
                                summary: summary.clone(),
                                risk: *risk,
                                state: *state,
                                payload: payload.clone(),
                                decided_by: decided_by.clone(),
                                reason: reason.clone(),
                            };
                            rows.push(Row {
                                version: hash_version(
                                    &(approval_id, title, state.label(), decided_by, reason),
                                    false,
                                ),
                                id: format!("{}#{}", message.id, id),
                                message_id: message.id.clone(),
                                kind: RowKind::Approval { view },
                            });
                        }
                        Part::Status {
                            code: StatusKind::Queued,
                            ..
                        } => {
                            if streaming {
                                queued = true;
                            }
                        }
                        Part::Error {
                            id,
                            code,
                            message: text,
                        } => rows.push(Row {
                            version: hash_version(&(code, text), false),
                            id: format!("{}#{}", message.id, id),
                            message_id: message.id.clone(),
                            kind: RowKind::Error {
                                code: code.clone(),
                                message: text.clone(),
                            },
                        }),
                        // Everything the agent did, not said: Details drawer only.
                        Part::Status { .. }
                        | Part::Tool { .. }
                        | Part::SubagentResult { .. }
                        | Part::Reasoning { .. }
                        | Part::Diff { .. }
                        | Part::File { .. }
                        | Part::Browser { .. }
                        | Part::Skills { .. }
                        | Part::Unknown => {}
                    }
                }
                // Stopped / failed turns leave one tiny glyph, no words.
                let glyph = match message.status {
                    MessageStatus::Cancelled => Some("■"),
                    MessageStatus::Error
                        if !message
                            .parts
                            .iter()
                            .any(|p| matches!(p, Part::Error { .. })) =>
                    {
                        Some("✗")
                    }
                    MessageStatus::Error
                    | MessageStatus::Done
                    | MessageStatus::Queued
                    | MessageStatus::Streaming => None,
                };
                if let Some(glyph) = glyph {
                    rows.push(Row {
                        version: 1,
                        id: format!("{}#glyph", message.id),
                        message_id: message.id.clone(),
                        kind: RowKind::Glyph { glyph },
                    });
                }
                // Speaker (F11): every reply whose speech is cached, plus the
                // newest finished reply while voice mode is on.
                let cached = speech.spoken.contains(&message.id);
                if !streaming
                    && message.role == Role::Assistant
                    && had_prose
                    && (cached || (speech.voice_mode && last_reply == Some(mix)))
                {
                    rows.push(Row {
                        version: hash_version(&cached, false),
                        id: format!("{}#speaker", message.id),
                        message_id: message.id.clone(),
                        kind: RowKind::Speaker { cached },
                    });
                }
                if show_details && !streaming && message.role == Role::Assistant && had_prose {
                    rows.push(Row {
                        version: 1,
                        id: format!("{}#details", message.id),
                        message_id: message.id.clone(),
                        kind: RowKind::Details,
                    });
                }
            }
        }
    }
    if running || queued {
        let last = messages.last().map(|m| m.id.clone()).unwrap_or_default();
        rows.push(Row {
            version: hash_version(&queued, true),
            id: "chat#indicator".into(),
            message_id: last,
            kind: RowKind::Indicator { queued },
        });
    }
    rows
}

// ---------------------------------------------------------------------------
// Markdown rendering with the veil
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn styled_text(
    id: &str,
    text: &str,
    base_color: gpui::Hsla,
    theme: &Theme,
    veil: Option<&Veil>,
    now: Instant,
    offset: usize,
) -> AnyElement {
    let spans = inline_spans(text);
    let mut full = String::new();
    let mut runs: Vec<TextRun> = Vec::new();
    let mut links: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    let sans = gpui::Font {
        family: theme.font_sans.into(),
        features: Default::default(),
        fallbacks: None,
        weight: FontWeight::NORMAL,
        style: gpui::FontStyle::Normal,
    };
    let mono = gpui::Font {
        family: theme.font_mono.into(),
        ..sans.clone()
    };
    for span in spans {
        let style = span.style;
        let mut font = if style.code {
            mono.clone()
        } else {
            sans.clone()
        };
        if style.bold {
            font.weight = FontWeight::SEMIBOLD;
        }
        if style.italic {
            font.style = gpui::FontStyle::Italic;
        }
        let start = full.len();
        if let Some(href) = span.href.clone() {
            links.push((start..start + span.text.len(), href));
        }
        runs.push(TextRun {
            len: span.text.len(),
            font,
            color: if style.link { theme.accent } else { base_color },
            background_color: if style.code {
                Some(theme.code_bg)
            } else {
                None
            },
            underline: if style.link {
                Some(gpui::UnderlineStyle {
                    color: Some(Theme::with_alpha(theme.accent, 0.6)),
                    thickness: px(1.0),
                    wavy: false,
                })
            } else {
                None
            },
            strikethrough: None,
        });
        full.push_str(&span.text);
    }
    let runs = match veil {
        Some(veil) => apply_veil(runs, &veil.spans(now), offset),
        None => runs,
    };
    let styled = StyledText::new(SharedString::from(full)).with_runs(runs);
    if links.is_empty() {
        return styled.into_any_element();
    }
    let ranges: Vec<std::ops::Range<usize>> = links.iter().map(|(r, _)| r.clone()).collect();
    let hrefs: Vec<String> = links.into_iter().map(|(_, h)| h).collect();
    gpui::InteractiveText::new(SharedString::from(id.to_string()), styled)
        .on_click(ranges, move |ix, _, cx| {
            if let Some(href) = hrefs.get(ix) {
                cx.open_url(href);
            }
        })
        .into_any_element()
}

/// Split runs at chunk boundaries and multiply alpha in (zeron veil.rs).
fn apply_veil(
    runs: Vec<TextRun>,
    spans: &[(std::ops::Range<usize>, f32)],
    offset: usize,
) -> Vec<TextRun> {
    if spans.is_empty() {
        return runs;
    }
    let mut out = Vec::new();
    let mut pos = 0usize;
    for run in runs {
        let run_range = pos..pos + run.len;
        let mut cursor = run_range.start;
        // Boundaries inside this run.
        let mut cuts: Vec<usize> = spans
            .iter()
            .flat_map(|(r, _)| [r.start.saturating_sub(offset), r.end.saturating_sub(offset)])
            .filter(|c| *c > run_range.start && *c < run_range.end)
            .collect();
        cuts.sort_unstable();
        cuts.dedup();
        cuts.push(run_range.end);
        for cut in cuts {
            if cut <= cursor {
                continue;
            }
            let mid = cursor;
            let alpha = spans
                .iter()
                .find(|(r, _)| {
                    let s = r.start.saturating_sub(offset);
                    let e = r.end.saturating_sub(offset);
                    mid >= s && mid < e
                })
                .map(|(_, a)| *a)
                .unwrap_or(1.0);
            let mut color = run.color;
            color.a *= alpha;
            out.push(TextRun {
                len: cut - cursor,
                color,
                ..run.clone()
            });
            cursor = cut;
        }
        pos = run_range.end;
    }
    out
}

fn render_block(
    row_id: &str,
    block: &Block,
    veil: Option<&Veil>,
    now: Instant,
    theme: &Theme,
) -> AnyElement {
    let base = div()
        .w_full()
        .min_w_0()
        .max_w(px(ASSISTANT_MAX_WIDTH))
        .text_color(theme.text)
        .text_size(px(13.5))
        .line_height(px(21.0));
    match block {
        Block::Paragraph(text) => base
            .child(styled_text(
                &format!("{row_id}.p"),
                text,
                theme.text,
                theme,
                veil,
                now,
                0,
            ))
            .into_any_element(),
        Block::Heading { level, text } => {
            let size = match level {
                1 => 20.0,
                2 => 17.0,
                _ => 15.0,
            };
            base.text_size(px(size))
                .font_weight(FontWeight::SEMIBOLD)
                .pt(px(6.0))
                .child(styled_text(
                    &format!("{row_id}.h"),
                    text,
                    theme.text,
                    theme,
                    veil,
                    now,
                    0,
                ))
                .into_any_element()
        }
        Block::Bullets(items) | Block::Numbered(items) => {
            let numbered = matches!(block, Block::Numbered(_));
            let mut col = base.flex().flex_col().gap(px(3.0));
            let mut offset = 0usize;
            // Two-digit markers ("10.") need a wider gutter than "•".
            let marker_w = if numbered && items.len() >= 10 {
                24.0
            } else {
                16.0
            };
            for (i, item) in items.iter().enumerate() {
                let marker = if numbered {
                    format!("{}.", i + 1)
                } else {
                    "•".to_string()
                };
                col = col.child(
                    div()
                        .flex()
                        .gap(px(8.0))
                        .child(
                            div()
                                .w(px(marker_w))
                                .flex_shrink_0()
                                .text_color(theme.text_muted)
                                .child(marker),
                        )
                        .child(div().flex_1().min_w_0().child(styled_text(
                            &format!("{row_id}.li{i}"),
                            item,
                            theme.text,
                            theme,
                            veil,
                            now,
                            offset,
                        ))),
                );
                offset += item.len() + 1;
            }
            col.into_any_element()
        }
        Block::Table { header, rows } => {
            let cols = header.len().max(1);
            let cell = |text: &str, head: bool, i: usize| {
                div()
                    .flex_1()
                    .min_w_0()
                    .px(px(10.0))
                    .py(px(6.0))
                    .when(i > 0, |el| el.border_l_1().border_color(theme.border))
                    .text_size(px(12.5))
                    .when(head, |el| el.font_weight(FontWeight::SEMIBOLD))
                    .text_color(if head { theme.text } else { theme.text_muted })
                    .child(text.to_string())
            };
            let mut table = div()
                .flex()
                .flex_col()
                .w_full()
                .rounded(px(8.0))
                .border_1()
                .border_color(theme.border)
                .overflow_hidden();
            let mut head_row = div().flex().bg(theme.code_bg);
            for (i, h) in header.iter().enumerate() {
                head_row = head_row.child(cell(h, true, i));
            }
            table = table.child(head_row);
            for (ri, row) in rows.iter().enumerate() {
                let mut r = div()
                    .flex()
                    .border_t_1()
                    .border_color(theme.border)
                    .when(ri % 2 == 1, |el| {
                        el.bg(Theme::with_alpha(theme.code_bg, 0.4))
                    });
                for i in 0..cols {
                    r = r.child(cell(row.get(i).map(String::as_str).unwrap_or(""), false, i));
                }
                table = table.child(r);
            }
            table.into_any_element()
        }
        Block::Code { lang, code } => {
            let mut card = div()
                .flex()
                .flex_col()
                .w_full()
                .rounded(px(8.0))
                .bg(theme.code_bg)
                .border_1()
                .border_color(theme.border);
            card = card.child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .px(px(10.0))
                    .py(px(4.0))
                    .border_b_1()
                    .border_color(theme.border)
                    .text_size(px(11.0))
                    .text_color(theme.text_faint)
                    .child(if lang.is_empty() {
                        "code".to_string()
                    } else {
                        lang.clone()
                    })
                    .child(div().child("Copy")),
            );
            let mono = gpui::Font {
                family: theme.font_mono.into(),
                features: Default::default(),
                fallbacks: None,
                weight: FontWeight::NORMAL,
                style: gpui::FontStyle::Normal,
            };
            let runs = vec![TextRun {
                len: code.len(),
                font: mono,
                color: theme.text,
                background_color: None,
                underline: None,
                strikethrough: None,
            }];
            let runs = match veil {
                Some(v) => apply_veil(runs, &v.spans(now), 0),
                None => runs,
            };
            card = card.child(
                div()
                    .px(px(10.0))
                    .py(px(8.0))
                    .text_size(px(12.5))
                    .line_height(px(18.0))
                    .child(StyledText::new(SharedString::from(code.clone())).with_runs(runs)),
            );
            card.into_any_element()
        }
    }
}

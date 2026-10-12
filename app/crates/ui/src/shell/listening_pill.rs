//! The Listening pill above Main's composer. One row per live wait: a
//! worker still going, a timer, a pull request, a CI run. Hover or click
//! opens the rows; each has a Stop. Nothing listening, no pill.

use chrono::{DateTime, Local, Utc};
use gpui::{
    div, prelude::*, px, AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled,
};
use zeron_engine::arbos_layout::AgentView;
use zeron_engine::arbos_wakes::SubscriptionView;
use zeron_rpc::methods;

use crate::theme::Theme;

use super::project_chrome::Surface;
use super::Shell;

/// Wide enough for a repo, a branch, and the two times on one row.
const OPEN_WIDTH: f32 = 380.0;

/// What one row waits on, and what its Stop does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Waits {
    /// A `.arbos/subscriptions/<id>.toml`. Stop removes the file.
    Subscription { id: String },
    /// A worker whose turn has not ended. Stop interrupts its chat.
    Worker { chat_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ListeningRow {
    pub waits: Waits,
    /// What the row says it waits on.
    pub label: String,
    /// RFC 3339. Empty when unknown.
    pub since: String,
    /// RFC 3339. Empty when unknown.
    pub next_wake: String,
}

/// The engine's rows as the pill shows them: working workers first, then
/// each unexpired subscription in file order. `worker_since` gives the
/// start time of a worker's chat, when the chat is known.
pub(super) fn listening_rows(
    subscriptions: &[SubscriptionView],
    agents: &[AgentView],
    worker_since: &dyn Fn(&str) -> Option<DateTime<Utc>>,
) -> Vec<ListeningRow> {
    let mut rows: Vec<ListeningRow> = agents
        .iter()
        .filter(|agent| agent.role == "worker" && agent.status == "working")
        .map(|agent| ListeningRow {
            waits: Waits::Worker {
                chat_id: agent.chat_id.clone(),
            },
            label: format!("Worker {}", agent.name),
            since: worker_since(&agent.chat_id)
                .map(|time| time.to_rfc3339())
                .unwrap_or_default(),
            next_wake: String::new(),
        })
        .collect();
    rows.extend(
        subscriptions
            .iter()
            .filter(|sub| !sub.expired)
            .map(|sub| ListeningRow {
                waits: Waits::Subscription { id: sub.id.clone() },
                label: subscription_label(&sub.kind, &sub.label),
                since: sub.created.clone(),
                next_wake: sub.next_wake.clone(),
            }),
    );
    rows
}

fn subscription_label(kind: &str, label: &str) -> String {
    match kind {
        "timer" | "slack" => label.to_owned(),
        "github_pr" => format!("Pull request {label}"),
        "github_ci" => format!("CI on {label}"),
        other => format!("{other} {label}"),
    }
}

/// The pill shows on Main's chat while at least one row is live.
pub(super) fn pill_shown(on_main_chat: bool, rows: &[ListeningRow]) -> bool {
    on_main_chat && !rows.is_empty()
}

pub(super) fn pill_text(count: usize) -> String {
    format!("Listening · {count}")
}

/// The second line of a row: when the wait began and, where known, the
/// next wake. Empty when neither time is known.
pub(super) fn row_detail(row: &ListeningRow, now: DateTime<Local>) -> String {
    let mut parts = Vec::new();
    if let Some(since) = clock_text(&row.since, now) {
        parts.push(format!("since {since}"));
    }
    if let Some(next) = clock_text(&row.next_wake, now) {
        parts.push(format!("next {next}"));
    }
    parts.join(" · ")
}

/// A local clock time: `08:30` on the same day as `now`, else `Oct 13 08:30`.
pub(super) fn clock_text(rfc3339: &str, now: DateTime<Local>) -> Option<String> {
    let time = DateTime::parse_from_rfc3339(rfc3339.trim())
        .ok()?
        .with_timezone(&Local);
    if time.date_naive() == now.date_naive() {
        Some(time.format("%H:%M").to_string())
    } else {
        Some(time.format("%b %-d %H:%M").to_string())
    }
}

impl Shell {
    pub(super) fn render_listening_pill(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let on_main = self.project.surface == Surface::Chat && self.selected_is_main(cx);
        if !pill_shown(on_main, &self.project.listening) {
            return div().into_any_element();
        }
        let theme = Theme::of(cx).clone();
        let rows = self.project.listening.clone();
        let open = self.project.listening_open || self.project.listening_hover;
        let now = Local::now();
        let mut card = div()
            .id("listening-pill")
            .rounded(px(10.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface)
            .flex()
            .flex_col()
            .when(open, |card| card.min_w(px(OPEN_WIDTH)))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                this.project.listening_hover = *hovered;
                cx.notify();
            }))
            .child(
                div()
                    .id("listening-pill-toggle")
                    .px(px(10.0))
                    .py(px(4.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .cursor_pointer()
                    .child(div().size(px(6.0)).rounded_full().bg(theme.accent))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(theme.text)
                            .child(pill_text(rows.len())),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.project.listening_open = !this.project.listening_open;
                        cx.notify();
                    })),
            );
        if open {
            let mut list = div()
                .px(px(10.0))
                .pb(px(8.0))
                .flex()
                .flex_col()
                .gap(px(4.0));
            for (index, row) in rows.into_iter().enumerate() {
                list = list.child(self.listening_row(index, row, now, &theme, cx));
            }
            card = card.child(list);
        }
        div()
            .w_full()
            .max_w(px(720.0))
            .mx_auto()
            .mb(px(8.0))
            .flex()
            .flex_col()
            .items_start()
            .child(card)
            .into_any_element()
    }

    fn listening_row(
        &self,
        index: usize,
        row: ListeningRow,
        now: DateTime<Local>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let detail = row_detail(&row, now);
        let waits = row.waits.clone();
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(theme.text)
                            .child(row.label),
                    )
                    .when(!detail.is_empty(), |column| {
                        column.child(
                            div()
                                .text_size(px(11.0))
                                .text_color(theme.text_muted)
                                .child(detail),
                        )
                    }),
            )
            .child(
                div()
                    .id(SharedString::from(format!("listening-stop-{index}")))
                    .px(px(8.0))
                    .py(px(3.0))
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .bg(theme.element_hover)
                    .text_size(px(11.0))
                    .text_color(theme.text)
                    .child("Stop")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.stop_listening(waits.clone(), cx);
                    })),
            )
            .into_any_element()
    }

    /// Stop one row through the engine: a worker's turn is interrupted; a
    /// subscription file is removed with `subscriptions_remove`. The row
    /// leaves the pill at once; the watch reconciles after.
    fn stop_listening(&mut self, waits: Waits, cx: &mut Context<Self>) {
        self.project.listening.retain(|row| row.waits != waits);
        cx.notify();
        match waits {
            Waits::Worker { chat_id } => {
                self.composer
                    .update(cx, |composer, cx| composer.interrupt_chat(chat_id, cx));
            }
            Waits::Subscription { id } => {
                let Some(project) = self.selected_project(cx) else {
                    return;
                };
                let Some(engine) = self.state.read(cx).engine().cloned() else {
                    self.show_notice(crate::toast::ToastKind::Error, "Engine not connected", cx);
                    return;
                };
                let params = serde_json::json!({
                    "op": "subscriptions_remove",
                    "project": project.path,
                    "id": id,
                });
                self.project.listening_task = Some(cx.spawn(async move |this, cx| {
                    if let Err(err) = engine.client().call(methods::ARBOS_CALL, params).await {
                        this.update(cx, |shell, cx| {
                            shell.show_notice(
                                crate::toast::ToastKind::Error,
                                format!("Stop failed: {err}"),
                                cx,
                            );
                            cx.notify();
                        })
                        .ok();
                    }
                }));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn sub(id: &str, kind: &str, label: &str, expired: bool) -> SubscriptionView {
        SubscriptionView {
            id: id.into(),
            kind: kind.into(),
            label: label.into(),
            created: "2026-10-12T06:00:00Z".into(),
            expires: String::new(),
            expired,
            last_fired: String::new(),
            next_wake: if kind == "timer" {
                "2026-10-12T08:30:00Z".into()
            } else {
                String::new()
            },
        }
    }

    fn agent(id: &str, role: &str, status: &str) -> AgentView {
        AgentView {
            id: id.into(),
            name: id.into(),
            role: role.into(),
            chat_id: format!("space-w-{id}"),
            status: status.into(),
            brief: String::new(),
            result: String::new(),
            machine: "local".into(),
            model: String::new(),
        }
    }

    fn no_chat(_: &str) -> Option<DateTime<Utc>> {
        None
    }

    #[test]
    fn working_workers_come_first_then_live_subscriptions() {
        let subs = vec![
            sub("ci-main", "github_ci", "unarbos/arbos main", false),
            sub("morning", "timer", "Daily at 08:30", false),
            sub("old", "github_pr", "unarbos/arbos#1", true),
            sub("pr-12", "github_pr", "unarbos/arbos#12", false),
            sub("room", "slack", "Slack #arbos", false),
        ];
        let agents = vec![
            agent("main", "coordinator", "working"),
            agent("fix-tests", "worker", "working"),
            agent("write-docs", "worker", "done"),
        ];
        let started = Utc.with_ymd_and_hms(2026, 10, 12, 7, 15, 0).unwrap();
        let since = |chat_id: &str| (chat_id == "space-w-fix-tests").then_some(started);
        let rows = listening_rows(&subs, &agents, &since);
        let labels: Vec<&str> = rows.iter().map(|row| row.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "Worker fix-tests",
                "CI on unarbos/arbos main",
                "Daily at 08:30",
                "Pull request unarbos/arbos#12",
                "Slack #arbos",
            ]
        );
        assert_eq!(
            rows[0].waits,
            Waits::Worker {
                chat_id: "space-w-fix-tests".into()
            }
        );
        assert_eq!(rows[0].since, "2026-10-12T07:15:00+00:00");
        assert_eq!(
            rows[1].waits,
            Waits::Subscription {
                id: "ci-main".into()
            }
        );
        assert_eq!(rows[1].since, "2026-10-12T06:00:00Z");
        assert_eq!(rows[1].next_wake, "", "CI is polled; no time is known");
        assert_eq!(rows[2].next_wake, "2026-10-12T08:30:00Z");
    }

    #[test]
    fn the_pill_hides_when_nothing_is_live_or_off_main() {
        assert!(!pill_shown(true, &[]));
        let only_expired = listening_rows(
            &[sub("old", "timer", "Daily at 08:30", true)],
            &[agent("done", "worker", "done")],
            &no_chat,
        );
        assert!(only_expired.is_empty());
        assert!(!pill_shown(true, &only_expired));
        let live = listening_rows(
            &[sub("tick", "timer", "Every 600 seconds", false)],
            &[],
            &no_chat,
        );
        assert!(pill_shown(true, &live));
        assert!(!pill_shown(false, &live), "a worker tab shows no pill");
        assert_eq!(pill_text(live.len()), "Listening · 1");
        assert_eq!(pill_text(12), "Listening · 12");
    }

    #[test]
    fn row_detail_names_the_start_and_the_next_wake() {
        let now = Local.with_ymd_and_hms(2026, 10, 12, 9, 0, 0).unwrap();
        let today = Local.with_ymd_and_hms(2026, 10, 12, 6, 5, 0).unwrap();
        let tomorrow = Local.with_ymd_and_hms(2026, 10, 13, 8, 30, 0).unwrap();
        let row = ListeningRow {
            waits: Waits::Subscription { id: "x".into() },
            label: "Daily at 08:30".into(),
            since: today.to_rfc3339(),
            next_wake: tomorrow.to_rfc3339(),
        };
        assert_eq!(row_detail(&row, now), "since 06:05 · next Oct 13 08:30");
        let unknown = ListeningRow {
            since: String::new(),
            next_wake: String::new(),
            ..row.clone()
        };
        assert_eq!(row_detail(&unknown, now), "");
        let no_next = ListeningRow {
            next_wake: "not a time".into(),
            ..row
        };
        assert_eq!(row_detail(&no_next, now), "since 06:05");
    }
}

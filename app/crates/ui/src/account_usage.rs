//! The composer footer's ring cluster. The plan-usage ring shows how much of
//! the active account's rate limit the session's harness has used, beside
//! the context ring. Clicking it opens the harness's accounts — each with its
//! usage meters — and clicking one switches to it, the same `ActivateAgentAccount` Settings →
//! Accounts runs. Both views share [`AccountsSnapshotCache`], so a switch in
//! either shows up in the other.
use std::path::Path;
use std::time::{Duration, Instant};

use gpui::{
    Context, Entity, IntoElement, Render, SharedString, Subscription, Task, Window, div,
    prelude::*, px,
};
use zeron_proto::{AgentAccount, AgentAccountsSnapshot, HarnessId};
use zeron_rpc::methods;

use crate::popover;
use crate::settings::accounts::{
    self, AccountsSnapshotCache, UsageLevel, render_usage_meter, reports_usage, signs_in,
    usage_color, usage_level,
};
use crate::state::AppState;
use crate::theme::Theme;

/// Forced probes hit the provider; the engine throttles them too, but the
/// ring re-probes on every hover, so don't even ask more often than this.
const FORCE_MIN_INTERVAL: Duration = Duration::from_secs(30);
/// Background re-probe while a composer is alive: usage moves as turns run.
const POLL_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// The bucket view re-reads the agent's log at most this often while it is
/// on screen; a turn takes longer than this to change it.
const BREAKDOWN_MIN_INTERVAL: Duration = Duration::from_secs(2);
/// How long the pointer rests on the context ring before the card opens;
/// a pass over the ring on the way to the composer opens nothing.
const HOVER_OPEN_DELAY: Duration = Duration::from_millis(200);
/// How long the pointer may be between the ring and its hover-opened card
/// before the card closes.
const HOVER_CLOSE_GRACE: Duration = Duration::from_millis(250);

/// The Arbos agent chat the footer belongs to: the project folder and the
/// chat id, both on this device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentChat {
    pub project: String,
    pub chat_id: String,
}

/// The binding limit: the most-used window of the account. Pure.
pub fn used_fraction(account: &AgentAccount) -> Option<f32> {
    account
        .usage_windows
        .iter()
        .map(|window| window.used_fraction.clamp(0.0, 1.0))
        .reduce(f32::max)
}

/// The harness's live account, when it reports usage. Pure.
pub fn active_account(
    snapshot: &AgentAccountsSnapshot,
    harness: HarnessId,
) -> Option<&AgentAccount> {
    snapshot
        .accounts
        .iter()
        .find(|account| account.harness == harness && account.active)
}

/// Loads (and switches) the accounts of the composer's target device.
pub struct AccountUsage {
    state: Entity<AppState>,
    /// `None` = this device, as in [`AccountsSnapshotCache`].
    target: Option<String>,
    harness: Option<HarnessId>,
    /// Once any list has been asked for the current target.
    loaded: bool,
    last_forced: Option<Instant>,
    error: Option<SharedString>,
    load_task: Option<Task<()>>,
    action_task: Option<Task<()>>,
    popup: popover::Popup<FooterCard>,
    /// The chat whose bucket view the context ring shows, when it is an
    /// agent of a local Arbos project.
    agent_chat: Option<AgentChat>,
    breakdown: Option<crate::context_usage::Breakdown>,
    breakdown_read_at: Option<Instant>,
    breakdown_task: Option<Task<()>>,
    /// The context card opened from a hover rather than a click. A click
    /// pins it; leaving ring and card closes it.
    hover_opened: bool,
    ring_hovered: bool,
    card_hovered: bool,
    hover_open: Option<Task<()>>,
    hover_close: Option<Task<()>>,
    _poll: Task<()>,
    _cache: Subscription,
    _state: Subscription,
}

impl AccountUsage {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                if this
                    .update(cx, |usage, cx| {
                        if usage.harness.is_some() {
                            usage.load(true, cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            _state: cx.observe(&state, |_, _, cx| cx.notify()),
            state,
            target: None,
            harness: None,
            loaded: false,
            last_forced: None,
            error: None,
            load_task: None,
            action_task: None,
            popup: popover::Popup::default(),
            agent_chat: None,
            breakdown: None,
            breakdown_read_at: None,
            breakdown_task: None,
            hover_opened: false,
            ring_hovered: false,
            card_hovered: false,
            hover_open: None,
            hover_close: None,
            _poll: poll,
            // Settings → Accounts writes the same cache.
            _cache: cx.observe_global::<AccountsSnapshotCache>(|_, cx| cx.notify()),
        }
    }

    /// Point at the session's harness and device; loads on first sight of a
    /// device. Cheap enough to call every render.
    pub fn track(
        &mut self,
        harness: Option<HarnessId>,
        target: Option<String>,
        agent_chat: Option<AgentChat>,
        cx: &mut Context<Self>,
    ) {
        let harness = harness.filter(|h| signs_in(*h) && reports_usage(*h));
        self.harness = harness;
        if self.target != target {
            self.target = target;
            self.loaded = false;
            self.last_forced = None;
            self.error = None;
        }
        if self.agent_chat != agent_chat {
            self.agent_chat = agent_chat;
            self.breakdown = None;
            self.breakdown_read_at = None;
            self.breakdown_task = None;
        }
        if harness.is_some() && !self.loaded {
            self.loaded = true;
            self.load(true, cx);
        }
    }

    /// Read the agent's session log for the bucket view, off the main
    /// thread, at most once per [`BREAKDOWN_MIN_INTERVAL`].
    fn read_breakdown(&mut self, cx: &mut Context<Self>) {
        let Some(agent_chat) = self.agent_chat.clone() else {
            return;
        };
        if self.breakdown_task.is_some()
            || self
                .breakdown_read_at
                .is_some_and(|at| at.elapsed() < BREAKDOWN_MIN_INTERVAL)
        {
            return;
        }
        let window = self
            .state
            .read(cx)
            .context_usage
            .and_then(|usage| usage.window);
        self.breakdown_task = Some(cx.spawn(async move |this, cx| {
            let breakdown = cx
                .background_executor()
                .spawn(async move {
                    crate::context_usage::measure(
                        Path::new(&agent_chat.project),
                        &agent_chat.chat_id,
                        window,
                    )
                })
                .await;
            this.update(cx, |usage, cx| {
                usage.breakdown = breakdown;
                usage.breakdown_read_at = Some(Instant::now());
                usage.breakdown_task = None;
                cx.notify();
            })
            .ok();
        }));
    }

    /// The pointer entered or left the context ring. A rest on the ring
    /// opens the card when nothing is up; leaving starts the grace timer.
    fn ring_hover(&mut self, hovered: bool, cx: &mut Context<Self>) {
        self.ring_hovered = hovered;
        if !hovered {
            self.hover_open = None;
            self.schedule_hover_close(cx);
            return;
        }
        self.hover_close = None;
        if self.popup.as_open().is_some() {
            return;
        }
        self.read_breakdown(cx);
        self.hover_open = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HOVER_OPEN_DELAY).await;
            this.update(cx, |usage, cx| {
                if usage.ring_hovered && usage.popup.as_open().is_none() {
                    usage.hover_opened = true;
                    usage.popup.open(FooterCard::Context);
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// The pointer entered or left the hover-opened context card.
    fn card_hover(&mut self, hovered: bool, cx: &mut Context<Self>) {
        self.card_hovered = hovered;
        if hovered {
            self.hover_close = None;
        } else {
            self.schedule_hover_close(cx);
        }
    }

    /// The context card is up and came from a hover, not a click.
    fn hover_card_up(&self) -> bool {
        self.hover_opened && self.popup.as_open() == Some(&FooterCard::Context)
    }

    /// Close a hover-opened card once the pointer has been off both the
    /// ring and the card for [`HOVER_CLOSE_GRACE`]. A pinned card stays.
    fn schedule_hover_close(&mut self, cx: &mut Context<Self>) {
        if !self.hover_card_up() {
            return;
        }
        self.hover_close = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HOVER_CLOSE_GRACE).await;
            this.update(cx, |usage, cx| {
                if usage.hover_card_up() && !usage.ring_hovered && !usage.card_hovered {
                    usage.dismiss(cx);
                }
            })
            .ok();
        }));
    }

    fn snapshot<'a>(&self, cx: &'a gpui::App) -> Option<&'a AgentAccountsSnapshot> {
        cx.try_global::<AccountsSnapshotCache>()?
            .0
            .get(&self.target)
    }

    fn params(&self, mut value: serde_json::Value) -> serde_json::Value {
        if let (Some(target), Some(object)) = (&self.target, value.as_object_mut()) {
            object.insert("targetDeviceId".into(), serde_json::json!(target));
        }
        value
    }

    /// Plain list first when nothing is cached (the engine's persisted usage
    /// paints at once), then the forced probe replaces it.
    fn load(&mut self, force_usage: bool, cx: &mut Context<Self>) {
        if force_usage {
            if self
                .last_forced
                .is_some_and(|at| at.elapsed() < FORCE_MIN_INTERVAL)
            {
                return;
            }
            self.last_forced = Some(Instant::now());
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let key = self.target.clone();
        let paint_first = force_usage && self.snapshot(cx).is_none();
        let plain = self.params(serde_json::json!({ "forceUsage": false }));
        let params = self.params(serde_json::json!({ "forceUsage": force_usage }));
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let fetch = |params: serde_json::Value| {
                let engine = engine.clone();
                async move {
                    engine
                        .client()
                        .call(methods::LIST_AGENT_ACCOUNTS, params)
                        .await
                        .ok()
                        .and_then(|value| {
                            serde_json::from_value::<AgentAccountsSnapshot>(value).ok()
                        })
                }
            };
            let store = |snapshot: AgentAccountsSnapshot, cx: &mut gpui::AsyncApp| {
                let key = key.clone();
                this.update(cx, |_, cx| {
                    cx.default_global::<AccountsSnapshotCache>()
                        .0
                        .insert(key, snapshot);
                })
                .ok();
            };
            if paint_first && let Some(snapshot) = fetch(plain).await {
                store(snapshot, cx);
            }
            if let Some(snapshot) = fetch(params).await {
                store(snapshot, cx);
            }
        }));
    }

    /// Switch optimistically, like Settings → Accounts: the rows flip at once
    /// and the engine's reply replaces them; a refusal restores the list.
    fn switch(&mut self, account: &AgentAccount, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let key = self.target.clone();
        let cache = &mut cx.default_global::<AccountsSnapshotCache>().0;
        let previous = cache.get(&key).cloned();
        if let Some(snapshot) = cache.get_mut(&key) {
            // Per-provider agents keep one live login per provider: only the
            // switched-to row's group changes.
            accounts::mark_switched(snapshot, account);
        }
        self.error = None;
        let params = self.params(serde_json::json!({
            "id": account.id,
            "accountId": account.id,
            "harness": account.harness,
        }));
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::ACTIVATE_AGENT_ACCOUNT, params)
                .await;
            this.update(cx, |usage, cx| {
                match result.map(serde_json::from_value::<AgentAccountsSnapshot>) {
                    Ok(Ok(snapshot)) => {
                        cx.default_global::<AccountsSnapshotCache>()
                            .0
                            .insert(key, snapshot);
                    }
                    Ok(Err(_)) => usage.load(false, cx),
                    Err(err) => {
                        if let Some(previous) = previous {
                            cx.default_global::<AccountsSnapshotCache>()
                                .0
                                .insert(key, previous);
                        }
                        usage.error = Some(err.to_string().into());
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// The ring's reading: the live account's most-used window, when the
    /// harness has a live account with usage to show.
    fn fraction(&self, cx: &gpui::App) -> Option<f32> {
        used_fraction(active_account(self.snapshot(cx)?, self.harness?)?)
    }

    /// A trigger click: open this ring's popover, or close it when the
    /// press found it open (the card's mouse-down-out already began that
    /// close — see `Popup::note_trigger_press`).
    fn toggle(&mut self, card: FooterCard, cx: &mut Context<Self>) {
        let press_was_open = self.popup.take_press_was_open();
        self.hover_open = None;
        if card == FooterCard::Context && self.hover_opened {
            // The context card last opened from a hover. A click on the
            // ring pins it: it stays until a click outside. The card's own
            // mouse-down-out may have begun closing it on this same press,
            // so open it again rather than trust its state. `hover_opened`
            // also survives a hover close, in which case this is a plain
            // open.
            self.hover_opened = false;
            self.hover_close = None;
            self.popup.open(card);
            self.read_breakdown(cx);
            cx.notify();
            return;
        }
        if press_was_open || self.popup.as_open() == Some(&card) {
            self.dismiss(cx);
            return;
        }
        match card {
            // Opening the card is the moment someone cares: re-probe.
            FooterCard::Accounts => self.load(true, cx),
            FooterCard::Context => self.read_breakdown(cx),
        }
        self.popup.open(card);
        cx.notify();
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        self.hover_close = None;
        self.card_hovered = false;
        if self.popup.begin_close() {
            popover::reap_popup(cx, |usage: &mut Self| &mut usage.popup);
        }
        cx.notify();
    }

    /// A footer ring that opens `card` above itself, right-aligned like the
    /// model picker.
    fn trigger<E: gpui::InteractiveElement + IntoElement>(
        &self,
        chip: gpui::Stateful<gpui::Div>,
        card: FooterCard,
        content: impl FnOnce(&Self, &mut Context<Self>) -> E,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let chip = chip
            .relative()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(move |usage, _, _, _| {
                    usage
                        .popup
                        .note_trigger_press_matching(|open| *open == card);
                }),
            )
            .on_click(cx.listener(move |usage, _, _, cx| usage.toggle(card, cx)));
        if self.popup.get() != Some(&card) {
            return chip.into_any_element();
        }
        let content = content(self, cx)
            .on_mouse_down_out(cx.listener(|usage, _, _, cx| usage.dismiss(cx)))
            .into_any_element();
        chip.child(popover::anchored_menu_above_end(
            card.id(),
            content,
            self.popup.closing_since(),
        ))
        .into_any_element()
    }

    fn accounts_card(&self, cx: &mut Context<Self>) -> gpui::Div {
        let theme = &Theme::of(cx).for_popup();
        let harness = self.harness;
        let rows: Vec<AgentAccount> = match (harness, self.snapshot(cx)) {
            (Some(harness), Some(snapshot)) => accounts::provider_accounts(snapshot, harness)
                .into_iter()
                .cloned()
                .collect(),
            _ => Vec::new(),
        };
        let title = format!(
            "{} accounts",
            harness.map_or("Agent", accounts::provider_name)
        );
        popover::popover_card(theme)
            .w(px(400.0))
            .flex()
            .flex_col()
            .child(popover::menu_heading(theme, &title))
            .children(rows.iter().enumerate().map(|(ix, account)| {
                let email: SharedString = account
                    .email
                    .clone()
                    .or_else(|| account.display_name.clone())
                    .unwrap_or_else(|| "Unknown account".into())
                    .into();
                let can_switch = !account.active && account.switchable;
                let mut meta: Vec<gpui::AnyElement> = account
                    .plan_label
                    .iter()
                    .map(|plan| {
                        div()
                            .child(SharedString::from(plan.clone()))
                            .into_any_element()
                    })
                    .collect();
                if account.active {
                    meta.push(
                        div()
                            .text_color(theme.accent)
                            .child(SharedString::from("In use"))
                            .into_any_element(),
                    );
                } else if let Some(reason) = account
                    .usage_error
                    .clone()
                    .filter(|_| account.usage_windows.is_empty())
                {
                    meta.push(div().child(SharedString::from(reason)).into_any_element());
                }
                let switch_to = account.clone();
                popover::menu_row(theme, account.active, format!("account-usage-row-{ix}"))
                    .id(("account-usage-row", ix))
                    .when(!can_switch, |row| row.cursor_default())
                    .when(can_switch, |row| {
                        row.on_click(cx.listener(move |usage, _, _, cx| {
                            usage.switch(&switch_to, cx);
                        }))
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(12.5))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(email),
                            )
                            .when(!meta.is_empty(), |el| {
                                el.child(crate::settings::widgets::meta_line(theme, meta))
                            }),
                    )
                    .child(
                        div().flex_none().flex().flex_col().gap(px(2.0)).children(
                            account
                                .usage_windows
                                .iter()
                                .take(2)
                                .map(|window| render_usage_meter(window, theme)),
                        ),
                    )
            }))
            .children(self.error.clone().map(|error| {
                div()
                    .px(px(8.0))
                    .py(px(4.0))
                    .text_size(px(12.0))
                    .text_color(theme.danger)
                    .child(error)
            }))
    }
}

/// Which footer ring's popover is up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FooterCard {
    Accounts,
    Context,
}

impl FooterCard {
    fn id(self) -> &'static str {
        match self {
            FooterCard::Accounts => "account-usage-menu",
            FooterCard::Context => "context-usage-menu",
        }
    }
}

/// The footer's ring cluster: account usage (accent arc, so the two read
/// apart), then context occupancy. Each opens its popover on click; the
/// context ring also opens its bucket view on a hover, and a click pins it.
impl Render for AccountUsage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let context = self.state.read(cx).context_usage;
        let account = self.fraction(cx).map(|fraction| {
            let level = usage_level(fraction);
            let chip = crate::context_usage::ring_chip(
                "account-usage",
                fraction,
                usage_color(level, &theme),
                match level {
                    UsageLevel::Normal => theme.text_muted,
                    _ => usage_color(level, &theme),
                },
                format!("{}%", (fraction * 100.0).round() as u32),
                self.popup.get() == Some(&FooterCard::Accounts),
                &theme,
            );
            self.trigger(
                chip,
                FooterCard::Accounts,
                |usage, cx| usage.accounts_card(cx),
                cx,
            )
        });
        let context = crate::context_usage::has_window(context).then(|| {
            let open = self.popup.get() == Some(&FooterCard::Context);
            if open {
                // The card is on screen: keep its numbers current as turns run.
                self.read_breakdown(cx);
            }
            let chip = crate::context_usage::chip(context, open, &theme).on_hover(
                cx.listener(|usage, hovered: &bool, _, cx| usage.ring_hover(*hovered, cx)),
            );
            self.trigger(
                chip,
                FooterCard::Context,
                move |usage, cx| {
                    crate::context_usage::card(
                        context,
                        usage.breakdown.as_ref(),
                        &Theme::of(cx).for_popup(),
                    )
                    .id("context-usage-card")
                    .on_hover(
                        cx.listener(|usage, hovered: &bool, _, cx| usage.card_hover(*hovered, cx)),
                    )
                },
                cx,
            )
        });
        div()
            .flex()
            .items_center()
            .gap(px(4.0))
            .children(account)
            .children(context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeron_proto::AgentUsageWindow;

    fn account(harness: HarnessId, active: bool, used: &[f32]) -> AgentAccount {
        serde_json::from_value(serde_json::json!({
            "id": format!("{harness:?}-{active}"),
            "harness": harness,
            "email": null,
            "planLabel": null,
            "active": active,
            "switchable": true,
            "usageWindows": used
                .iter()
                .map(|fraction| AgentUsageWindow {
                    label: "5h".into(),
                    used_fraction: *fraction,
                    resets_at: None,
                })
                .collect::<Vec<_>>(),
        }))
        .unwrap()
    }

    #[test]
    fn ring_shows_the_most_used_window() {
        assert_eq!(used_fraction(&account(HarnessId::Codex, true, &[])), None);
        assert_eq!(
            used_fraction(&account(HarnessId::Codex, true, &[0.12, 0.64])),
            Some(0.64)
        );
        assert_eq!(
            used_fraction(&account(HarnessId::Codex, true, &[1.4])),
            Some(1.0)
        );
    }

    #[test]
    fn active_account_is_scoped_to_the_harness() {
        let snapshot = AgentAccountsSnapshot {
            accounts: vec![
                account(HarnessId::ClaudeCode, true, &[0.3]),
                account(HarnessId::Codex, false, &[0.1]),
                account(HarnessId::Codex, true, &[0.2]),
            ],
            warnings: Vec::new(),
        };
        assert_eq!(
            active_account(&snapshot, HarnessId::Codex).map(|a| a.id.as_str()),
            Some("Codex-true")
        );
        assert!(active_account(&snapshot, HarnessId::Cursor).is_none());
    }
}

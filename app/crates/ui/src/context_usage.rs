//! Context occupancy is read from the replicated chat snapshot, never local CLI state.
//!
//! The bucket view behind the ring is different: it is what the last request
//! of an Arbos agent was made of, read from the agent's own session log
//! through [`zeron_engine::arbos_context`]. It exists only for a chat that is
//! an agent of a local Arbos project.
use std::path::Path;

use crate::theme::Theme;
use gpui::{IntoElement, PathBuilder, SharedString, canvas, div, point, prelude::*, px, relative};
use zeron_engine::arbos_context::{self, UsageView};
use zeron_proto::ContextUsage;

/// One row of the bucket view.
#[derive(Debug, Clone, PartialEq)]
pub struct BucketRow {
    pub label: &'static str,
    pub tokens: u64,
    /// This bucket's part of the request, 0 to 1. The bars compare the
    /// buckets with each other; the total line compares the request with
    /// the window.
    pub share: f32,
}

/// What filled the last request, bucket by bucket.
#[derive(Debug, Clone, PartialEq)]
pub struct Breakdown {
    /// The request as a whole, in tokens.
    pub used: u64,
    pub window: Option<u64>,
    /// The buckets are fx's own measurement of the request. Otherwise they
    /// are an estimate from the texts in the log.
    pub measured: bool,
    pub turns: usize,
    pub rows: Vec<BucketRow>,
}

/// The bucket view for the chat `chat_id` of the project at `project`, or
/// `None` when that chat is not one of the project's agents. Reads the
/// agent's session log: call it off the main thread.
pub fn measure(project: &Path, chat_id: &str, window: Option<u64>) -> Option<Breakdown> {
    arbos_context::usage_for_chat(project, chat_id, window).map(|view| breakdown(&view))
}

fn breakdown(view: &UsageView) -> Breakdown {
    let sum: u64 = view.buckets.iter().map(|bucket| bucket.tokens).sum();
    let whole = view.used.max(sum).max(1) as f32;
    Breakdown {
        used: view.used,
        window: view.window,
        measured: view.measured,
        turns: view.turns,
        rows: view
            .buckets
            .iter()
            .map(|bucket| BucketRow {
                label: bucket_label(bucket.name),
                tokens: bucket.tokens,
                share: bucket.tokens as f32 / whole,
            })
            .collect(),
    }
}

/// The engine names buckets the way fx logs them; the view says them the
/// way a person reads them.
fn bucket_label(name: &str) -> &'static str {
    match name {
        "system prompt" => "System prompt",
        "tools" => "Tools",
        "rules" => "Rules",
        "skills" => "Skills",
        "summarized conversation" => "Summary",
        "conversation" => "Conversation",
        _ => "Other",
    }
}

/// The first line of the bucket view: the request against the window.
fn total_line(breakdown: &Breakdown) -> String {
    match breakdown.window {
        Some(window) if window > 0 => format!(
            "{} / {} tokens",
            with_separators(breakdown.used),
            with_separators(window)
        ),
        _ => format!("{} tokens", with_separators(breakdown.used)),
    }
}

/// The percent beside [`total_line`], when the window is known.
fn total_percent(breakdown: &Breakdown) -> Option<String> {
    let window = breakdown.window.filter(|window| *window > 0)?;
    Some(format!(
        "{}%",
        (breakdown.used * 100).div_ceil(window).min(100)
    ))
}

/// The request as a part of the window, 0 to 1, for the total bar.
fn total_fraction(breakdown: &Breakdown) -> f32 {
    match breakdown.window {
        Some(window) if window > 0 => (breakdown.used as f32 / window as f32).clamp(0.0, 1.0),
        _ => 0.0,
    }
}

/// The last line of the bucket view: where the numbers come from.
fn source_line(breakdown: &Breakdown) -> &'static str {
    if breakdown.measured {
        "Measured on the last request"
    } else if breakdown.turns == 0 {
        "Estimated. No request sent yet"
    } else {
        "Estimated until the next request"
    }
}

/// The context ring's trigger chip; the footer ([`crate::account_usage`])
/// opens [`card`] from it on click.
pub fn chip(usage: Option<ContextUsage>, open: bool, theme: &Theme) -> gpui::Stateful<gpui::Div> {
    let fraction = usage.and_then(ContextUsage::fraction);
    let color = match fraction {
        Some(f) if f >= 0.9 => theme.danger,
        Some(f) if f >= 0.75 => theme.warning,
        Some(_) => theme.text_muted,
        None => theme.text_faint,
    };
    let label = fraction
        .map(|f| format!("{:.0}%", f * 100.0))
        .unwrap_or_else(|| "—".into());
    ring_chip(
        "context-usage",
        fraction.unwrap_or(0.0) as f32,
        color,
        color,
        label,
        open,
        theme,
    )
}

/// One footer ring indicator: ring + percent, identical geometry for every
/// ring so they sit side by side as equals. `arc` colours the ring's fill,
/// `text` the label; `open` holds the hover wash while its popover is up.
pub(crate) fn ring_chip(
    id: &'static str,
    fraction: f32,
    arc: gpui::Hsla,
    text: gpui::Hsla,
    label: String,
    open: bool,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .flex()
        .items_center()
        .gap(px(5.0))
        .h(px(24.0))
        .px(px(6.0))
        .rounded(px(6.0))
        .text_size(px(11.0))
        .text_color(text)
        .cursor_pointer()
        .when(open, |s| s.bg(crate::theme::ink(0.05)))
        .hover(|s| s.bg(crate::theme::ink(0.05)))
        .child(ring(fraction, arc, theme))
        .child(SharedString::from(label))
}

/// The footer's 16px progress ring: a faint full track under a `fraction` arc
/// starting at twelve o'clock. Shared with the account usage indicator.
pub(crate) fn ring(fraction: f32, color: gpui::Hsla, theme: &Theme) -> impl IntoElement {
    let track = theme.text_faint.opacity(0.25);
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let center = bounds.center();
            let mut arc = |fraction: f32, color| {
                if fraction <= 0.0 {
                    return;
                }
                let steps = (64.0 * fraction).ceil().max(2.0) as usize;
                let mut path = PathBuilder::stroke(px(1.8));
                for i in 0..=steps {
                    let angle = -std::f32::consts::FRAC_PI_2
                        + std::f32::consts::TAU * fraction * i as f32 / steps as f32;
                    let p = point(
                        center.x + px(6.0 * angle.cos()),
                        center.y + px(6.0 * angle.sin()),
                    );
                    if i == 0 {
                        path.move_to(p);
                    } else {
                        path.line_to(p);
                    }
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, color);
                }
            };
            arc(1.0, track);
            arc(fraction.clamp(0.0, 1.0), color);
        },
    )
    .size(px(16.0))
}

fn with_separators(count: u64) -> String {
    let digits = count.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// whether the indicator has anything to measure against: harnesses that
/// never report a window (antigravity) get no indicator at all, rather than a
/// permanently empty ring.
pub fn has_window(usage: Option<ContextUsage>) -> bool {
    usage
        .and_then(|usage| usage.window)
        .is_some_and(|window| window > 0)
}

fn details(usage: Option<ContextUsage>) -> String {
    match usage.unwrap_or_default() {
        ContextUsage {
            tokens: Some(tokens),
            window: Some(window),
        } if window > 0 => {
            format!(
                "{} / {} tokens\n{} tokens remaining",
                with_separators(tokens),
                with_separators(window),
                with_separators(window.saturating_sub(tokens))
            )
        }
        ContextUsage {
            tokens: Some(tokens),
            ..
        } => format!(
            "{} tokens used\nContext limit not reported",
            with_separators(tokens)
        ),
        ContextUsage {
            window: Some(window),
            ..
        } if window > 0 => format!(
            "{} token capacity\nWaiting for context usage",
            with_separators(window)
        ),
        _ => "Context usage not reported by this harness yet".into(),
    }
}

/// The context ring's popover content. With a [`Breakdown`] it is the bucket
/// view; without one (a chat that is not an Arbos agent, or a log not read
/// yet) it is the plain used-and-remaining text.
pub fn card(
    usage: Option<ContextUsage>,
    breakdown: Option<&Breakdown>,
    theme: &Theme,
) -> gpui::Div {
    let card = crate::popover::popover_card(theme)
        .flex()
        .flex_col()
        .child(crate::popover::menu_heading(theme, "Context window"));
    match breakdown {
        Some(breakdown) => card.child(buckets(breakdown, theme)),
        None => card.child(
            div()
                .px(px(8.0))
                .pb(px(6.0))
                .text_size(px(12.0))
                .line_height(px(19.0))
                .whitespace_nowrap()
                .text_color(theme.text_muted)
                .child(SharedString::from(details(usage))),
        ),
    }
}

const BUCKET_CARD_WIDTH: f32 = 236.0;
const BAR_HEIGHT: f32 = 4.0;

/// The total line and bar, one row per bucket, and the source line.
fn buckets(breakdown: &Breakdown, theme: &Theme) -> gpui::Div {
    let total_fraction = total_fraction(breakdown);
    let total_color = match total_fraction {
        f if f >= 0.9 => theme.danger,
        f if f >= 0.75 => theme.warning,
        _ => theme.accent,
    };
    div()
        .w(px(BUCKET_CARD_WIDTH))
        .px(px(8.0))
        .pb(px(6.0))
        .flex()
        .flex_col()
        .gap(px(6.0))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(4.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .text_size(px(12.0))
                        .child(
                            div()
                                .text_color(theme.text)
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child(SharedString::from(total_line(breakdown))),
                        )
                        .children(total_percent(breakdown).map(|percent| {
                            div()
                                .text_color(theme.text_muted)
                                .child(SharedString::from(percent))
                        })),
                )
                .child(bar(total_fraction, breakdown.used > 0, total_color, theme)),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(5.0))
                .pt(px(2.0))
                .children(breakdown.rows.iter().map(|row| bucket_row(row, theme))),
        )
        .child(
            div()
                .pt(px(2.0))
                .text_size(px(11.0))
                .text_color(theme.text_faint)
                .child(SharedString::from(source_line(breakdown))),
        )
}

fn bucket_row(row: &BucketRow, theme: &Theme) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(px(3.0))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .text_size(px(11.5))
                .child(
                    div()
                        .text_color(theme.text_muted)
                        .child(SharedString::from(row.label)),
                )
                .child(
                    div()
                        .text_color(theme.text)
                        .child(SharedString::from(with_separators(row.tokens))),
                ),
        )
        .child(bar(row.share, row.tokens > 0, theme.text_muted, theme))
}

/// A thin bar: a faint full track under a `fraction` fill. A bucket that
/// holds anything shows at least a sliver, so a 0.2% rule file does not
/// read as empty.
fn bar(fraction: f32, nonzero: bool, color: gpui::Hsla, theme: &Theme) -> gpui::Div {
    let fraction = fraction.clamp(0.0, 1.0);
    div()
        .w_full()
        .h(px(BAR_HEIGHT))
        .rounded(px(BAR_HEIGHT / 2.0))
        .bg(theme.text_faint.opacity(0.2))
        .overflow_hidden()
        .child(
            div()
                .h_full()
                .w(relative(fraction))
                .when(nonzero, |fill| fill.min_w(px(2.0)))
                .rounded(px(BAR_HEIGHT / 2.0))
                .bg(color),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn indicator_needs_a_reported_window() {
        assert!(!has_window(None));
        assert!(!has_window(Some(ContextUsage {
            tokens: Some(1_200),
            window: None,
        })));
        assert!(!has_window(Some(ContextUsage {
            tokens: Some(1_200),
            window: Some(0),
        })));
        assert!(has_window(Some(ContextUsage {
            tokens: None,
            window: Some(200_000),
        })));
    }

    #[test]
    fn missing_usage_is_distinct_from_zero_and_overflow() {
        assert!(details(None).contains("not reported"));
        assert!(
            details(Some(ContextUsage {
                tokens: Some(0),
                window: Some(200)
            }))
            .contains("200 tokens remaining")
        );
        assert!(
            details(Some(ContextUsage {
                tokens: Some(250),
                window: Some(200)
            }))
            .contains("0 tokens remaining")
        );
        assert!(
            details(Some(ContextUsage {
                tokens: Some(10),
                window: Some(0)
            }))
            .contains("limit not reported")
        );
    }

    fn view(used: u64, window: Option<u64>, measured: bool, tokens: [u64; 6]) -> UsageView {
        let names = [
            "system prompt",
            "tools",
            "rules",
            "skills",
            "summarized conversation",
            "conversation",
        ];
        UsageView {
            used,
            window,
            percent: window.map(|window| (used * 100).div_ceil(window)),
            measured,
            turns: if measured { 3 } else { 0 },
            buckets: names
                .iter()
                .zip(tokens)
                .map(|(name, tokens)| arbos_context::Bucket {
                    name,
                    tokens,
                    percent: None,
                })
                .collect(),
            history: Vec::new(),
        }
    }

    #[test]
    fn the_bucket_view_lists_the_six_sections_in_order_with_their_share() {
        let view = view(
            9_400,
            Some(200_000),
            true,
            [1_500, 4_000, 300, 120, 800, 2_600],
        );
        let breakdown = breakdown(&view);
        assert_eq!(
            breakdown
                .rows
                .iter()
                .map(|row| row.label)
                .collect::<Vec<_>>(),
            [
                "System prompt",
                "Tools",
                "Rules",
                "Skills",
                "Summary",
                "Conversation"
            ]
        );
        assert_eq!(breakdown.rows[1].tokens, 4_000);
        // 9,320 of the 9,400 are in the buckets; shares are against the request.
        assert!((breakdown.rows[1].share - 4_000.0 / 9_400.0).abs() < 1e-6);
        assert!((breakdown.rows[3].share - 120.0 / 9_400.0).abs() < 1e-6);
        assert_eq!(total_line(&breakdown), "9,400 / 200,000 tokens");
        assert_eq!(total_percent(&breakdown).as_deref(), Some("5%"));
        assert!((total_fraction(&breakdown) - 0.047).abs() < 1e-6);
        assert_eq!(source_line(&breakdown), "Measured on the last request");
    }

    #[test]
    fn shares_follow_the_bucket_sum_when_it_exceeds_the_request() {
        let view = view(0, None, false, [1_000, 0, 0, 0, 0, 3_000]);
        let breakdown = breakdown(&view);
        assert!((breakdown.rows[0].share - 0.25).abs() < 1e-6);
        assert!((breakdown.rows[5].share - 0.75).abs() < 1e-6);
        assert_eq!(breakdown.rows[1].share, 0.0);
        assert_eq!(total_line(&breakdown), "0 tokens");
        assert_eq!(total_percent(&breakdown), None);
        assert_eq!(total_fraction(&breakdown), 0.0);
        assert_eq!(source_line(&breakdown), "Estimated. No request sent yet");
    }

    #[test]
    fn an_empty_estimate_has_no_share_and_an_old_log_says_until_the_next_request() {
        let empty = breakdown(&view(0, Some(1_000), false, [0; 6]));
        assert!(empty.rows.iter().all(|row| row.share == 0.0));
        let mut stale = view(2_400, Some(1_000), false, [2_000, 0, 0, 0, 0, 400]);
        stale.turns = 4;
        let stale = breakdown(&stale);
        assert_eq!(source_line(&stale), "Estimated until the next request");
        assert_eq!(total_percent(&stale).as_deref(), Some("100%"));
        assert_eq!(total_fraction(&stale), 1.0);
    }

    #[test]
    fn unknown_bucket_names_read_as_other() {
        assert_eq!(bucket_label("mcp"), "Other");
        assert_eq!(bucket_label("summarized conversation"), "Summary");
    }

    #[test]
    fn a_chat_outside_an_arbos_project_has_no_bucket_view() {
        let root = std::env::temp_dir().join(format!("arbos-ring-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(measure(&root, "some-chat", Some(1_000)), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn token_counts_are_grouped_by_thousands() {
        assert_eq!(with_separators(0), "0");
        assert_eq!(with_separators(999), "999");
        assert_eq!(with_separators(5417), "5,417");
        assert_eq!(with_separators(1_048_576), "1,048,576");
        assert_eq!(
            details(Some(ContextUsage {
                tokens: Some(5417),
                window: Some(1_048_576)
            })),
            "5,417 / 1,048,576 tokens\n1,043,159 tokens remaining"
        );
    }
}

//! Group chats in the shell (F6, design S15 / D69): stacked participant glyphs in a group tab,
//! the sender chip above another Project's message, and the "+ project" control in the tab
//! header with its picker. Everything here is pure rendering over `AppState`; the shell owns
//! the open/closed state of the picker.

use gpui::{div, prelude::*, px, Context, Div, ElementId, Entity, Hsla, SharedString, Stateful};

use crate::app::AppState;
use crate::model::{Chat, ChatKind, Project};
use crate::ui::frost::{frosted, MENU_BLUR};
use crate::ui::shell::Shell;
use crate::ui::theme::{Theme, PANEL_RADIUS};

const MAX_GLYPHS: usize = 4;
const GLYPH: f32 = 16.0;

/// `#rrggbb` → colour; anything else falls back.
pub fn project_color(hex: &str, fallback: Hsla) -> Hsla {
    let h = hex.trim().trim_start_matches('#');
    if h.len() != 6 {
        return fallback;
    }
    match u32::from_str_radix(h, 16) {
        Ok(v) => gpui::rgb(v).into(),
        Err(_) => fallback,
    }
}

/// A stable accent for a sender id when its Project is not in the registry (e.g. in a
/// participant's "Projects ⇄" tab): a hue from the id, the theme's saturation.
pub fn sender_color(seed: &str, theme: &Theme) -> Hsla {
    let mut h: u32 = 2166136261;
    for b in seed.bytes() {
        h = (h ^ b as u32).wrapping_mul(16777619);
    }
    let mut c = theme.accent;
    c.h = (h % 360) as f32 / 360.0;
    c.s = c.s.max(0.45);
    c
}

fn glyph(icon: &str, color: Hsla, size: f32, theme: &Theme) -> Div {
    div()
        .size(px(size))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(Theme::with_alpha(color, 0.18))
        .border_1()
        .border_color(Theme::with_alpha(color, 0.8))
        .text_size(px(size * 0.58))
        .text_color(theme.text)
        .child(icon.to_owned())
}

/// Group tab: the participants' glyphs stacked, host first. The "Projects ⇄" tab shows `⇄`.
pub fn tab_glyphs(chat: &Chat, projects: &[Project], theme: &Theme) -> Option<Div> {
    match chat.kind {
        ChatKind::Group => {
            let mut row = div().flex().items_center().flex_shrink_0();
            for (i, id) in chat.participants.iter().take(MAX_GLYPHS).enumerate() {
                let found = projects.iter().find(|p| &p.id == id);
                let (icon, color) = found
                    .map(|p| (p.icon.clone(), project_color(&p.color, theme.accent)))
                    .unwrap_or_else(|| ("·".to_owned(), sender_color(id, theme)));
                let slug = found.and_then(|p| crate::ui::avatars::slug_for(p, projects));
                let head = match slug {
                    Some(slug) => crate::ui::avatars::element(
                        Some(&slug),
                        crate::ui::avatars::AvatarState::Idle,
                        GLYPH,
                        &icon,
                        color,
                        theme,
                    ),
                    None => glyph(&icon, color, GLYPH, theme),
                };
                row = row.child(head.when(i > 0, |el| el.ml(px(-5.0))));
            }
            let extra = chat.participants.len().saturating_sub(MAX_GLYPHS);
            if extra > 0 {
                row = row.child(
                    div()
                        .ml(px(3.0))
                        .text_size(px(10.0))
                        .text_color(theme.text_faint)
                        .child(format!("+{extra}")),
                );
            }
            Some(row)
        }
        ChatKind::Projects => Some(
            div()
                .flex_shrink_0()
                .text_size(px(13.0))
                .text_color(theme.text_muted)
                .child("⇄"),
        ),
        ChatKind::Chat | ChatKind::Other => None,
    }
}

/// The caption above another Project's message: its glyph and name.
/// The sender chip above another agent's message, with its tree avatar when it has one.
pub fn sender_chip_with(
    name: &str,
    icon: Option<&str>,
    color: Hsla,
    avatar: Option<&str>,
    state: crate::ui::avatars::AvatarState,
    theme: &Theme,
) -> Div {
    let head = match avatar {
        Some(slug) => crate::ui::avatars::element(
            Some(slug),
            state,
            GLYPH + 4.0,
            icon.unwrap_or("·"),
            color,
            theme,
        ),
        None => glyph(icon.unwrap_or("·"), color, GLYPH, theme),
    };
    div()
        .w_full()
        .flex()
        .items_center()
        .gap(px(6.0))
        .pt(px(10.0))
        .pb(px(2.0))
        .child(head)
        .child(
            div()
                .text_size(px(11.0))
                .text_color(theme.text_muted)
                .child(name.to_owned()),
        )
}

/// "+ project" in the tab header. In a group it adds a participant; elsewhere it starts a
/// group hosted by the selected Project.
pub fn header_button(open: bool, theme: &Theme) -> Stateful<Div> {
    div()
        .id("add-project")
        .flex_shrink_0()
        .h(px(24.0))
        .px(px(8.0))
        .flex()
        .items_center()
        .rounded(px(6.0))
        .text_size(px(11.5))
        .whitespace_nowrap()
        .bg(if open {
            theme.surface_2
        } else {
            gpui::transparent_black()
        })
        .text_color(if open { theme.text } else { theme.text_muted })
        .hover(|s| s.bg(theme.surface_2))
        .cursor_pointer()
        .child("+ project")
}

/// The picker under "+ project": every other Project not yet in the active group.
pub fn picker(state: &Entity<AppState>, top: f32, right: f32, cx: &mut Context<Shell>) -> Div {
    let theme = Theme::of(cx).clone();
    let s = state.read(cx);
    let selected = s.selected_project.clone().unwrap_or_default();
    let group = s.active_chat().filter(|c| c.is_group()).cloned();
    let candidates: Vec<Project> = s
        .projects
        .iter()
        .filter(|p| p.id != selected)
        .filter(|p| !p.is_people() && !p.is_guest())
        .filter(|p| {
            group
                .as_ref()
                .map(|g| !g.participants.contains(&p.id))
                .unwrap_or(true)
        })
        .cloned()
        .collect();
    let heading = match &group {
        Some(g) => format!("ADD TO “{}”", g.title.to_uppercase()),
        None => "NEW GROUP WITH".to_owned(),
    };
    let mut card = div()
        .id("add-project-picker")
        .w(px(232.0))
        .flex()
        .flex_col()
        .gap(px(4.0))
        .p(px(8.0))
        .rounded(px(PANEL_RADIUS))
        .bg(theme.card())
        .border_1()
        .border_color(theme.border_strong)
        .shadow_lg()
        .child(
            div()
                .px(px(4.0))
                .pb(px(2.0))
                .text_size(px(10.5))
                .text_color(theme.text_faint)
                .overflow_hidden()
                .text_ellipsis()
                .child(heading),
        );
    if candidates.is_empty() {
        card = card.child(
            div()
                .px(px(4.0))
                .py(px(4.0))
                .text_size(px(12.0))
                .text_color(theme.text_muted)
                .child("Every other Project is already here."),
        );
    }
    for p in candidates {
        let id = p.id.clone();
        let color = project_color(&p.color, theme.accent);
        card = card.child(
            div()
                .id(ElementId::Name(SharedString::from(format!(
                    "pick-{}",
                    p.id
                ))))
                .flex()
                .items_center()
                .gap(px(8.0))
                .px(px(6.0))
                .py(px(5.0))
                .rounded(px(6.0))
                .text_size(px(12.5))
                .text_color(theme.text)
                .cursor_pointer()
                .hover(|s| s.bg(theme.surface_2))
                .child(glyph(&p.icon, color, 18.0, &theme))
                .child(div().flex_1().child(p.name.clone()))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.app_state()
                        .update(cx, |s, cx| s.add_project_to_chat(id.clone(), cx));
                    this.set_group_picker(false, cx);
                })),
        );
    }
    div()
        .absolute()
        .top(px(top))
        .right(px(right))
        .child(frosted(PANEL_RADIUS, MENU_BLUR, card))
}

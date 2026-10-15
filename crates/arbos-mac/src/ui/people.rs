//! People chats (D72) in the shell: avatars, the person tiles in the rail, the
//! `+` menu and new-chat sheet, the DM header with its member chips and
//! pickers, the typing line. Pure rendering over `AppState`; the shell owns the
//! open/closed flags.

use std::sync::Arc;

use gpui::{
    div, prelude::*, px, Context, Div, ElementId, Entity, Focusable as _, FontWeight, RenderImage,
    SharedString, Stateful,
};

use crate::app::AppState;
use crate::model::{AgentMember, Person, Project};
use crate::ui::composer::ComposerInput;
use crate::ui::frost::{frosted, MENU_BLUR, PANEL_BLUR};
use crate::ui::groups::{project_color, sender_color};
use crate::ui::shell::Shell;
use crate::ui::theme::{Theme, PANEL_RADIUS, RAIL_TILE, RAIL_WIDTH};

/// Round avatar: the GitHub picture when decoded, else an initial on a stable colour.
pub fn avatar(image: Option<Arc<RenderImage>>, person: &Person, size: f32, theme: &Theme) -> Div {
    let base = div()
        .size(px(size))
        .flex_shrink_0()
        .rounded_full()
        .overflow_hidden()
        .flex()
        .items_center()
        .justify_center();
    match image {
        Some(image) => base.child(
            gpui::img(image)
                .size(px(size))
                .rounded_full()
                .object_fit(gpui::ObjectFit::Cover),
        ),
        None => {
            let color = sender_color(&person.handle, theme);
            base.bg(Theme::with_alpha(color, 0.25))
                .border_1()
                .border_color(Theme::with_alpha(color, 0.8))
                .text_size(px(size * 0.46))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .child(person.initial())
        }
    }
}

/// A small square glyph for a shared agent (its Project's icon and colour),
/// or its tree avatar when the agent carries one.
pub fn agent_glyph(agent: &AgentMember, size: f32, theme: &Theme) -> Div {
    let color = project_color(&agent.color, theme.accent);
    if let Some(slug) = agent.avatar.as_deref() {
        return crate::ui::avatars::element(
            Some(slug),
            crate::ui::avatars::AvatarState::Idle,
            size,
            &agent.icon,
            color,
            theme,
        );
    }
    div()
        .size(px(size))
        .flex_shrink_0()
        .rounded(px(size * 0.28))
        .bg(Theme::with_alpha(color, 0.18))
        .border_1()
        .border_color(Theme::with_alpha(color, 0.8))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(size * 0.55))
        .text_color(theme.text)
        .child(agent.icon.clone())
}

/// Rail tile for a DM: round avatar, handle under it, unread badge, pending dot.
#[allow(clippy::too_many_arguments)]
pub fn person_tile(
    space: &Project,
    image: Option<Arc<RenderImage>>,
    selected: bool,
    collapsed: bool,
    unread: usize,
    typing: bool,
    theme: &Theme,
    cx: &mut Context<Shell>,
) -> Stateful<Div> {
    let Some(person) = space.person().cloned() else {
        return div().id("person-tile-empty");
    };
    let id = space.id.clone();
    let tile_size = if collapsed { 36.0 } else { RAIL_TILE };
    let mut tile = div()
        .id(ElementId::Name(SharedString::from(format!(
            "tile-{}",
            space.id
        ))))
        .relative()
        .w(px(tile_size))
        .h(px(if collapsed { 36.0 } else { RAIL_TILE - 6.0 }))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(3.0))
        .rounded(px(PANEL_RADIUS))
        .border_1()
        .border_color(if selected {
            theme.accent
        } else {
            gpui::transparent_black()
        })
        .bg(if selected {
            theme.surface_2
        } else {
            gpui::transparent_black()
        })
        .cursor_pointer()
        .hover(|s| s.bg(theme.surface_2))
        .on_click(cx.listener(move |this, _, window, cx| {
            this.state
                .update(cx, |s, cx| s.select_project(id.clone(), cx));
            crate::ui::composer::Composer::focus(&this.composer, window, cx);
        }))
        .child(avatar(
            image,
            &person,
            if collapsed { 26.0 } else { 30.0 },
            theme,
        ));
    if !collapsed {
        tile = tile.child(
            div()
                .text_size(px(10.5))
                .text_color(if space.pending {
                    theme.text_faint
                } else {
                    theme.text
                })
                .max_w(px(RAIL_TILE - 8.0))
                .truncate()
                .child(format!("@{}", person.handle)),
        );
    }
    if typing {
        tile = tile.child(
            div()
                .absolute()
                .top(px(5.0))
                .left(px(6.0))
                .size(px(6.0))
                .rounded_full()
                .bg(theme.accent),
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
    } else if space.pending {
        tile = tile.child(
            div()
                .absolute()
                .top(px(5.0))
                .right(px(6.0))
                .size(px(5.0))
                .rounded_full()
                .bg(theme.text_faint),
        );
    }
    tile
}

/// The `+` menu: New project / New chat with a person.
pub fn new_menu(cx: &mut Context<Shell>) -> Div {
    let theme = Theme::of(cx).clone();
    let row = |id: &'static str, label: &'static str, hint: &'static str, theme: &Theme| {
        div()
            .id(id)
            .flex()
            .flex_col()
            .gap(px(1.0))
            .px(px(10.0))
            .py(px(7.0))
            .rounded(px(6.0))
            .cursor_pointer()
            .hover(|s| s.bg(theme.surface_2))
            .child(
                div()
                    .text_size(px(12.5))
                    .text_color(theme.text)
                    .child(label),
            )
            .child(
                div()
                    .text_size(px(10.5))
                    .text_color(theme.text_faint)
                    .child(hint),
            )
    };
    let card = div()
        .w(px(230.0))
        .flex()
        .flex_col()
        .gap(px(2.0))
        .p(px(6.0))
        .rounded(px(PANEL_RADIUS))
        .bg(theme.card())
        .border_1()
        .border_color(theme.border_strong)
        .shadow_lg()
        .child(
            row(
                "new-menu-project",
                "New project",
                "A workspace on your machine",
                &theme,
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.new_menu_open = false;
                this.state.update(cx, |s, cx| {
                    s.new_project_open = true;
                    s.new_chat_open = false;
                    cx.notify();
                });
                let handle = this.project_input.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
            })),
        )
        .child(
            row(
                "new-menu-person",
                "New chat with a person",
                "Find anyone by GitHub handle",
                &theme,
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.new_menu_open = false;
                this.people_input.update(cx, |input, cx| input.clear(cx));
                this.state.update(cx, |s, cx| s.open_new_chat(cx));
                let handle = this.people_input.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
            })),
        );
    div()
        .absolute()
        .left(px(RAIL_WIDTH + 8.0))
        .bottom(px(48.0))
        .child(frosted(PANEL_RADIUS, MENU_BLUR, card))
}

/// Centered sheet: `@username` search over the relay directory; a row starts the DM.
pub fn new_chat_sheet(
    state: &Entity<AppState>,
    input: &Entity<ComposerInput>,
    cx: &mut Context<Shell>,
) -> Div {
    let theme = Theme::of(cx).clone();
    let s = state.read(cx);
    let query = s.people_query.clone();
    let busy = s.people_busy;
    let error = s.people_error.clone();
    let results: Vec<(Person, Option<Arc<RenderImage>>)> = s
        .people_results
        .iter()
        .map(|p| (p.clone(), s.avatar(&p.avatar_url)))
        .collect();
    let typed = query.trim().trim_start_matches('@').to_string();
    let invite_into = s
        .invite_target
        .as_ref()
        .and_then(|id| s.projects.iter().find(|p| &p.id == id))
        .map(|p| p.name.clone());

    let input_row = div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .h(px(40.0))
        .px(px(14.0))
        .rounded(px(20.0))
        .bg(theme.surface_1)
        .border_1()
        .border_color(theme.border_strong)
        .child(
            div()
                .text_size(px(14.0))
                .text_color(theme.text_faint)
                .child("@"),
        )
        .child(input.clone());

    let mut list = div().flex().flex_col().gap(px(2.0)).pt(px(6.0));
    if let Some(err) = error {
        list = list.child(
            div()
                .px(px(10.0))
                .py(px(6.0))
                .text_size(px(12.0))
                .text_color(theme.danger)
                .child(err),
        );
    }
    if results.is_empty() && !typed.is_empty() && !busy {
        list = list.child(
            div()
                .px(px(10.0))
                .py(px(6.0))
                .text_size(px(12.0))
                .text_color(theme.text_muted)
                .child(format!(
                    "No one on Arbos matches @{typed} yet. Enter sends an invite by GitHub login."
                )),
        );
    }
    if busy && results.is_empty() {
        list = list.child(
            div()
                .px(px(10.0))
                .py(px(6.0))
                .text_size(px(12.0))
                .text_color(theme.text_faint)
                .child("Searching…"),
        );
    }
    for (person, image) in results {
        let handle = person.handle.clone();
        let invited = person.status == "invited";
        list = list.child(
            div()
                .id(ElementId::Name(SharedString::from(format!(
                    "person-{}",
                    person.id
                ))))
                .flex()
                .items_center()
                .gap(px(10.0))
                .px(px(10.0))
                .py(px(7.0))
                .rounded(px(8.0))
                .cursor_pointer()
                .hover(|s| s.bg(theme.surface_2))
                .child(avatar(image, &person, 30.0, &theme))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_size(px(13.0))
                                .text_color(theme.text)
                                .child(format!("@{}", person.handle)),
                        )
                        .when_some(person.name.clone(), |el, name| {
                            el.child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(theme.text_muted)
                                    .child(name),
                            )
                        }),
                )
                .when(invited, |el| {
                    el.child(
                        div()
                            .px(px(6.0))
                            .py(px(2.0))
                            .rounded(px(6.0))
                            .bg(theme.surface_2)
                            .text_size(px(10.5))
                            .text_color(theme.text_muted)
                            .child("not on Arbos yet"),
                    )
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.state
                        .update(cx, |s, cx| s.pick_person(handle.clone(), cx));
                })),
        );
    }

    let (title, blurb) = match &invite_into {
        Some(name) => (
            format!("Invite to {name}"),
            "They get the same tabs and workspace output; your agent answers them too.".to_string(),
        ),
        None => (
            "New chat".to_string(),
            "Anyone with a GitHub account. The chat opens for both of you at once.".to_string(),
        ),
    };
    let card = div()
        .w(px(440.0))
        .max_w_full()
        .flex()
        .flex_col()
        .gap(px(10.0))
        .p(px(16.0))
        .rounded(px(PANEL_RADIUS))
        .bg(theme.card())
        .border_1()
        .border_color(theme.border_strong)
        .shadow_lg()
        .child(
            div()
                .flex()
                .items_center()
                .child(
                    div()
                        .flex_1()
                        .text_size(px(15.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.text)
                        .child(title),
                )
                .child(
                    div()
                        .id("new-chat-close")
                        .px(px(8.0))
                        .py(px(3.0))
                        .rounded(px(6.0))
                        .text_size(px(12.0))
                        .text_color(theme.text_muted)
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.surface_2))
                        .child("Close")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.state.update(cx, |s, cx| s.close_new_chat(cx));
                            crate::ui::composer::Composer::focus(&this.composer, window, cx);
                        })),
                ),
        )
        .child(
            div()
                .text_size(px(12.0))
                .text_color(theme.text_muted)
                .child(blurb),
        )
        .child(input_row)
        .child(list);
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(Theme::with_alpha(theme.bg, 0.45))
        .child(frosted(PANEL_RADIUS, PANEL_BLUR, card))
}

/// Strip under the tab bar in a people chat: avatar, @handle, name, member
/// chips (people + agents), lock state, "+ Add agent", "Members".
pub fn header(
    space: &Project,
    state: &Entity<AppState>,
    add_agent_open: bool,
    members_open: bool,
    cx: &mut Context<Shell>,
) -> Div {
    let theme = Theme::of(cx).clone();
    let s = state.read(cx);
    let Some(person) = space.person().cloned() else {
        return div();
    };
    let image = s.avatar(&person.avatar_url);
    let lock = if space.agents.is_empty() {
        "People only".to_string()
    } else {
        let owners: Vec<String> = space
            .agents
            .iter()
            .map(|a| format!("@{}", a.owner_handle))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        format!("Visible to {}'s agent", owners.join(" and "))
    };
    let pill = |id: &'static str, label: String, open: bool, theme: &Theme| {
        div()
            .id(id)
            .flex_shrink_0()
            .h(px(24.0))
            .px(px(9.0))
            .flex()
            .items_center()
            .gap(px(4.0))
            .rounded(px(12.0))
            .text_size(px(11.5))
            .whitespace_nowrap()
            .bg(if open {
                theme.surface_2
            } else {
                gpui::transparent_black()
            })
            .border_1()
            .border_color(theme.border)
            .text_color(if open { theme.text } else { theme.text_muted })
            .hover(|s| s.bg(theme.surface_2).text_color(theme.text))
            .cursor_pointer()
            .child(label)
    };
    let mut chips = div().flex().items_center().gap(px(6.0)).min_w_0();
    for agent in &space.agents {
        chips = chips.child(
            div()
                .flex()
                .items_center()
                .gap(px(5.0))
                .pl(px(3.0))
                .pr(px(8.0))
                .h(px(24.0))
                .rounded(px(12.0))
                .bg(theme.surface_1)
                .border_1()
                .border_color(theme.border)
                .child(agent_glyph(agent, 18.0, &theme))
                .child(
                    div()
                        .text_size(px(11.5))
                        .text_color(theme.text)
                        .child(format!("@{}", agent.handle)),
                )
                .child(
                    div()
                        .text_size(px(10.5))
                        .text_color(theme.text_faint)
                        .child(format!("· @{}", agent.owner_handle)),
                ),
        );
    }
    div()
        .w_full()
        .h(px(44.0))
        .flex()
        .items_center()
        .gap(px(10.0))
        .px(px(16.0))
        .border_b_1()
        .border_color(theme.border)
        .child(avatar(image, &person, 26.0, &theme))
        .child(
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .child(
                            div()
                                .text_size(px(13.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.text)
                                .child(format!("@{}", person.handle)),
                        )
                        .when_some(person.name.clone(), |el, name| {
                            el.child(
                                div()
                                    .text_size(px(12.0))
                                    .text_color(theme.text_muted)
                                    .child(name),
                            )
                        })
                        .when(space.pending, |el| {
                            el.child(
                                div()
                                    .px(px(6.0))
                                    .py(px(1.0))
                                    .rounded(px(6.0))
                                    .bg(theme.surface_2)
                                    .text_size(px(10.5))
                                    .text_color(theme.text_muted)
                                    .child("invited · not signed in yet"),
                            )
                        }),
                )
                .child(
                    div()
                        .text_size(px(10.5))
                        .text_color(theme.text_faint)
                        .child(lock),
                ),
        )
        .child(div().flex_1())
        .child(chips)
        .child(
            pill("add-agent", "+ Add agent".into(), add_agent_open, &theme).on_click(cx.listener(
                |this, _, _, cx| {
                    this.add_agent_open = !this.add_agent_open;
                    this.members_open = false;
                    cx.notify();
                },
            )),
        )
        .child(
            pill(
                "members",
                format!("Members · {}", 1 + space.people.len() + space.agents.len()),
                members_open,
                &theme,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.members_open = !this.members_open;
                this.add_agent_open = false;
                cx.notify();
            })),
        )
}

/// Under "+ Add agent": my projects not yet in the chat.
pub fn add_agent_picker(
    space: &Project,
    state: &Entity<AppState>,
    right: f32,
    cx: &mut Context<Shell>,
) -> Div {
    let theme = Theme::of(cx).clone();
    let s = state.read(cx);
    let chat_id = space.id.clone();
    let candidates: Vec<Project> = s
        .my_projects()
        .filter(|p| !space.agents.iter().any(|a| a.project_id == p.id))
        .cloned()
        .collect();
    let mut card = div()
        .w(px(260.0))
        .flex()
        .flex_col()
        .gap(px(2.0))
        .p(px(6.0))
        .rounded(px(PANEL_RADIUS))
        .bg(theme.card())
        .border_1()
        .border_color(theme.border_strong)
        .shadow_lg()
        .child(
            div()
                .px(px(8.0))
                .py(px(4.0))
                .text_size(px(11.0))
                .text_color(theme.text_faint)
                .child("ADD ONE OF YOUR AGENTS"),
        );
    if candidates.is_empty() {
        card = card.child(
            div()
                .px(px(8.0))
                .py(px(6.0))
                .text_size(px(12.0))
                .text_color(theme.text_muted)
                .child("Every project of yours is already here."),
        );
    }
    for p in candidates {
        let pid = p.id.clone();
        let chat = chat_id.clone();
        let color = project_color(&p.color, theme.accent);
        card = card.child(
            div()
                .id(ElementId::Name(SharedString::from(format!(
                    "add-agent-{}",
                    p.id
                ))))
                .flex()
                .items_center()
                .gap(px(8.0))
                .px(px(8.0))
                .py(px(6.0))
                .rounded(px(6.0))
                .cursor_pointer()
                .hover(|s| s.bg(theme.surface_2))
                .child(
                    div()
                        .size(px(22.0))
                        .rounded(px(6.0))
                        .bg(Theme::with_alpha(color, 0.18))
                        .border_1()
                        .border_color(Theme::with_alpha(color, 0.8))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(12.0))
                        .text_color(theme.text)
                        .child(p.icon.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .text_size(px(12.5))
                        .text_color(theme.text)
                        .child(p.name.clone()),
                )
                .child(
                    div()
                        .text_size(px(10.5))
                        .text_color(theme.text_faint)
                        .child(format!("@{}", Project::mention_slug(&p.name))),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.add_agent_open = false;
                    this.state
                        .update(cx, |s, cx| s.add_agent(chat.clone(), pid.clone(), cx));
                })),
        );
    }
    card = card.child(
        div()
            .px(px(8.0))
            .pt(px(4.0))
            .text_size(px(10.5))
            .text_color(theme.text_faint)
            .child("Mention it (@slug) to get an answer. Everyone here can ask it anything your project can do."),
    );
    div()
        .absolute()
        .top(px(crate::ui::theme::TAB_BAR_HEIGHT + 48.0))
        .right(px(right))
        .child(frosted(PANEL_RADIUS, MENU_BLUR, card))
}

/// Member list: people (Block / Remove), agents (owner handle, Remove when mine), Leave.
pub fn members_popover(
    space: &Project,
    state: &Entity<AppState>,
    right: f32,
    cx: &mut Context<Shell>,
) -> Div {
    let theme = Theme::of(cx).clone();
    let s = state.read(cx);
    let me = s.me_handle();
    let me_id = s.me_id();
    let chat_id = space.id.clone();
    let mut card = div()
        .w(px(300.0))
        .flex()
        .flex_col()
        .gap(px(2.0))
        .p(px(6.0))
        .rounded(px(PANEL_RADIUS))
        .bg(theme.card())
        .border_1()
        .border_color(theme.border_strong)
        .shadow_lg()
        .child(
            div()
                .px(px(8.0))
                .py(px(4.0))
                .text_size(px(11.0))
                .text_color(theme.text_faint)
                .child("MEMBERS"),
        );
    let action = |id: ElementId, label: &'static str, danger: bool, theme: &Theme| {
        div()
            .id(id)
            .px(px(7.0))
            .py(px(2.0))
            .rounded(px(5.0))
            .text_size(px(11.0))
            .text_color(if danger {
                theme.danger
            } else {
                theme.text_muted
            })
            .cursor_pointer()
            .hover(|s| s.bg(theme.surface_2))
            .child(label)
    };
    // Me first.
    card = card.child(
        div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .px(px(8.0))
            .py(px(5.0))
            .child(
                div()
                    .flex_1()
                    .text_size(px(12.5))
                    .text_color(theme.text)
                    .child(format!("@{me} (you)")),
            )
            .child(
                div()
                    .text_size(px(10.5))
                    .text_color(theme.text_faint)
                    .child("owner"),
            ),
    );
    for p in &space.people {
        let image = s.avatar(&p.avatar_url);
        let handle = p.handle.clone();
        let user_id = p.id.clone();
        let chat_a = chat_id.clone();
        let chat_b = chat_id.clone();
        card = card.child(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .px(px(8.0))
                .py(px(5.0))
                .child(avatar(image, p, 22.0, &theme))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(12.5))
                        .text_color(theme.text)
                        .child(format!("@{}", p.handle)),
                )
                .child(
                    action(
                        ElementId::Name(SharedString::from(format!("block-{}", p.id))),
                        "Block",
                        false,
                        &theme,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.members_open = false;
                        let h = handle.clone();
                        let c = chat_a.clone();
                        this.state.update(cx, |s, cx| {
                            s.block_user(h, cx);
                            s.leave_chat(c, cx);
                        });
                    })),
                )
                .child(
                    action(
                        ElementId::Name(SharedString::from(format!("remove-{}", p.id))),
                        "Remove",
                        true,
                        &theme,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.members_open = false;
                        let u = user_id.clone();
                        let c = chat_b.clone();
                        this.state.update(cx, |s, cx| s.remove_member(c, u, cx));
                    })),
                ),
        );
    }
    for a in &space.agents {
        let mine = a.owner_id == me_id || a.owner_handle == me;
        let pid = a.project_id.clone();
        let chat = chat_id.clone();
        card = card.child(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .px(px(8.0))
                .py(px(5.0))
                .child(agent_glyph(a, 22.0, &theme))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_size(px(12.5))
                                .text_color(theme.text)
                                .child(format!("{} · @{}", a.name, a.handle)),
                        )
                        .child(
                            div()
                                .text_size(px(10.5))
                                .text_color(theme.text_faint)
                                .child(format!(
                                    "owned by @{} · {}",
                                    a.owner_handle,
                                    match a.reply_policy {
                                        crate::model::ReplyPolicy::Mention =>
                                            "answers when mentioned",
                                        crate::model::ReplyPolicy::EveryMessage =>
                                            "answers every message",
                                    }
                                )),
                        ),
                )
                .when(mine, |el| {
                    el.child(
                        action(
                            ElementId::Name(SharedString::from(format!(
                                "remove-agent-{}",
                                a.project_id
                            ))),
                            "Remove",
                            true,
                            &theme,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.members_open = false;
                            let p = pid.clone();
                            let c = chat.clone();
                            this.state.update(cx, |s, cx| s.remove_agent(c, p, cx));
                        })),
                    )
                }),
        );
    }
    let chat_leave = chat_id.clone();
    card = card.child(div().h(px(1.0)).my(px(4.0)).bg(theme.border)).child(
        div()
            .flex()
            .items_center()
            .px(px(8.0))
            .py(px(4.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(10.5))
                    .text_color(theme.text_faint)
                    .child(if space.agents.is_empty() {
                        "Nobody's agent reads this chat."
                    } else {
                        "Agents answer with their owner's tools and credits; any member may approve their cards."
                    }),
            )
            .child(
                action(ElementId::Name("leave-chat".into()), "Leave", true, &theme).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.members_open = false;
                        let c = chat_leave.clone();
                        this.state.update(cx, |s, cx| s.leave_chat(c, cx));
                    }),
                ),
            ),
    );
    div()
        .absolute()
        .top(px(crate::ui::theme::TAB_BAR_HEIGHT + 48.0))
        .right(px(right))
        .child(frosted(PANEL_RADIUS, MENU_BLUR, card))
}

/// "@sam is typing…" above the composer.
pub fn typing_line(handle: &str, theme: &Theme) -> Div {
    div().w_full().flex().justify_center().pb(px(2.0)).child(
        div()
            .w(px(crate::ui::theme::COMPOSER_WIDTH))
            .max_w_full()
            .px(px(14.0))
            .text_size(px(11.0))
            .text_color(theme.text_faint)
            .child(format!("@{handle} is typing…")),
    )
}

/// Stacked avatars for a project with people in it (tile corner / header).
pub fn avatar_stack(
    people: &[(Person, Option<Arc<RenderImage>>)],
    size: f32,
    max: usize,
    theme: &Theme,
) -> Div {
    let mut row = div().flex().items_center();
    for (i, (p, image)) in people.iter().take(max).enumerate() {
        let image = image.clone();
        row = row.child(
            div()
                .when(i > 0, |el| el.ml(px(-size * 0.3)))
                .rounded_full()
                .border_1()
                .border_color(theme.bg)
                .child(avatar(image, p, size, theme)),
        );
    }
    if people.len() > max {
        row = row.child(
            div()
                .ml(px(2.0))
                .text_size(px(size * 0.5))
                .text_color(theme.text_faint)
                .child(format!("+{}", people.len() - max)),
        );
    }
    row
}

/// Tab-bar controls for a project: "shared by @owner" when I am a guest, the
/// member avatars, and "Invite" for the owner.
pub fn project_controls(
    project: &Project,
    state: &Entity<AppState>,
    members_open: bool,
    cx: &mut Context<Shell>,
) -> Div {
    let theme = Theme::of(cx).clone();
    let s = state.read(cx);
    let project_id = project.id.clone();
    let mut row = div().flex().items_center().gap(px(6.0)).flex_shrink_0();
    if project.is_guest() {
        let owner = project.owner().cloned();
        let label = format!(
            "shared by @{}",
            project
                .owner_handle
                .clone()
                .or_else(|| owner.as_ref().map(|o| o.handle.clone()))
                .unwrap_or_else(|| "owner".into())
        );
        row = row.child(
            div()
                .flex()
                .items_center()
                .gap(px(5.0))
                .h(px(24.0))
                .pl(px(3.0))
                .pr(px(8.0))
                .rounded(px(12.0))
                .bg(theme.surface_1)
                .border_1()
                .border_color(theme.border)
                .when_some(owner, |el, o| {
                    el.child(avatar(s.avatar(&o.avatar_url), &o, 18.0, &theme))
                })
                .child(
                    div()
                        .text_size(px(11.0))
                        .whitespace_nowrap()
                        .text_color(theme.text_muted)
                        .child(label),
                ),
        );
    }
    let guests: Vec<Person> = project.guests().cloned().collect();
    if !guests.is_empty() || project.is_guest() {
        let shown: Vec<Person> = if project.is_guest() {
            project.people.clone()
        } else {
            guests.clone()
        };
        row = row.child(
            div()
                .id("project-members")
                .flex()
                .items_center()
                .gap(px(6.0))
                .h(px(24.0))
                .px(px(6.0))
                .rounded(px(12.0))
                .cursor_pointer()
                .bg(if members_open {
                    theme.surface_2
                } else {
                    gpui::transparent_black()
                })
                .hover(|st| st.bg(theme.surface_2))
                .child(avatar_stack(
                    &shown
                        .iter()
                        .map(|p| (p.clone(), s.avatar(&p.avatar_url)))
                        .collect::<Vec<_>>(),
                    20.0,
                    3,
                    &theme,
                ))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.members_open = !this.members_open;
                    this.add_agent_open = false;
                    cx.notify();
                })),
        );
    }
    if !project.is_guest() && crate::ui::avatars::available() {
        let slug = s.avatar_slug(project);
        let st = s.avatar_state(&project.id);
        let pid = project.id.clone();
        let picker_open = s.avatar_picker.as_deref() == Some(project.id.as_str());
        row = row.child(
            div()
                .id("project-avatar")
                .flex_shrink_0()
                .h(px(26.0))
                .px(px(3.0))
                .flex()
                .items_center()
                .rounded(px(13.0))
                .bg(if picker_open {
                    theme.surface_2
                } else {
                    gpui::transparent_black()
                })
                .hover(|st| st.bg(theme.surface_2))
                .cursor_pointer()
                .child(crate::ui::avatars::element(
                    slug.as_deref(),
                    st,
                    22.0,
                    &project.icon,
                    project_color(&project.color, theme.accent),
                    &theme,
                ))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.members_open = false;
                    this.add_agent_open = false;
                    let p = pid.clone();
                    this.state.update(cx, |s, cx| s.toggle_avatar_picker(p, cx));
                })),
        );
    }
    if !project.is_guest() {
        row = row.child(
            div()
                .id("project-invite")
                .flex_shrink_0()
                .h(px(24.0))
                .px(px(8.0))
                .flex()
                .items_center()
                .rounded(px(6.0))
                .text_size(px(11.5))
                .whitespace_nowrap()
                .text_color(theme.text_muted)
                .hover(|st| st.bg(theme.surface_2).text_color(theme.text))
                .cursor_pointer()
                .child("Invite")
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.new_menu_open = false;
                    this.people_input.update(cx, |input, cx| input.clear(cx));
                    let id = project_id.clone();
                    this.state.update(cx, |s, cx| s.open_invite(id, cx));
                    let handle = this.people_input.read(cx).focus_handle(cx);
                    window.focus(&handle, cx);
                })),
        );
    }
    row
}

/// Members of a project: the owner, the invited people (Remove for the owner).
pub fn project_members_popover(
    project: &Project,
    state: &Entity<AppState>,
    right: f32,
    cx: &mut Context<Shell>,
) -> Div {
    let theme = Theme::of(cx).clone();
    let s = state.read(cx);
    let me = s.me_handle();
    let project_id = project.id.clone();
    let mut card = div()
        .w(px(280.0))
        .flex()
        .flex_col()
        .gap(px(2.0))
        .p(px(6.0))
        .rounded(px(PANEL_RADIUS))
        .bg(theme.card())
        .border_1()
        .border_color(theme.border_strong)
        .shadow_lg()
        .child(
            div()
                .px(px(8.0))
                .py(px(4.0))
                .text_size(px(11.0))
                .text_color(theme.text_faint)
                .child(format!("PEOPLE IN {}", project.name.to_uppercase())),
        );
    if !project.is_guest() {
        card = card.child(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .px(px(8.0))
                .py(px(5.0))
                .child(
                    div()
                        .flex_1()
                        .text_size(px(12.5))
                        .text_color(theme.text)
                        .child(format!("@{me} (you)")),
                )
                .child(
                    div()
                        .text_size(px(10.5))
                        .text_color(theme.text_faint)
                        .child("owner"),
                ),
        );
    }
    for p in &project.people {
        let image = s.avatar(&p.avatar_url);
        let user_id = p.id.clone();
        let pid = project_id.clone();
        let is_owner = p.role == "owner";
        card = card.child(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .px(px(8.0))
                .py(px(5.0))
                .child(avatar(image, p, 22.0, &theme))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(12.5))
                        .text_color(theme.text)
                        .child(format!("@{}", p.handle)),
                )
                .child(
                    div()
                        .text_size(px(10.5))
                        .text_color(theme.text_faint)
                        .child(if is_owner { "owner" } else { "member" }),
                )
                .when(!project.is_guest() && !is_owner, |el| {
                    el.child(
                        div()
                            .id(ElementId::Name(SharedString::from(format!(
                                "remove-member-{}",
                                p.id
                            ))))
                            .px(px(7.0))
                            .py(px(2.0))
                            .rounded(px(5.0))
                            .text_size(px(11.0))
                            .text_color(theme.danger)
                            .cursor_pointer()
                            .hover(|st| st.bg(theme.surface_2))
                            .child("Remove")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.members_open = false;
                                let u = user_id.clone();
                                let p = pid.clone();
                                this.state
                                    .update(cx, |s, cx| s.remove_project_member(p, u, cx));
                            })),
                    )
                }),
        );
    }
    card = card.child(
        div()
            .px(px(8.0))
            .pt(px(4.0))
            .text_size(px(10.5))
            .text_color(theme.text_faint)
            .child(if project.is_guest() {
                "You can chat and drive the agent here; settings and members stay with the owner."
            } else {
                "Members see every tab and the workspace output, and the agent answers them as it answers you."
            }),
    );
    div()
        .absolute()
        .top(px(crate::ui::theme::TAB_BAR_HEIGHT + 6.0))
        .right(px(right))
        .child(frosted(PANEL_RADIUS, MENU_BLUR, card))
}

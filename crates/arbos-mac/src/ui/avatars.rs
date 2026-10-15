//! Tree-character avatars for agents (assets/avatars/trees). The characters
//! ship as pre-rendered PNG frame strips per animation state; GPUI's
//! multi-frame `RenderImage` plays them (and honours reduced motion by
//! holding the first frame), so no Lottie runtime is needed in the app.
//!
//! Every project gets a character on creation: a hash of its id picks one,
//! skipping characters already used in this user's rail. The owner can change
//! it in the Avatar picker (`PATCH /projects/{id} {avatar}`).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use gpui::{div, prelude::*, px, Div, Hsla, RenderImage};
use image::Delay;
use serde::Deserialize;

use gpui::{Context, ElementId, Entity, SharedString};

use crate::app::AppState;
use crate::model::{Id, Project};
use crate::ui::frost::{frosted, MENU_BLUR};
use crate::ui::shell::Shell;
use crate::ui::theme::{Theme, PANEL_RADIUS, TAB_BAR_HEIGHT};

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/tree_avatars.rs"));
}

/// What the character is doing (chosen from the agent's status).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AvatarState {
    Idle,
    Working,
    Reply,
}

impl AvatarState {
    fn key(self) -> &'static str {
        match self {
            AvatarState::Idle => "idle",
            AvatarState::Working => "working",
            AvatarState::Reply => "reply",
        }
    }
}

/// One character from manifest.json.
#[derive(Clone, Debug)]
pub struct Character {
    pub slug: String,
    pub name: String,
    /// `ready: false` in the manifest = placeholder art, files may be missing.
    pub ready: bool,
    /// frames per second per state (default 30).
    fps: HashMap<String, f32>,
}

#[derive(Deserialize)]
struct Manifest {
    #[serde(default)]
    characters: Vec<ManifestCharacter>,
}

#[derive(Deserialize)]
struct ManifestCharacter {
    slug: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default = "yes")]
    ready: bool,
    #[serde(default)]
    frames: HashMap<String, StateMeta>,
    #[serde(default)]
    fps: Option<f32>,
}

fn yes() -> bool {
    true
}

#[derive(Deserialize)]
struct StateMeta {
    #[serde(default)]
    fps: Option<f32>,
}

fn characters() -> &'static [Character] {
    static CHARS: OnceLock<Vec<Character>> = OnceLock::new();
    CHARS.get_or_init(|| {
        let parsed: Option<Manifest> = serde_json::from_str(embedded::MANIFEST_JSON).ok();
        let mut out: Vec<Character> = parsed
            .map(|m| {
                m.characters
                    .into_iter()
                    .map(|c| Character {
                        name: c.name.clone().unwrap_or_else(|| title(&c.slug)),
                        ready: c.ready,
                        fps: c
                            .frames
                            .iter()
                            .map(|(k, v)| (k.clone(), v.fps.or(c.fps).unwrap_or(30.0)))
                            .collect(),
                        slug: c.slug,
                    })
                    .collect()
            })
            .unwrap_or_default();
        // Strips without a manifest entry still count as characters.
        let mut seen: HashSet<String> = out.iter().map(|c| c.slug.clone()).collect();
        for (slug, _, _, _, _) in embedded::STRIPS {
            if seen.insert((*slug).to_string()) {
                out.push(Character {
                    slug: (*slug).to_string(),
                    name: title(slug),
                    ready: true,
                    fps: HashMap::new(),
                });
            }
        }
        // Manifest order is a contract (the relay picks slugs[fnv1a64(id) % n]); do not sort.
        out
    })
}

fn title(slug: &str) -> String {
    slug.split(['-', '_'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Every character, for the picker.
pub fn all() -> &'static [Character] {
    characters()
}

/// Anything to show or pick at all (manifest or strips present).
pub fn available() -> bool {
    !characters().is_empty()
}

/// FNV-1a 64 over the UTF-8 bytes — the relay's `avatar.rs` arithmetic.
pub fn fnv1a64(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// The relay's default: `slugs[fnv1a64(project_id) % n]` in manifest order.
/// Only when the relay sent no avatar at all does the client walk forward to
/// the next character not already used in this rail (the relay itself does
/// not dedupe, so a relay-sent slug is always shown as is).
pub fn assign(project_id: &str, taken: &HashSet<String>) -> Option<String> {
    let chars = characters();
    if chars.is_empty() {
        return None;
    }
    let start = (fnv1a64(project_id) % chars.len() as u64) as usize;
    for i in 0..chars.len() {
        let c = &chars[(start + i) % chars.len()];
        if !taken.contains(&c.slug) {
            return Some(c.slug.clone());
        }
    }
    Some(chars[start].slug.clone())
}

/// The character a project shows: its own choice, else the hash pick (the
/// rest of this user's rail counts as taken so auto picks stay distinct).
pub fn slug_for(project: &Project, all_projects: &[Project]) -> Option<String> {
    if let Some(slug) = project.avatar.as_deref().filter(|s| !s.is_empty()) {
        return Some(slug.to_string());
    }
    let taken: HashSet<String> = all_projects
        .iter()
        .filter(|p| p.id != project.id && !p.is_people())
        .filter_map(|p| p.avatar.clone())
        .collect();
    assign(&project.id, &taken)
}

type Key = (String, &'static str, bool, u32);

fn cache() -> &'static Mutex<HashMap<Key, Option<Arc<RenderImage>>>> {
    static CACHE: OnceLock<Mutex<HashMap<Key, Option<Arc<RenderImage>>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The decoded strip for (slug, state, rail size, scale), falling back to the
/// other scale, then to the idle strip, then to the non-rail size.
pub fn image(slug: &str, state: AvatarState, rail: bool) -> Option<Arc<RenderImage>> {
    // Chat sizes draw the 56 px strip, rail tiles the 72 px one (see build.rs).
    let scale = if rail { 1u32 } else { 2u32 };
    let key: Key = (slug.to_string(), state.key(), rail, scale);
    if let Some(hit) = cache().lock().unwrap().get(&key) {
        return hit.clone();
    }
    let fps = characters()
        .iter()
        .find(|c| c.slug == slug)
        .and_then(|c| c.fps.get(state.key()).copied())
        .unwrap_or(30.0);
    let other = if rail { 2u32 } else { 1u32 };
    let candidates: [(&str, bool, u32); 6] = [
        (state.key(), rail, scale),
        (state.key(), rail, other),
        ("idle", rail, scale),
        ("idle", rail, other),
        (state.key(), !rail, if rail { 2 } else { 1 }),
        ("idle", !rail, if rail { 2 } else { 1 }),
    ];
    let mut decoded = None;
    for (st, r, sc) in candidates {
        if let Some((_, _, _, _, bytes)) = embedded::STRIPS
            .iter()
            .find(|(s, k, rr, ss, _)| *s == slug && *k == st && *rr == r && *ss == sc)
        {
            decoded = decode_strip(bytes, fps).map(Arc::new);
            if decoded.is_some() {
                break;
            }
        }
    }
    cache().lock().unwrap().insert(key, decoded.clone());
    decoded
}

/// A horizontal strip of square frames → BGRA frames with a uniform delay.
fn decode_strip(bytes: &[u8], fps: f32) -> Option<RenderImage> {
    let img = image::load_from_memory(bytes).ok()?.into_rgba8();
    let (w, h) = img.dimensions();
    if h == 0 || w < h {
        return None;
    }
    let n = (w / h).max(1);
    let delay = Delay::from_numer_denom_ms(1000, fps.max(1.0) as u32);
    let mut frames = Vec::with_capacity(n as usize);
    for i in 0..n {
        let sub = image::imageops::crop_imm(&img, i * h, 0, h, h).to_image();
        let mut raw = sub.into_raw();
        for px in raw.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        let buffer = image::RgbaImage::from_raw(h, h, raw)?;
        frames.push(image::Frame::from_parts(buffer, 0, 0, delay));
    }
    Some(RenderImage::new(frames))
}

/// Length of one pass of the reply reaction, for the "play once" window.
pub fn reply_duration_ms(slug: &str) -> u64 {
    image(slug, AvatarState::Reply, false)
        .map(|img| {
            let n = img.frame_count() as u64;
            let fps = characters()
                .iter()
                .find(|c| c.slug == slug)
                .and_then(|c| c.fps.get("reply").copied())
                .unwrap_or(30.0);
            (n * 1000) / fps.max(1.0) as u64
        })
        .unwrap_or(1000)
}

/// The avatar element: the animated character when assets exist, otherwise
/// the project's glyph in its colour circle (the pre-avatar look).
pub fn element(
    slug: Option<&str>,
    state: AvatarState,
    size: f32,
    fallback_glyph: &str,
    color: Hsla,
    theme: &Theme,
) -> Div {
    let rail = size >= 48.0;
    if let Some(img) = slug.and_then(|s| image(s, state, rail)) {
        return div()
            .size(px(size))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .child(
                gpui::img(img)
                    .size(px(size))
                    .object_fit(gpui::ObjectFit::Contain),
            );
    }
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
        .text_size(px(size * 0.55))
        .text_color(theme.text)
        .child(fallback_glyph.to_owned())
}

/// Settings → Avatar: every character in a grid; the current one is ringed.
pub fn picker(
    project: &Project,
    state: &Entity<AppState>,
    right: f32,
    cx: &mut Context<Shell>,
) -> Div {
    let theme = Theme::of(cx).clone();
    let s = state.read(cx);
    let current = s.avatar_slug(project);
    let project_id: Id = project.id.clone();
    let mut grid = div()
        .flex()
        .flex_wrap()
        .gap(px(8.0))
        .max_w(px(5.0 * 80.0 + 4.0 * 8.0));
    for c in all() {
        let slug = c.slug.clone();
        let selected = current.as_deref() == Some(c.slug.as_str());
        let pid = project_id.clone();
        grid = grid.child(
            div()
                .id(ElementId::Name(SharedString::from(format!(
                    "avatar-pick-{}",
                    c.slug
                ))))
                .w(px(80.0))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(4.0))
                .p(px(6.0))
                .when(!c.ready, |el| el.opacity(0.6))
                .rounded(px(10.0))
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
                .hover(|st| st.bg(theme.surface_2))
                .child(element(
                    Some(&c.slug),
                    AvatarState::Idle,
                    44.0,
                    "·",
                    theme.accent,
                    &theme,
                ))
                .child(
                    div()
                        .text_size(px(10.5))
                        .text_color(if selected {
                            theme.text
                        } else if c.ready {
                            theme.text_muted
                        } else {
                            theme.text_faint
                        })
                        .max_w(px(68.0))
                        .truncate()
                        .child(c.name.clone()),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    let p = pid.clone();
                    let sl = slug.clone();
                    this.state.update(cx, |st, cx| st.set_avatar(p, sl, cx));
                })),
        );
    }
    let card = div()
        .flex()
        .flex_col()
        .gap(px(6.0))
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
                .child(format!("AVATAR · {}", project.name.to_uppercase())),
        )
        .child(grid)
        .child(
            div()
                .text_size(px(10.5))
                .text_color(theme.text_faint)
                .child("Picked for you from the project id; choose any tree to make it yours."),
        );
    div()
        .absolute()
        .top(px(TAB_BAR_HEIGHT + 6.0))
        .right(px(right))
        .child(frosted(PANEL_RADIUS, MENU_BLUR, card))
}

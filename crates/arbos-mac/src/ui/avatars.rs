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
    /// mono + accents: `{ slug, name, bg, ring, tree, face }` hex colours.
    #[serde(default)]
    colors: Vec<ManifestColor>,
    /// The colour a project without `avatar_color` shows (default "mono").
    #[serde(default)]
    default_color: Option<String>,
}

#[derive(Deserialize)]
struct ManifestColor {
    #[serde(default, alias = "slug", alias = "key")]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    bg: Option<String>,
    #[serde(default)]
    ring: Option<String>,
    #[serde(default)]
    ring_opacity: Option<f32>,
    #[serde(default)]
    tree: Option<String>,
    #[serde(default)]
    tree_dark: Option<String>,
    #[serde(default)]
    tree_light: Option<String>,
    #[serde(default)]
    face: Option<String>,
}

/// One avatar colour (manifest `colors[]`). The tree carries the colour; the
/// disc stays neutral black with a thin neutral ring. The `-tree` strips are
/// grayscale levels — base 255, light 220, dark 170, face 0 — that
/// [`decode_strip`] maps onto `tree` / `tree_light` / `tree_dark` / `face`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Palette {
    pub id: String,
    pub name: String,
    pub bg: [u8; 3],
    pub ring: [u8; 3],
    /// Ring alpha, 0–255 (manifest `ring_opacity`, default opaque).
    pub ring_alpha: u8,
    pub tree: [u8; 3],
    pub tree_dark: [u8; 3],
    pub tree_light: [u8; 3],
    pub face: [u8; 3],
    /// The manifest's `default_color`: drawn from the shipped disc strips as is.
    pub is_default: bool,
}

/// Grayscale level → palette hue, in level order (see [`Palette`]).
const LEVELS: [u8; 4] = [0, 170, 220, 255];

impl Palette {
    /// Black disc, white ring and tree: the look without any manifest colours.
    pub fn mono() -> Palette {
        Palette {
            id: "mono".into(),
            name: "Mono".into(),
            bg: [0, 0, 0],
            ring: [255, 255, 255],
            ring_alpha: 255,
            tree: [255, 255, 255],
            tree_dark: [170, 170, 170],
            tree_light: [220, 220, 220],
            face: [0, 0, 0],
            is_default: true,
        }
    }

    /// The hue for one strip pixel: its gray level, interpolated between the
    /// two nearest encoded levels so anti-aliased edges stay smooth.
    fn ink(&self, level: u8) -> [u8; 3] {
        let hues = [self.face, self.tree_dark, self.tree_light, self.tree];
        let hi = LEVELS.iter().position(|l| level <= *l).unwrap_or(3);
        if hi == 0 || LEVELS[hi] == level {
            return hues[hi];
        }
        let (l0, l1) = (LEVELS[hi - 1] as u32, LEVELS[hi] as u32);
        let t = level as u32 - l0;
        let span = l1 - l0;
        let mut out = [0u8; 3];
        for i in 0..3 {
            let a = hues[hi - 1][i] as u32;
            let b = hues[hi][i] as u32;
            out[i] = ((a * (span - t) + b * t + span / 2) / span) as u8;
        }
        out
    }

    pub fn is_mono(&self) -> bool {
        self.is_default
    }

    pub fn bg_hsla(&self) -> Hsla {
        rgb_hsla(self.bg)
    }

    pub fn ring_hsla(&self) -> Hsla {
        let mut c = rgb_hsla(self.ring);
        c.a = self.ring_alpha as f32 / 255.0;
        c
    }

    pub fn tree_hsla(&self) -> Hsla {
        rgb_hsla(self.tree)
    }
}

/// `rgb` scaled by `level / 255` — the shade a gray level would show in mono.
fn scale_rgb(rgb: [u8; 3], level: u8) -> [u8; 3] {
    rgb.map(|v| ((v as u32 * level as u32 + 127) / 255) as u8)
}

fn rgb_hsla(c: [u8; 3]) -> Hsla {
    gpui::rgb(((c[0] as u32) << 16) | ((c[1] as u32) << 8) | c[2] as u32).into()
}

/// `#rrggbb` / `rrggbb` / `#rgb` → bytes.
pub fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let h = s.trim().trim_start_matches('#');
    let h = match h.len() {
        3 => h.chars().flat_map(|c| [c, c]).collect::<String>(),
        6 => h.to_string(),
        _ => return None,
    };
    let v = u32::from_str_radix(&h, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

fn palettes() -> &'static [Palette] {
    static COLORS: OnceLock<Vec<Palette>> = OnceLock::new();
    COLORS.get_or_init(|| {
        let parsed: Option<Manifest> = serde_json::from_str(embedded::MANIFEST_JSON).ok();
        let (colors, default) = parsed
            .map(|m| (m.colors, m.default_color))
            .unwrap_or_default();
        let mut out: Vec<Palette> = Vec::new();
        for (i, c) in colors.into_iter().enumerate() {
            let mono = Palette::mono();
            let id =
                c.id.or_else(|| c.name.clone().map(|n| n.to_lowercase().replace(' ', "-")))
                    .unwrap_or_else(|| {
                        if i == 0 {
                            "mono".into()
                        } else {
                            format!("c{i}")
                        }
                    });
            let tree = c.tree.as_deref().and_then(parse_hex).unwrap_or(mono.tree);
            out.push(Palette {
                name: c.name.unwrap_or_else(|| title(&id)),
                bg: c.bg.as_deref().and_then(parse_hex).unwrap_or(mono.bg),
                ring: c.ring.as_deref().and_then(parse_hex).unwrap_or(mono.ring),
                ring_alpha: c
                    .ring_opacity
                    .map(|o| (o.clamp(0.0, 1.0) * 255.0).round() as u8)
                    .unwrap_or(255),
                tree,
                tree_dark: c
                    .tree_dark
                    .as_deref()
                    .and_then(parse_hex)
                    .unwrap_or_else(|| scale_rgb(tree, 170)),
                tree_light: c
                    .tree_light
                    .as_deref()
                    .and_then(parse_hex)
                    .unwrap_or_else(|| scale_rgb(tree, 220)),
                face: c.face.as_deref().and_then(parse_hex).unwrap_or(mono.face),
                is_default: match &default {
                    Some(d) => d.eq_ignore_ascii_case(&id),
                    None => i == 0,
                },
                id,
            });
        }
        if out.is_empty() {
            out.push(Palette::mono());
        }
        // The default comes first so `palettes()[0]` is always the fallback.
        match out.iter().position(|p| p.is_default) {
            Some(i) if i != 0 => {
                let d = out.remove(i);
                out.insert(0, d);
            }
            Some(_) => {}
            None => out[0].is_default = true,
        }
        out
    })
}

/// Every colour, for the picker (manifest order; the first is the default).
pub fn colors() -> &'static [Palette] {
    palettes()
}

/// Colours exist to choose from (more than the mono default).
pub fn colors_available() -> bool {
    palettes().len() > 1 && embedded::STRIPS.iter().any(|(_, _, _, _, tree, _)| *tree)
}

/// The palette for a project's `avatar_color`: a manifest id, else a raw hex
/// accent (tree + ring in that colour on the mono disc), else mono.
pub fn palette_for(color: Option<&str>) -> Palette {
    let Some(c) = color.map(str::trim).filter(|c| !c.is_empty()) else {
        return palettes()[0].clone();
    };
    if let Some(p) = palettes().iter().find(|p| p.id.eq_ignore_ascii_case(c)) {
        return p.clone();
    }
    match parse_hex(c) {
        Some(rgb) => Palette {
            id: c.to_string(),
            name: c.to_string(),
            bg: palettes()[0].bg,
            ring: palettes()[0].ring,
            ring_alpha: palettes()[0].ring_alpha,
            tree: rgb,
            tree_dark: scale_rgb(rgb, 170),
            tree_light: scale_rgb(rgb, 220),
            face: palettes()[0].face,
            is_default: false,
        },
        None => palettes()[0].clone(),
    }
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
        for (slug, _, _, _, _, _) in embedded::STRIPS {
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

/// (slug, state, rail, scale, colour id or "" for the shipped disc strip)
type Key = (String, &'static str, bool, u32, String);

fn cache() -> &'static Mutex<HashMap<Key, Option<Arc<RenderImage>>>> {
    static CACHE: OnceLock<Mutex<HashMap<Key, Option<Arc<RenderImage>>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The decoded strip for (slug, state, rail size, scale), falling back to the
/// other scale, then to the idle strip, then to the non-rail size.
pub fn image(slug: &str, state: AvatarState, rail: bool) -> Option<Arc<RenderImage>> {
    image_tinted(slug, state, rail, None)
}

/// Like [`image`], in a colour: the tree-only strip (white on transparent)
/// multiplied by the palette's tree ink, cached per (slug, state, colour).
/// Mono, or a character without a tree-only strip, draws the shipped disc.
pub fn image_tinted(
    slug: &str,
    state: AvatarState,
    rail: bool,
    palette: Option<&Palette>,
) -> Option<Arc<RenderImage>> {
    // Chat sizes draw the 56 px strip, rail tiles the 72 px one (see build.rs).
    let scale = if rail { 1u32 } else { 2u32 };
    let tint = palette.filter(|p| !p.is_mono() && has_tree_strip(slug));
    let color_key = tint.map(|p| p.id.clone()).unwrap_or_default();
    let key: Key = (slug.to_string(), state.key(), rail, scale, color_key);
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
    let want_tree = tint.is_some();
    let mut decoded = None;
    for (st, r, sc) in candidates {
        if let Some((_, _, _, _, _, bytes)) =
            embedded::STRIPS.iter().find(|(s, k, rr, ss, t, _)| {
                *s == slug && *k == st && *rr == r && *ss == sc && *t == want_tree
            })
        {
            decoded = decode_strip(bytes, fps, tint).map(Arc::new);
            if decoded.is_some() {
                break;
            }
        }
    }
    cache().lock().unwrap().insert(key, decoded.clone());
    decoded
}

/// The character ships a tree-only strip (needed to recolour it).
fn has_tree_strip(slug: &str) -> bool {
    embedded::STRIPS
        .iter()
        .any(|(s, _, _, _, tree, _)| *s == slug && *tree)
}

/// Does this project draw its own disc + ring (coloured tree) rather than
/// the shipped disc strip?
pub fn draws_disc(slug: &str, palette: &Palette) -> bool {
    !palette.is_mono() && has_tree_strip(slug)
}

/// A horizontal strip of square frames → BGRA frames with a uniform delay.
/// With `tint`, every pixel's gray level picks its hue from the palette (the
/// `-tree` strips are gray levels on transparent; alpha is kept as is).
fn decode_strip(bytes: &[u8], fps: f32, tint: Option<&Palette>) -> Option<RenderImage> {
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
            if let Some(p) = tint {
                if px[3] > 0 {
                    let level = px[0].max(px[1]).max(px[2]);
                    px[..3].copy_from_slice(&p.ink(level));
                }
            }
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
/// the project's glyph in its colour circle (the pre-avatar look). A coloured
/// avatar (`avatar_color`; `None` = mono) is a GPUI disc (`bg`, neutral
/// black) with a thin ring under the level-mapped tree; mono draws the
/// shipped strip as is.
#[allow(clippy::too_many_arguments)]
pub fn element(
    slug: Option<&str>,
    avatar_color: Option<&str>,
    state: AvatarState,
    size: f32,
    fallback_glyph: &str,
    color: Hsla,
    theme: &Theme,
) -> Div {
    let rail = size >= 48.0;
    let palette = palette_for(avatar_color);
    if let Some(slug) = slug {
        if let Some(img) = image_tinted(slug, state, rail, Some(&palette)) {
            let disc = draws_disc(slug, &palette);
            // The art's ring is a 2 px stroke at the 56/72 px strip sizes;
            // match that weight when we draw it ourselves.
            let ring = (size / 20.0).clamp(1.0, 2.0);
            return div()
                .size(px(size))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .when(disc, |el| {
                    el.rounded_full()
                        .bg(palette.bg_hsla())
                        .border(px(ring))
                        .border_color(palette.ring_hsla())
                })
                .child(
                    gpui::img(img)
                        .size(px(if disc { size - 2.0 * ring } else { size }))
                        .object_fit(gpui::ObjectFit::Contain),
                );
        }
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

/// One colour swatch: the neutral disc and ring with a dot of tree ink.
fn swatch(p: &Palette, size: f32, selected: bool, theme: &Theme) -> Div {
    div()
        .size(px(size))
        .flex_shrink_0()
        .rounded_full()
        .bg(p.bg_hsla())
        .border(px(if selected { 2.0 } else { 1.0 }))
        .border_color(if selected {
            theme.accent
        } else {
            p.ring_hsla()
        })
        .flex()
        .items_center()
        .justify_center()
        .child(div().size(px(size * 0.5)).rounded_full().bg(p.tree_hsla()))
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
    let current_color = s.avatar_color(project);
    let palette = palette_for(current_color.as_deref());
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
                    current_color.as_deref(),
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
    // Live preview (the chosen tree, animated, in the chosen colour) and the
    // colour row, when the assets ship colours and tree-only strips.
    let colour_row = colors_available().then(|| {
        let mut row = div().flex().flex_wrap().items_center().gap(px(6.0));
        for p in colors() {
            let selected = p.id == palette.id;
            let pid = project_id.clone();
            let id = p.id.clone();
            row = row.child(
                div()
                    .id(ElementId::Name(SharedString::from(format!(
                        "avatar-color-{}",
                        p.id
                    ))))
                    .p(px(2.0))
                    .rounded_full()
                    .cursor_pointer()
                    .hover(|st| st.bg(theme.surface_2))
                    .child(swatch(p, 22.0, selected, &theme))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let p = pid.clone();
                        let c = id.clone();
                        this.state
                            .update(cx, |st, cx| st.set_avatar_color(p, c, cx));
                    })),
            );
        }
        div()
            .flex()
            .items_center()
            .gap(px(12.0))
            .pb(px(4.0))
            .child(element(
                current.as_deref(),
                current_color.as_deref(),
                s.avatar_state(&project.id),
                56.0,
                &project.icon,
                theme.accent,
                &theme,
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme.text_muted)
                            .child(format!(
                                "{} · {}",
                                current
                                    .as_deref()
                                    .and_then(|sl| all().iter().find(|c| c.slug == sl))
                                    .map(|c| c.name.clone())
                                    .unwrap_or_else(|| "Tree".to_string()),
                                palette.name
                            )),
                    )
                    .child(row),
            )
    });
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
        .children(colour_row)
        .child(grid)
        .child(
            div()
                .text_size(px(10.5))
                .text_color(theme.text_faint)
                .child(if colors_available() {
                    "Picked for you from the project id; choose any tree and colour to make it yours."
                } else {
                    "Picked for you from the project id; choose any tree to make it yours."
                }),
        );
    div()
        .absolute()
        .top(px(TAB_BAR_HEIGHT + 6.0))
        .right(px(right))
        .child(frosted(PANEL_RADIUS, MENU_BLUR, card))
}

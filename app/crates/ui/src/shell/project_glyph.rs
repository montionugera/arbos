//! Every project wears an icon. With no choice made, a project gets a
//! built-in glyph in a palette color, both picked from its id so the same
//! project always looks the same. The person can swap the glyph or the
//! color from the rail's context menu, or use an emoji, an image, or the
//! repository's own artwork instead.
use gpui::{AnyElement, Hsla, div, prelude::*, px};

use crate::icons;
use crate::theme::Theme;

/// The built-in symbols, by stored name. The order is the picker's order.
pub(crate) const GLYPHS: [(&str, &str); 12] = [
    ("folder", icons::FOLDER),
    ("code", icons::FILE_CODE),
    ("terminal", icons::TERMINAL),
    ("globe", icons::GLOBE),
    ("bot", icons::BOT),
    ("star", icons::STAR),
    ("home", icons::HOME),
    ("cloud", icons::CLOUD),
    ("phone", icons::SMARTPHONE),
    ("gallery", icons::GALLERY),
    ("chat", icons::CHAT_ROUND_LINE),
    ("branch", icons::GIT_BRANCH),
];

/// The palette, by stored name: (dark appearance, light appearance). The
/// order is the picker's order; keep it stable so defaults do not drift.
pub(crate) const COLORS: [(&str, u32, u32); 8] = [
    ("slate", 0x94a3b8, 0x475569),
    ("blue", 0x93c5fd, 0x2563eb),
    ("violet", 0xc4b5fd, 0x7c3aed),
    ("rose", 0xfda4af, 0xbe123c),
    ("amber", 0xfcd34d, 0xa16207),
    ("emerald", 0x6ee7b7, 0x047857),
    ("teal", 0x5eead4, 0x0f766e),
    ("orange", 0xfdba74, 0xc2410c),
];

/// What a project's icon is, as stored in its `project.toml`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProjectIcon {
    /// A built-in symbol in a palette color.
    Glyph {
        glyph: &'static str,
        color: &'static str,
    },
    /// One emoji.
    Emoji(String),
    /// An image file on disk.
    Image(String),
    /// The repository's own artwork (favicon, app icon).
    Repo,
}

impl ProjectIcon {
    /// Read a stored value. A glyph or color the app does not know, or any
    /// other unknown form, falls back to the project's default.
    pub(crate) fn parse(stored: &str, seed: &str) -> Self {
        let stored = stored.trim();
        if let Some(emoji) = stored.strip_prefix("emoji:") {
            let emoji = emoji.trim();
            if !emoji.is_empty() {
                return Self::Emoji(emoji.to_owned());
            }
        } else if let Some(path) = stored.strip_prefix("image:") {
            let path = path.trim();
            if !path.is_empty() {
                return Self::Image(path.to_owned());
            }
        } else if stored == "repo" {
            return Self::Repo;
        } else if let Some(rest) = stored.strip_prefix("glyph:")
            && let Some((glyph, color)) = rest.split_once(':')
            && let Some(glyph) = GLYPHS
                .iter()
                .map(|(name, _)| *name)
                .find(|name| *name == glyph)
            && let Some(color) = COLORS
                .iter()
                .map(|(name, ..)| *name)
                .find(|name| *name == color)
        {
            return Self::Glyph { glyph, color };
        }
        Self::default_for(seed)
    }

    /// The icon a project wears until a choice is made: a glyph and a color
    /// chosen from `seed` (the project's id), so it never changes on its own.
    pub(crate) fn default_for(seed: &str) -> Self {
        let hash = fnv1a(seed);
        // Two independent picks from one hash: the low bits choose the
        // color, the high bits the glyph.
        let color = COLORS[hash as usize % COLORS.len()].0;
        let glyph = GLYPHS[(hash >> 16) as usize % GLYPHS.len()].0;
        Self::Glyph { glyph, color }
    }

    /// The value to store in `project.toml`.
    pub(crate) fn stored(&self) -> String {
        match self {
            Self::Glyph { glyph, color } => format!("glyph:{glyph}:{color}"),
            Self::Emoji(emoji) => format!("emoji:{emoji}"),
            Self::Image(path) => format!("image:{path}"),
            Self::Repo => "repo".into(),
        }
    }

    /// The glyph and color the picker should highlight: the chosen ones, or
    /// the project's defaults when the icon is an emoji, an image, or the
    /// repository's artwork.
    pub(crate) fn glyph_and_color(&self, seed: &str) -> (&'static str, &'static str) {
        match self {
            Self::Glyph { glyph, color } => (glyph, color),
            Self::Emoji(_) | Self::Image(_) | Self::Repo => match Self::default_for(seed) {
                Self::Glyph { glyph, color } => (glyph, color),
                Self::Emoji(_) | Self::Image(_) | Self::Repo => unreachable!(),
            },
        }
    }
}

fn fnv1a(text: &str) -> u32 {
    text.bytes().fold(2166136261u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(16777619)
    })
}

/// The palette color for the current appearance.
pub(crate) fn color_tone(color: &str, theme: &Theme) -> Hsla {
    let (_, dark, light) = COLORS
        .iter()
        .find(|(name, ..)| *name == color)
        .copied()
        .unwrap_or(COLORS[0]);
    Hsla::from(gpui::rgb(if theme.appearance.is_dark() {
        dark
    } else {
        light
    }))
}

fn glyph_asset(glyph: &str) -> &'static str {
    GLYPHS
        .iter()
        .find(|(name, _)| *name == glyph)
        .map(|(_, asset)| *asset)
        .unwrap_or(GLYPHS[0].1)
}

/// A rounded tile of `size` px: the glyph in its color over a tint of the
/// same color. The rail, the Main tab, and the picker all draw this.
pub(crate) fn glyph_tile(glyph: &str, color: &str, size: f32, theme: &Theme) -> AnyElement {
    let tone = color_tone(color, theme);
    let selector = format!("project-glyph-{glyph}-{color}");
    div()
        .debug_selector(move || selector.clone())
        .size(px(size))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px((size * 0.22).max(3.0)))
        .bg(tone.opacity(0.18))
        .text_color(tone)
        .child(icons::icon(glyph_asset(glyph)).size(px(size * 0.62)))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_icon_is_stable_per_project_and_comes_from_the_built_in_sets() {
        let first = ProjectIcon::default_for("space-1");
        assert_eq!(first, ProjectIcon::default_for("space-1"));
        let ProjectIcon::Glyph { glyph, color } = &first else {
            panic!("the default is a glyph");
        };
        assert!(GLYPHS.iter().any(|(name, _)| name == glyph));
        assert!(COLORS.iter().any(|(name, ..)| name == color));
        // Unknown or empty stored values read as the default, so a project
        // never shows up without an icon.
        assert_eq!(ProjectIcon::parse("", "space-1"), first);
        assert_eq!(ProjectIcon::parse("glyph:nope:blue", "space-1"), first);
        assert_eq!(ProjectIcon::parse("glyph:folder:nope", "space-1"), first);
        // Different projects spread over the sets.
        let defaults: std::collections::HashSet<_> = (0..64)
            .map(|index| ProjectIcon::default_for(&format!("space-{index}")).stored())
            .collect();
        assert!(defaults.len() > 8, "{defaults:?}");
    }

    #[test]
    fn stored_values_round_trip() {
        for icon in [
            ProjectIcon::Glyph {
                glyph: "terminal",
                color: "teal",
            },
            ProjectIcon::Emoji("🐯".into()),
            ProjectIcon::Image("/tmp/icon.png".into()),
            ProjectIcon::Repo,
        ] {
            assert_eq!(ProjectIcon::parse(&icon.stored(), "seed"), icon);
        }
        assert_eq!(
            ProjectIcon::Emoji("🐯".into()).glyph_and_color("seed"),
            ProjectIcon::default_for("seed").glyph_and_color("seed")
        );
    }
}

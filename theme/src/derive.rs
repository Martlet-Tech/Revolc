//! Fill missing semantic roles so a theme can be created from just a few
//! colours (or imported from another app with a partial mapping).

use crate::{Color, Theme, ThemeKind};

fn ensure(theme: &mut Theme, role: &str, color: Color) {
    if !theme.colors.contains_key(role) {
        theme.colors.insert(role.to_string(), color);
    }
}

/// Add every missing role. Called after inheritance and user overrides.
pub fn fill_missing(theme: &mut Theme) {
    let dark = theme.kind == ThemeKind::Dark;
    let default_app = if dark {
        Color(0x1F, 0x1F, 0x1F)
    } else {
        Color(0xF3, 0xF3, 0xF3)
    };
    let default_accent = if dark {
        Color(0x4C, 0xC2, 0xFF)
    } else {
        Color(0x0F, 0x6C, 0xBD)
    };
    let default_danger = if dark {
        Color(0xF1, 0x4C, 0x4C)
    } else {
        Color(0xC4, 0x2B, 0x1C)
    };

    let app = if theme.colors.contains_key("app.background") {
        theme.color("app.background")
    } else {
        default_app
    };
    let surface = if theme.colors.contains_key("surface.background") {
        theme.color("surface.background")
    } else {
        app.shift(if dark { 0.03 } else { -0.03 })
    };

    ensure(theme, "app.background", app);
    ensure(
        theme,
        "chrome.background",
        app.shift(if dark { 0.035 } else { -0.02 }),
    );
    ensure(theme, "surface.background", surface);
    ensure(
        theme,
        "surface.raised",
        surface.shift(if dark { 0.04 } else { 0.06 }),
    );
    ensure(
        theme,
        "surface.hover",
        surface.shift(if dark { 0.09 } else { -0.05 }),
    );

    let line_base = theme.color("surface.raised");
    ensure(
        theme,
        "border.subtle",
        line_base.shift(if dark { 0.12 } else { -0.13 }),
    );

    let ink = if app.luminance() > 0.35 {
        Color(0x1A, 0x1A, 0x1A)
    } else {
        Color(0xF5, 0xF5, 0xF5)
    };
    ensure(theme, "foreground.primary", ink);
    ensure(theme, "foreground.secondary", ink.blend(app, 0.38));
    ensure(theme, "foreground.inverse", app);

    let accent = theme.color("accent.default");
    ensure(
        theme,
        "accent.default",
        if accent == Color(0, 0, 0) {
            default_accent
        } else {
            accent
        },
    );
    ensure(
        theme,
        "accent.soft",
        theme.color("accent.default").blend(surface, 0.62),
    );

    let danger = theme.color("danger.default");
    ensure(
        theme,
        "danger.default",
        if danger == Color(0, 0, 0) {
            default_danger
        } else {
            danger
        },
    );
    ensure(
        theme,
        "danger.hover",
        theme
            .color("danger.default")
            .shift(if dark { 0.08 } else { -0.08 }),
    );
    ensure(theme, "focus.ring", theme.color("accent.default"));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn partial(kind: ThemeKind, colors: &[(&str, &str)]) -> Theme {
        let mut map = std::collections::HashMap::new();
        for (k, v) in colors {
            map.insert((*k).to_string(), (*v).to_string());
        }
        let mut theme = Theme {
            id: "test".into(),
            name: "test".into(),
            kind,
            colors: map
                .iter()
                .filter_map(|(k, v)| Some((k.clone(), Color::parse(v)?)))
                .collect(),
            source: "test".into(),
        };
        fill_missing(&mut theme);
        theme
    }

    #[test]
    fn four_seeds_produce_a_complete_theme() {
        let theme = partial(
            ThemeKind::Dark,
            &[
                ("app.background", "#10261A"),
                ("surface.background", "#1E4632"),
                ("accent.default", "#8BC98F"),
                ("foreground.primary", "#F2F7F3"),
            ],
        );
        for role in crate::ROLE_KEYS {
            assert!(theme.colors.contains_key(*role), "missing {role}");
        }
        assert!(
            theme
                .color("foreground.primary")
                .contrast(theme.color("app.background"))
                > 4.5
        );
    }

    #[test]
    fn light_theme_picks_dark_ink() {
        let theme = partial(ThemeKind::Light, &[("app.background", "#F3F3F3")]);
        assert!(theme.color("foreground.primary").luminance() < 0.2);
    }

    #[test]
    fn does_not_overwrite_explicit_values() {
        let theme = partial(ThemeKind::Dark, &[("danger.default", "#123456")]);
        assert_eq!(theme.color("danger.default"), Color(0x12, 0x34, 0x56));
    }
}

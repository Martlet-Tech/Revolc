//! Four bundled themes. Adding one = drop a JSON in `themes/` + add an entry
//! here and a test; no UI code change.

pub const LIGHT: (&str, &str) = ("light", include_str!("../themes/light.json"));
pub const DARK: (&str, &str) = ("dark", include_str!("../themes/dark.json"));
pub const CLOVER_LIGHT: (&str, &str) =
    ("clover-light", include_str!("../themes/clover-light.json"));
pub const CLOVER_DARK: (&str, &str) = ("clover-dark", include_str!("../themes/clover-dark.json"));

pub const ALL: &[(&str, &str)] = &[LIGHT, DARK, CLOVER_LIGHT, CLOVER_DARK];

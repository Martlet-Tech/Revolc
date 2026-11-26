//! Minimal colour maths for theme derivation and contrast checks.
//! No Win32 dependency: `COLORREF` byte order is handled by the UI layer.

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color(pub u8, pub u8, pub u8);

impl Color {
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().trim_start_matches('#');
        if s.len() != 6 {
            return None;
        }
        let v = u32::from_str_radix(s, 16).ok()?;
        Some(Self(
            ((v >> 16) & 0xff) as u8,
            ((v >> 8) & 0xff) as u8,
            (v & 0xff) as u8,
        ))
    }

    pub fn to_hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.0, self.1, self.2)
    }

    pub fn to_colorref(self) -> u32 {
        (self.2 as u32) << 16 | (self.1 as u32) << 8 | self.0 as u32
    }

    /// WCAG relative luminance (0 = black, 1 = white).
    pub fn luminance(self) -> f64 {
        let channel = |c: u8| -> f64 {
            let c = c as f64 / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(self.0) + 0.7152 * channel(self.1) + 0.0722 * channel(self.2)
    }

    pub fn contrast(self, other: Color) -> f64 {
        let a = self.luminance();
        let b = other.luminance();
        let (hi, lo) = if a > b { (a, b) } else { (b, a) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// Linear blend: `t = 0` is `self`, `t = 1` is `other`.
    pub fn blend(self, other: Color, t: f64) -> Color {
        let t = t.clamp(0.0, 1.0);
        let mix = |a: u8, b: u8| -> u8 {
            (a as f64 * (1.0 - t) + b as f64 * t)
                .round()
                .clamp(0.0, 255.0) as u8
        };
        Color(
            mix(self.0, other.0),
            mix(self.1, other.1),
            mix(self.2, other.2),
        )
    }

    /// Shift lightness while keeping hue direction. `amount > 0` lightens.
    /// HSL is used instead of OKLCH to stay dependency-free; the derivation
    /// only needs monotonic lightness steps, not perceptual uniformity.
    pub fn shift(self, amount: f64) -> Color {
        let (h, s, l) = self.to_hsl();
        Color::from_hsl(h, s, (l + amount).clamp(0.0, 1.0))
    }

    fn to_hsl(self) -> (f64, f64, f64) {
        let r = self.0 as f64 / 255.0;
        let g = self.1 as f64 / 255.0;
        let b = self.2 as f64 / 255.0;
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let l = (max + min) / 2.0;
        let d = max - min;
        if d == 0.0 {
            return (0.0, 0.0, l);
        }
        let s = d / (1.0 - (2.0 * l - 1.0).abs());
        let h = match max {
            x if x == r => ((g - b) / d).rem_euclid(6.0),
            x if x == g => ((b - r) / d) + 2.0,
            _ => ((r - g) / d) + 4.0,
        } * 60.0;
        (h, s, l)
    }

    fn from_hsl(h: f64, s: f64, l: f64) -> Color {
        let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
        let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
        let m = l - c / 2.0;
        let (r, g, b) = match (h.rem_euclid(360.0) / 60.0) as u32 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        let byte = |v: f64| -> u8 { ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8 };
        Color(byte(r), byte(g), byte(b))
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_roundtrip() {
        assert_eq!(Color::parse("#1B3D28"), Some(Color(0x1b, 0x3d, 0x28)));
        assert_eq!(Color(0x1b, 0x3d, 0x28).to_hex(), "#1B3D28");
        assert_eq!(Color::parse("nope"), None);
    }

    #[test]
    fn colorref_uses_bgr_byte_order() {
        assert_eq!(Color(0x11, 0x22, 0x33).to_colorref(), 0x00332211);
    }

    #[test]
    fn contrast_is_symmetric_and_bounded() {
        let a = Color(0xff, 0xff, 0xff);
        let b = Color(0x00, 0x00, 0x00);
        assert!((a.contrast(b) - 21.0).abs() < 0.01);
        assert!((a.contrast(b) - b.contrast(a)).abs() < 1e-9);
    }

    #[test]
    fn shift_keeps_mono_lightness() {
        let base = Color(0x1b, 0x3d, 0x28);
        assert!(base.shift(0.1).luminance() > base.luminance());
        assert!(base.shift(-0.1).luminance() < base.luminance());
    }
}

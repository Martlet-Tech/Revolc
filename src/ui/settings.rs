//! Settings tab: a painted (no HWND) page inside the main window's view rect.
//!
//! Layout is computed once by `layout()` and used by both `paint()` and
//! `hit()` — the old code duplicated the arithmetic and needed a `y -= 12`
//! hack to tuck hints under rows. All colours come from the resolved semantic
//! palette; see docs/UI_SPEC.md.

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::{
    CreateFontW, CreatePen, CreateSolidBrush, DeleteObject, DrawTextW, FillRect, GetStockObject,
    RoundRect, SelectObject, SetBkMode, SetTextColor, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS,
    DEFAULT_CHARSET, DEFAULT_GUI_FONT, DEFAULT_PITCH, DRAW_TEXT_FORMAT, DT_CENTER, DT_END_ELLIPSIS,
    DT_LEFT, DT_SINGLELINE, DT_VCENTER, FW_NORMAL, HDC, HFONT, HGDIOBJ, NULL_BRUSH, OUT_TT_PRECIS,
    PS_SOLID, TRANSPARENT,
};

use revolc_common::config::Config;
use revolc_common::settings::SettingId;
use revolc_theme::ThemeSet;

use super::gdi::line;

const RAIL_W: i32 = 140;
const RAIL_ITEM_H: i32 = 44;
const CARD_H: i32 = 64;
const CARD_HINT_H: i32 = 20;
const CARD_GAP: i32 = 12;
const CONTENT_TOP: i32 = 88;

const CATEGORIES: [&str; 3] = ["常规", "主题", "关于"];
pub const CATEGORY_COUNT: usize = CATEGORIES.len();

fn settings_in_category(category: usize) -> Vec<SettingId> {
    match category {
        0 => vec![
            SettingId::TakeoverMode,
            SettingId::Autostart,
            SettingId::CloseAction,
        ],
        1 => vec![SettingId::ThemePreset, SettingId::TabRadius],
        _ => Vec::new(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Control {
    Toggle,
    Segmented,
    Swatch,
}

impl Control {
    fn of(id: SettingId) -> Self {
        match id {
            SettingId::TakeoverMode | SettingId::Autostart => Control::Toggle,
            SettingId::CloseAction | SettingId::TabRadius => Control::Segmented,
            SettingId::ThemePreset => Control::Swatch,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsHit {
    Category(usize),
    Setting(SettingId),
    /// A specific option in a segmented/swatch control.
    Choice(SettingId, usize),
}

struct RowLayout {
    id: SettingId,
    rect: RECT,
    choices: Vec<RECT>,
    control: Control,
}

struct Layout {
    rail: Vec<RECT>,
    rows: Vec<RowLayout>,
}

pub struct SettingsPanel {
    category: usize,
}

impl SettingsPanel {
    pub fn new() -> Self {
        Self { category: 0 }
    }

    pub fn select(&mut self, category: usize) {
        if category < CATEGORY_COUNT {
            self.category = category;
        }
    }

    fn pane(rc: RECT) -> (i32, i32) {
        let left = rc.left + RAIL_W + 24;
        let right = (rc.right - 24).min(left + 720);
        (left, right)
    }

    /// Single source of truth for every rectangle on the page.
    fn layout(&self, rc: RECT, cfg: &Config) -> Layout {
        let rail = (0..CATEGORY_COUNT)
            .map(|i| RECT {
                left: rc.left + 8,
                top: rc.top + CONTENT_TOP + (i as i32) * (RAIL_ITEM_H + 8),
                right: rc.left + RAIL_W - 8,
                bottom: rc.top + CONTENT_TOP + (i as i32) * (RAIL_ITEM_H + 8) + RAIL_ITEM_H,
            })
            .collect();
        let (left, right) = Self::pane(rc);
        let mut rows = Vec::new();
        let mut y = rc.top + CONTENT_TOP;
        for id in settings_in_category(self.category) {
            let hint = if id.is_available(cfg) {
                id.hint()
            } else {
                id.unavailable_hint()
            };
            let height = CARD_H + if hint.is_empty() { 0 } else { CARD_HINT_H };
            let rect = RECT {
                left,
                top: y,
                right,
                bottom: y + height,
            };
            let control = Control::of(id);
            let count = match control {
                Control::Toggle => 1,
                Control::Segmented | Control::Swatch => id.choices(cfg).len().max(1),
            };
            let cw = match control {
                Control::Segmented => 64,
                Control::Swatch => 20,
                Control::Toggle => 40,
            };
            let total = count as i32 * cw + (count as i32 - 1) * 4;
            let start = right - 14 - total;
            let cy = y + (CARD_H - 22) / 2;
            let choices = (0..count)
                .map(|i| RECT {
                    left: start + i as i32 * (cw + 4),
                    top: cy,
                    right: start + i as i32 * (cw + 4) + cw,
                    bottom: cy + 22,
                })
                .collect();
            rows.push(RowLayout {
                id,
                rect,
                choices,
                control,
            });
            y += height + CARD_GAP;
        }
        Layout { rail, rows }
    }

    /// Paint the settings page into `rc` (already the view rect).
    pub unsafe fn paint(&self, hdc: HDC, rc: RECT, cfg: &Config) {
        let p = &cfg.palette;
        fill(hdc, rc, COLORREF(p.app_background().to_colorref()));
        fill(
            hdc,
            RECT {
                left: rc.left,
                top: rc.top,
                right: rc.left + RAIL_W,
                bottom: rc.bottom,
            },
            COLORREF(p.surface_background().to_colorref()),
        );
        fill(
            hdc,
            RECT {
                left: rc.left + RAIL_W,
                top: rc.top,
                right: rc.left + RAIL_W + 1,
                bottom: rc.bottom,
            },
            COLORREF(p.border_subtle().to_colorref()),
        );

        let title_font = font(16, 500);
        let body_font = font(13, FW_NORMAL.0 as i32);
        let caption_font = font(11, FW_NORMAL.0 as i32);
        let old_font = SelectObject(hdc, HGDIOBJ(body_font.0));
        let _ = SetBkMode(hdc, TRANSPARENT);

        let layout = self.layout(rc, cfg);
        for (i, item) in layout.rail.iter().enumerate() {
            if i == self.category {
                fill(hdc, *item, COLORREF(p.surface_raised().to_colorref()));
                fill(
                    hdc,
                    RECT {
                        left: item.left,
                        top: item.top,
                        right: item.left + 3,
                        bottom: item.bottom,
                    },
                    COLORREF(p.accent_default().to_colorref()),
                );
                text(
                    hdc,
                    RECT {
                        left: item.left + 12,
                        ..*item
                    },
                    CATEGORIES[i],
                    title_font,
                    COLORREF(p.foreground_primary().to_colorref()),
                    DT_LEFT,
                );
            } else {
                text(
                    hdc,
                    RECT {
                        left: item.left + 12,
                        ..*item
                    },
                    CATEGORIES[i],
                    body_font,
                    COLORREF(p.foreground_secondary().to_colorref()),
                    DT_LEFT,
                );
            }
        }

        let (left, right) = Self::pane(rc);
        let heading = match self.category {
            0 => ("常规", "管理接管方式与窗口行为"),
            1 => ("主题", "配色、标签形状与界面氛围"),
            _ => (
                "关于 Revolc",
                "把 Windows 资源管理器改造成多标签浏览器 · Clover 的原生重制版",
            ),
        };
        text(
            hdc,
            RECT {
                left,
                top: rc.top + 24,
                right,
                bottom: rc.top + 52,
            },
            heading.0,
            title_font,
            COLORREF(p.foreground_primary().to_colorref()),
            DT_LEFT,
        );
        text(
            hdc,
            RECT {
                left,
                top: rc.top + 52,
                right,
                bottom: rc.top + 76,
            },
            heading.1,
            caption_font,
            COLORREF(p.foreground_secondary().to_colorref()),
            DT_LEFT,
        );

        for row in &layout.rows {
            let available = row.id.is_available(cfg);
            card(
                hdc,
                row.rect,
                COLORREF(p.surface_raised().to_colorref()),
                COLORREF(p.border_subtle().to_colorref()),
            );
            let ink = if available {
                COLORREF(p.foreground_primary().to_colorref())
            } else {
                COLORREF(p.foreground_secondary().to_colorref())
            };
            text(
                hdc,
                RECT {
                    left: row.rect.left + 12,
                    top: row.rect.top + 4,
                    right: right - 150,
                    bottom: row.rect.top + CARD_H - 4,
                },
                row.id.label(),
                body_font,
                ink,
                DT_LEFT,
            );
            let hint = if available {
                row.id.hint()
            } else {
                row.id.unavailable_hint()
            };
            if !hint.is_empty() {
                text(
                    hdc,
                    RECT {
                        left: row.rect.left + 12,
                        top: row.rect.top + CARD_H,
                        right,
                        bottom: row.rect.bottom,
                    },
                    hint,
                    caption_font,
                    COLORREF(p.foreground_secondary().to_colorref()),
                    DT_LEFT,
                );
            }
            let selected = current_index(row.id, cfg);
            match row.control {
                Control::Toggle => {
                    let cell = row.choices[0];
                    let on = available && selected == Some(0);
                    let fill = if on {
                        COLORREF(p.accent_default().to_colorref())
                    } else {
                        COLORREF(p.surface_hover().to_colorref())
                    };
                    fill_round(hdc, cell, fill, 11);
                    let knob_x = if on { cell.right - 15 } else { cell.left + 3 };
                    fill_round(
                        hdc,
                        RECT {
                            left: knob_x,
                            top: cell.top + 3,
                            right: knob_x + 16,
                            bottom: cell.bottom - 3,
                        },
                        COLORREF(p.foreground_primary().to_colorref()),
                        8,
                    );
                }
                Control::Segmented => {
                    let choices = row.id.choices(cfg);
                    for (i, cell) in row.choices.iter().enumerate() {
                        let active = available && Some(i) == selected;
                        let fill = if active {
                            COLORREF(p.accent_default().to_colorref())
                        } else {
                            COLORREF(p.surface_hover().to_colorref())
                        };
                        fill_round(hdc, *cell, fill, 4);
                        let ink = if active {
                            COLORREF(p.app_background().to_colorref())
                        } else {
                            COLORREF(p.foreground_secondary().to_colorref())
                        };
                        let label = choices.get(i).map(|c| c.0.as_str()).unwrap_or("");
                        text(hdc, *cell, label, caption_font, ink, DT_CENTER);
                    }
                }
                Control::Swatch => {
                    let set = ThemeSet::discover();
                    for (i, cell) in row.choices.iter().enumerate() {
                        let theme_id = row
                            .id
                            .choices(cfg)
                            .get(i)
                            .map(|c| c.1.trim_matches('"').to_string());
                        let color = theme_id
                            .as_deref()
                            .and_then(|id| set.resolve(id).ok())
                            .map(|t| COLORREF(t.accent_default().to_colorref()))
                            .unwrap_or_else(|| COLORREF(p.accent_default().to_colorref()));
                        fill_round(hdc, *cell, color, 9);
                        if Some(i) == selected {
                            outline_round(
                                hdc,
                                RECT {
                                    left: cell.left - 2,
                                    top: cell.top - 2,
                                    right: cell.right + 2,
                                    bottom: cell.bottom + 2,
                                },
                                COLORREF(p.foreground_primary().to_colorref()),
                                11,
                            );
                        }
                    }
                    text(
                        hdc,
                        RECT {
                            left: row.rect.left + 12,
                            top: row.rect.top + 26,
                            right,
                            bottom: row.rect.bottom,
                        },
                        &cfg.palette.name,
                        caption_font,
                        COLORREF(p.foreground_secondary().to_colorref()),
                        DT_LEFT,
                    );
                }
            }
        }

        SelectObject(hdc, HGDIOBJ(old_font.0));
        let _ = DeleteObject(HGDIOBJ(title_font.0));
        let _ = DeleteObject(HGDIOBJ(body_font.0));
        let _ = DeleteObject(HGDIOBJ(caption_font.0));
    }

    pub fn hit(&self, x: i32, y: i32, rc: RECT, cfg: &Config) -> Option<SettingsHit> {
        let layout = self.layout(rc, cfg);
        for (i, item) in layout.rail.iter().enumerate() {
            if inside(x, y, *item) {
                return Some(SettingsHit::Category(i));
            }
        }
        for row in layout.rows {
            if !inside(x, y, row.rect) || !row.id.is_available(cfg) {
                continue;
            }
            for (i, cell) in row.choices.iter().enumerate() {
                if inside(x, y, *cell) && row.control != Control::Toggle {
                    return Some(SettingsHit::Choice(row.id, i));
                }
            }
            // Anywhere else on the card cycles the value; the control cells
            // above select a specific option.
            return Some(SettingsHit::Setting(row.id));
        }
        None
    }
}

fn current_index(id: SettingId, cfg: &Config) -> Option<usize> {
    let current = id.display(cfg);
    id.choices(cfg)
        .iter()
        .position(|(label, _)| *label == current)
}

fn inside(x: i32, y: i32, rc: RECT) -> bool {
    x >= rc.left && x < rc.right && y >= rc.top && y < rc.bottom
}

unsafe fn fill(hdc: HDC, rc: RECT, color: COLORREF) {
    let brush = CreateSolidBrush(color);
    let _ = FillRect(hdc, &rc, brush);
    let _ = DeleteObject(HGDIOBJ(brush.0));
}

unsafe fn card(hdc: HDC, rc: RECT, bg: COLORREF, border: COLORREF) {
    fill(hdc, rc, bg);
    let pen = CreatePen(PS_SOLID, 1, border);
    let old = SelectObject(hdc, HGDIOBJ(pen.0));
    line(hdc, rc.left, rc.top, rc.right - 1, rc.top);
    line(hdc, rc.left, rc.bottom - 1, rc.right - 1, rc.bottom - 1);
    line(hdc, rc.left, rc.top, rc.left, rc.bottom - 1);
    line(hdc, rc.right - 1, rc.top, rc.right - 1, rc.bottom - 1);
    SelectObject(hdc, old);
    let _ = DeleteObject(HGDIOBJ(pen.0));
}

unsafe fn fill_round(hdc: HDC, rc: RECT, color: COLORREF, radius: i32) {
    let pen = CreatePen(PS_SOLID, 1, color);
    let old = SelectObject(hdc, HGDIOBJ(pen.0));
    let brush = CreateSolidBrush(color);
    let old_brush = SelectObject(hdc, HGDIOBJ(brush.0));
    let _ = RoundRect(hdc, rc.left, rc.top, rc.right, rc.bottom, radius, radius);
    let _ = SelectObject(hdc, old_brush);
    let _ = SelectObject(hdc, old);
    let _ = DeleteObject(HGDIOBJ(brush.0));
    let _ = DeleteObject(HGDIOBJ(pen.0));
}

unsafe fn outline_round(hdc: HDC, rc: RECT, color: COLORREF, radius: i32) {
    let pen = CreatePen(PS_SOLID, 1, color);
    let old = SelectObject(hdc, HGDIOBJ(pen.0));
    let old_brush = SelectObject(hdc, HGDIOBJ(GetStockObject(NULL_BRUSH).0));
    let _ = RoundRect(hdc, rc.left, rc.top, rc.right, rc.bottom, radius, radius);
    let _ = SelectObject(hdc, old_brush);
    let _ = SelectObject(hdc, old);
    let _ = DeleteObject(HGDIOBJ(pen.0));
}

unsafe fn font(px: i32, weight: i32) -> HFONT {
    let handle = CreateFontW(
        -px,
        0,
        0,
        0,
        weight,
        0,
        0,
        0,
        DEFAULT_CHARSET.0 as u32,
        OUT_TT_PRECIS.0 as u32,
        CLIP_DEFAULT_PRECIS.0 as u32,
        CLEARTYPE_QUALITY.0 as u32,
        DEFAULT_PITCH.0 as u32,
        w!("Microsoft YaHei UI"),
    );
    if handle.is_invalid() {
        HFONT(GetStockObject(DEFAULT_GUI_FONT).0)
    } else {
        handle
    }
}

unsafe fn text(
    hdc: HDC,
    mut rc: RECT,
    label: &str,
    font: HFONT,
    ink: COLORREF,
    align: DRAW_TEXT_FORMAT,
) {
    let old = SelectObject(hdc, HGDIOBJ(font.0));
    let _ = SetTextColor(hdc, ink);
    let mut wide: Vec<u16> = label.encode_utf16().chain(Some(0)).collect();
    let flags = DRAW_TEXT_FORMAT(DT_SINGLELINE.0 | DT_VCENTER.0 | DT_END_ELLIPSIS.0 | align.0);
    let _ = DrawTextW(hdc, &mut wide, &mut rc, flags);
    SelectObject(hdc, HGDIOBJ(old.0));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_and_hit_share_the_same_rectangles() {
        let cfg = Config::default();
        let rc = RECT {
            left: 0,
            top: 0,
            right: 900,
            bottom: 500,
        };
        let panel = SettingsPanel::new();
        let layout = panel.layout(rc, &cfg);
        assert_eq!(layout.rows.len(), 3);
        let row = &layout.rows[0];
        assert_eq!(
            panel.hit(row.rect.left + 20, row.rect.top + 10, rc, &cfg),
            Some(SettingsHit::Setting(row.id))
        );
    }

    #[test]
    fn swatch_control_exposes_choices() {
        let cfg = Config::default();
        let rc = RECT {
            left: 0,
            top: 0,
            right: 900,
            bottom: 500,
        };
        let mut panel = SettingsPanel::new();
        panel.select(1);
        let layout = panel.layout(rc, &cfg);
        assert_eq!(layout.rows[0].choices.len(), 4);
        let cell = layout.rows[0].choices[1];
        assert_eq!(
            panel.hit(
                (cell.left + cell.right) / 2,
                (cell.top + cell.bottom) / 2,
                rc,
                &cfg
            ),
            Some(SettingsHit::Choice(SettingId::ThemePreset, 1))
        );
    }
}

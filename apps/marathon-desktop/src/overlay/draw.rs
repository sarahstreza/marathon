//! Drawing the overlay with egui's painter. Everything is laid out in
//! points on a screen [`SCREEN_HEIGHT_POINTS`](super::SCREEN_HEIGHT_POINTS) tall, sized to be readable
//! from across a room (PLAN.md product principle, "Glanceable").
//!
//! Shapes are painted directly rather than through egui's widgets: the
//! overlay is not interactive (input arrives as actions), and painting
//! gives exact control over the look and the fades.

use std::time::Instant;

use egui::text::{LayoutJob, TextWrapping};
use egui::{Align2, Color32, CornerRadius, FontId, LayerId, Painter, Pos2, Rect, Vec2, pos2, vec2};
use marathon_client::tuner::format_ms;

use super::Overlay;

/// Distance from the screen edges.
const MARGIN: f32 = 24.0;
const PANEL: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 190);
const TEXT: Color32 = Color32::from_rgb(240, 240, 240);
const DIM: Color32 = Color32::from_rgb(170, 170, 175);
const ACCENT: Color32 = Color32::from_rgb(235, 170, 60);
const TRACK: Color32 = Color32::from_rgba_premultiplied(90, 90, 90, 160);
const HIGHLIGHT: Color32 = Color32::from_rgba_premultiplied(60, 60, 70, 220);
const ERROR: Color32 = Color32::from_rgba_premultiplied(110, 20, 20, 220);
const ROUNDING: f32 = 10.0;

pub fn draw(overlay: &Overlay, ctx: &egui::Context, now: Instant) {
    let screen = ctx.content_rect();
    let painter = ctx.layer_painter(LayerId::background());

    let opacity = overlay.banner_fade.opacity(now);
    if opacity > 0.0 {
        let mut painter = painter.clone();
        painter.set_opacity(opacity);
        banner(overlay, &painter, screen, now);
    }
    let opacity = overlay.guide_fade.opacity(now);
    if opacity > 0.0 {
        let mut painter = painter.clone();
        painter.set_opacity(opacity);
        guide(overlay, &painter, screen);
    }
    let opacity = overlay.toast_fade.opacity(now);
    if opacity > 0.0
        && let Some(message) = &overlay.toast
    {
        let mut painter = painter.clone();
        painter.set_opacity(opacity);
        toast(message, &painter, screen);
    }
}

/// The channel banner along the bottom: number, name, item, progress.
fn banner(overlay: &Overlay, painter: &Painter, screen: Rect, now: Instant) {
    let Some(banner) = &overlay.banner else {
        return;
    };
    let height = 104.0;
    let rect = Rect::from_min_max(
        pos2(screen.left() + MARGIN, screen.bottom() - MARGIN - height),
        pos2(screen.right() - MARGIN, screen.bottom() - MARGIN),
    );
    painter.rect_filled(rect, ROUNDING, PANEL);

    let inner = rect.shrink(16.0);
    let number_width = 110.0;
    painter.text(
        pos2(inner.left() + number_width / 2.0, inner.center().y),
        Align2::CENTER_CENTER,
        banner.channel_number.to_string(),
        FontId::proportional(52.0),
        ACCENT,
    );

    let left = inner.left() + number_width + 12.0;
    let width = inner.right() - left;
    let time_width = 130.0;
    line(
        painter,
        pos2(left, inner.top()),
        &banner.channel_name,
        26.0,
        TEXT,
        width - time_width,
    );
    let item = if overlay.tuning && banner.title.is_none() {
        "Tuning…".to_owned()
    } else {
        banner.item_line()
    };
    line(
        painter,
        pos2(left, inner.top() + 34.0),
        &item,
        19.0,
        DIM,
        width,
    );

    // Progress: a bar along the bottom, and the times on the right.
    let bar = Rect::from_min_size(pos2(left, inner.bottom() - 6.0), vec2(width, 6.0));
    painter.rect_filled(bar, CornerRadius::same(3), TRACK);
    let position = overlay.position_at(now);
    if let (Some(position), Some(duration)) = (position, overlay.progress.duration_ms)
        && duration > 0
    {
        let fraction = (position as f32 / duration as f32).clamp(0.0, 1.0);
        let filled = Rect::from_min_size(bar.min, vec2(bar.width() * fraction, bar.height()));
        painter.rect_filled(filled, CornerRadius::same(3), ACCENT);
    }
    let times = match (position, overlay.progress.duration_ms) {
        (Some(p), Some(d)) => format!("{} / {}", format_ms(p), format_ms(d)),
        (Some(p), None) => format_ms(p),
        _ => String::new(),
    };
    let paused = if overlay.progress.paused && !overlay.tuning {
        "Paused  "
    } else {
        ""
    };
    painter.text(
        pos2(inner.right(), inner.top() + 4.0),
        Align2::RIGHT_TOP,
        format!("{paused}{times}"),
        FontId::monospace(16.0),
        DIM,
    );
}

/// The guide: a list of channels down the left, the highlighted one kept
/// in view.
fn guide(overlay: &Overlay, painter: &Painter, screen: Rect) {
    let Some(guide) = &overlay.guide else {
        return;
    };
    let width = (screen.width() * 0.5).clamp(300.0, 520.0);
    let rect = Rect::from_min_max(
        pos2(screen.left() + MARGIN, screen.top() + MARGIN),
        pos2(screen.left() + MARGIN + width, screen.bottom() - MARGIN),
    );
    painter.rect_filled(rect, ROUNDING, PANEL);
    let inner = rect.shrink(14.0);
    painter.text(
        inner.left_top(),
        Align2::LEFT_TOP,
        "Guide",
        FontId::proportional(24.0),
        TEXT,
    );

    let row_height = 48.0;
    let top = inner.top() + 38.0;
    let visible = (((inner.bottom() - top) / row_height).floor() as usize).max(1);
    let first = first_visible_row(guide.highlighted, guide.rows.len(), visible);
    for (slot, (index, row)) in guide
        .rows
        .iter()
        .enumerate()
        .skip(first)
        .take(visible)
        .enumerate()
    {
        let y = top + slot as f32 * row_height;
        let row_rect = Rect::from_min_size(pos2(inner.left(), y), vec2(inner.width(), row_height));
        if index == guide.highlighted {
            painter.rect_filled(row_rect.shrink(2.0), 6.0, HIGHLIGHT);
        }
        let number_color = if row.current { ACCENT } else { TEXT };
        painter.text(
            pos2(row_rect.left() + 48.0, row_rect.center().y),
            Align2::RIGHT_CENTER,
            row.number.to_string(),
            FontId::proportional(24.0),
            number_color,
        );
        let left = row_rect.left() + 62.0;
        let width = row_rect.right() - left - 8.0;
        line(painter, pos2(left, y + 4.0), &row.name, 19.0, TEXT, width);
        let on = row.on.as_deref().unwrap_or("Nothing to play");
        line(painter, pos2(left, y + 26.0), on, 15.0, DIM, width);
    }
}

/// The first row to show so that `highlighted` is in view, roughly
/// centred, without scrolling past either end.
fn first_visible_row(highlighted: usize, rows: usize, visible: usize) -> usize {
    if rows <= visible {
        return 0;
    }
    highlighted.saturating_sub(visible / 2).min(rows - visible)
}

/// An error message in the top right.
fn toast(message: &str, painter: &Painter, screen: Rect) {
    let width = (screen.width() * 0.45).max(240.0);
    let galley = painter.layout_job(job(message, 17.0, TEXT, width - 28.0, 3));
    let size = galley.size() + Vec2::splat(28.0);
    let rect = Rect::from_min_size(
        pos2(screen.right() - MARGIN - size.x, screen.top() + MARGIN),
        size,
    );
    painter.rect_filled(rect, ROUNDING, ERROR);
    painter.galley(rect.min + Vec2::splat(14.0), galley, TEXT);
}

/// One line of text at `pos` (top left), cut short with an ellipsis if
/// wider than `max_width`.
fn line(painter: &Painter, pos: Pos2, text: &str, size: f32, color: Color32, max_width: f32) {
    let galley = painter.layout_job(job(text, size, color, max_width, 1));
    painter.galley(pos, galley, color);
}

fn job(text: &str, size: f32, color: Color32, max_width: f32, max_rows: usize) -> LayoutJob {
    let mut job = LayoutJob::simple(
        text.to_owned(),
        FontId::proportional(size),
        color,
        max_width.max(1.0),
    );
    job.wrap = TextWrapping {
        max_width: max_width.max(1.0),
        max_rows,
        break_anywhere: max_rows == 1,
        overflow_character: Some('…'),
    };
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_highlighted_row_stays_in_view() {
        assert_eq!(first_visible_row(0, 5, 10), 0);
        assert_eq!(first_visible_row(4, 5, 10), 0);
        assert_eq!(first_visible_row(0, 30, 10), 0);
        assert_eq!(first_visible_row(12, 30, 10), 7);
        assert_eq!(first_visible_row(29, 30, 10), 20);
        for h in 0..30 {
            let first = first_visible_row(h, 30, 10);
            assert!((first..first + 10).contains(&h), "{h}");
        }
    }
}

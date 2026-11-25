// Modified by enzUI contributors. See FORK.md.
//! How far shaped glyphs can paint beyond their line.
use super::cosmic_text::{FontSystem, LayoutRun, fontdb};

/// Extra reach, as a fraction of the font size, allowed for a variable font
/// drawn at another weight: its header bounds describe the default instance.
const VARIABLE_WEIGHT_REACH: f32 = 0.1;

/// The vertical span a layout run's glyphs can paint, in buffer units from
/// the top of the buffer: its line box, grown to cover every glyph's font
/// bounds placed where the glyph sits, mark offsets included.
///
/// The span excludes rasterization rounding, under two physical pixels on
/// each side. Returns `None` when a glyph's font is not in `fonts`.
pub fn run_span(run: &LayoutRun<'_>, fonts: &mut FontSystem) -> Option<(f32, f32)> {
    let mut top = run.line_top;
    let mut bottom = run.line_top + run.line_height;
    let mut face: Option<(fontdb::ID, fontdb::Weight, Reach)> = None;

    for glyph in run.glyphs {
        let reach = match face {
            Some((id, weight, reach)) if id == glyph.font_id && weight == glyph.font_weight => {
                reach
            }
            _ => {
                let reach = font_reach(fonts, glyph.font_id, glyph.font_weight)?;
                face = Some((glyph.font_id, glyph.font_weight, reach));
                reach
            }
        };
        let baseline = run.line_y + glyph.y - glyph.font_size * glyph.y_offset;
        top = top.min(baseline - reach.above * glyph.font_size);
        bottom = bottom.max(baseline + reach.below * glyph.font_size);
    }

    Some((top, bottom))
}

/// How far a font's glyphs reach above and below the baseline, per unit of
/// font size.
#[derive(Debug, Clone, Copy)]
struct Reach {
    above: f32,
    below: f32,
}

fn font_reach(fonts: &mut FontSystem, id: fontdb::ID, weight: fontdb::Weight) -> Option<Reach> {
    let font = fonts.get_font(id, weight)?;
    let metrics = font.metrics();
    let em = f32::from(metrics.units_per_em);
    if em <= 0.0 {
        return None;
    }
    let (high, low) = metrics
        .bounds
        .map_or((metrics.ascent, metrics.descent), |bounds| {
            (
                bounds.y_max.max(metrics.ascent),
                bounds.y_min.min(metrics.descent),
            )
        });
    let slack = if font
        .as_swash()
        .variations()
        .find_by_tag(u32::from_be_bytes(*b"wght"))
        .is_some()
    {
        VARIABLE_WEIGHT_REACH
    } else {
        0.0
    };

    Some(Reach {
        above: (high / em).max(0.0) + slack,
        below: (-low / em).max(0.0) + slack,
    })
}

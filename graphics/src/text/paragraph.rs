// Modified by enzUI contributors. See FORK.md.
//! Draw paragraphs.
use crate::core;
use crate::core::alignment;
use crate::core::text::{
    Affinity, Alignment, CursorPosition, Ellipsis, Hit, LineHeight, Shaping, Span, Text, Wrapping,
};
use crate::core::{Font, Pixels, Point, Rectangle, Size};
use crate::text;

use std::fmt;
use std::sync::{self, Arc};

/// A bunch of text.
#[derive(Clone, PartialEq)]
pub struct Paragraph(Arc<Internal>);

#[derive(Clone)]
struct Internal {
    buffer: cosmic_text::Buffer,
    font: Font,
    shaping: Shaping,
    wrapping: Wrapping,
    ellipsis: Ellipsis,
    align_x: Alignment,
    align_y: alignment::Vertical,
    bounds: Size,
    min_bounds: Size,
    version: text::Version,
    hint: bool,
    hint_factor: f32,
}

impl Paragraph {
    /// Creates a new empty [`Paragraph`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the buffer of the [`Paragraph`].
    pub fn buffer(&self) -> &cosmic_text::Buffer {
        &self.internal().buffer
    }

    /// Creates a [`Weak`] reference to the [`Paragraph`].
    ///
    /// This is useful to avoid cloning the [`Paragraph`] when
    /// referential guarantees are unnecessary. For instance,
    /// when creating a rendering tree.
    pub fn downgrade(&self) -> Weak {
        let paragraph = self.internal();

        Weak {
            raw: Arc::downgrade(paragraph),
            min_bounds: paragraph.min_bounds,
            align_x: paragraph.align_x,
            align_y: paragraph.align_y,
        }
    }

    /// Reflows this paragraph using an exclusively owned font system, without the global font lock.
    ///
    /// The font database must preserve the face identities used to shape this paragraph.
    /// Existing paragraph clones keep their geometry; hinting, alignment and ellipsis are retained.
    pub fn resize_owned(&mut self, font_system: &mut cosmic_text::FontSystem, bounds: Size) {
        let paragraph = Arc::make_mut(&mut self.0);
        paragraph.buffer.set_size(
            Some(bounds.width * paragraph.hint_factor),
            Some(bounds.height * paragraph.hint_factor),
        );
        paragraph.buffer.set_ellipsize(text::to_ellipsize(
            paragraph.ellipsis,
            bounds.height * paragraph.hint_factor,
        ));
        paragraph.buffer.shape_until_scroll(font_system, false);
        paragraph.min_bounds = text::align(&mut paragraph.buffer, font_system, paragraph.align_x)
            / paragraph.hint_factor;
        paragraph.bounds = bounds;
    }

    fn internal(&self) -> &Arc<Internal> {
        &self.0
    }
}

impl core::text::Paragraph for Paragraph {
    fn with_text(text: Text<&str>) -> Self {
        log::trace!("Allocating plain paragraph: {}", text.content);

        let mut font_system = text::font_system().write().expect("Write font system");

        let (hint, hint_factor) = match text::hint_factor(text.size, text.hint_factor) {
            Some(hint_factor) => (true, hint_factor),
            _ => (false, 1.0),
        };

        let mut buffer = cosmic_text::Buffer::new(
            font_system.raw(),
            cosmic_text::Metrics::new(
                f32::from(text.size) * hint_factor,
                f32::from(text.line_height.to_absolute(text.size)) * hint_factor,
            ),
        );

        if hint {
            buffer.set_hinting(cosmic_text::Hinting::Enabled);
        }

        buffer.set_size(
            Some(text.bounds.width * hint_factor),
            Some(text.bounds.height * hint_factor),
        );

        buffer.set_wrap(text::to_wrap(text.wrapping));
        buffer.set_ellipsize(text::to_ellipsize(
            text.ellipsis,
            text.bounds.height * hint_factor,
        ));

        buffer.set_text(
            text.content,
            &text::to_attributes(text.font),
            text::to_shaping_for(text.shaping, text.content, text.font),
            None,
        );
        buffer.shape_until_scroll(font_system.raw(), false);

        let min_bounds = text::align(&mut buffer, font_system.raw(), text.align_x) / hint_factor;

        Self(Arc::new(Internal {
            buffer,
            hint,
            hint_factor,
            font: text.font,
            align_x: text.align_x,
            align_y: text.align_y,
            shaping: text.shaping,
            wrapping: text.wrapping,
            ellipsis: text.ellipsis,
            bounds: text.bounds,
            min_bounds,
            version: font_system.version(),
        }))
    }

    fn with_spans<Link>(text: Text<&[Span<'_, Link>]>) -> Self {
        log::trace!("Allocating rich paragraph: {} spans", text.content.len());

        let mut font_system = text::font_system().write().expect("Write font system");

        let (hint, hint_factor) = match text::hint_factor(text.size, text.hint_factor) {
            Some(hint_factor) => (true, hint_factor),
            _ => (false, 1.0),
        };

        let mut buffer = cosmic_text::Buffer::new(
            font_system.raw(),
            cosmic_text::Metrics::new(
                f32::from(text.size) * hint_factor,
                f32::from(text.line_height.to_absolute(text.size)) * hint_factor,
            ),
        );

        if hint {
            buffer.set_hinting(cosmic_text::Hinting::Enabled);
        }

        buffer.set_size(
            Some(text.bounds.width * hint_factor),
            Some(text.bounds.height * hint_factor),
        );

        buffer.set_wrap(text::to_wrap(text.wrapping));
        buffer.set_ellipsize(text::to_ellipsize(
            text.ellipsis,
            text.bounds.height * hint_factor,
        ));

        buffer.set_rich_text(
            text.content.iter().enumerate().map(|(i, span)| {
                let attrs = text::to_attributes(span.font.unwrap_or(text.font));

                let attrs = match (span.size, span.line_height) {
                    (None, None) => attrs,
                    _ => {
                        let size = span.size.unwrap_or(text.size);

                        attrs.metrics(cosmic_text::Metrics::new(
                            f32::from(size) * hint_factor,
                            f32::from(
                                span.line_height
                                    .unwrap_or(text.line_height)
                                    .to_absolute(size),
                            ) * hint_factor,
                        ))
                    }
                };

                let attrs = if let Some(color) = span.color {
                    attrs.color(text::to_color(color))
                } else {
                    attrs
                };

                let attrs = attrs.padding(cosmic_text::SpanPadding {
                    start: span.padding.left,
                    end: span.padding.right,
                });

                (span.text.as_ref(), attrs.metadata(i))
            }),
            &text::to_attributes(text.font),
            cosmic_text::Shaping::Advanced,
            None,
        );

        buffer.shape_until_scroll(font_system.raw(), false);

        let min_bounds = text::align(&mut buffer, font_system.raw(), text.align_x) / hint_factor;

        Self(Arc::new(Internal {
            buffer,
            hint,
            hint_factor,
            font: text.font,
            align_x: text.align_x,
            align_y: text.align_y,
            shaping: text.shaping,
            wrapping: text.wrapping,
            ellipsis: text.ellipsis,
            bounds: text.bounds,
            min_bounds,
            version: font_system.version(),
        }))
    }

    fn resize(&mut self, new_bounds: Size) {
        let paragraph = Arc::make_mut(&mut self.0);

        let mut font_system = text::font_system().write().expect("Write font system");

        paragraph.buffer.set_size(
            Some(new_bounds.width * paragraph.hint_factor),
            Some(new_bounds.height * paragraph.hint_factor),
        );
        // The ellipsize height limit is bounds-derived state that `set_size`
        // does not touch. Both constructors seed it from the bounds they were
        // given, so without this a paragraph reused through the resize fast
        // path keeps ellipsizing at the height it was built with: growing
        // truncates earlier than it needs to, shrinking strands the ellipsis
        // on a line that is no longer visible. Re-seed it the way the
        // constructors do, before the reflow below.
        paragraph.buffer.set_ellipsize(text::to_ellipsize(
            paragraph.ellipsis,
            new_bounds.height * paragraph.hint_factor,
        ));
        paragraph
            .buffer
            .shape_until_scroll(font_system.raw(), false);

        let min_bounds = text::align(&mut paragraph.buffer, font_system.raw(), paragraph.align_x)
            / paragraph.hint_factor;

        paragraph.bounds = new_bounds;
        paragraph.min_bounds = min_bounds;
    }

    fn compare(&self, text: Text<()>) -> core::text::Difference {
        let font_system = text::font_system().read().expect("Read font system");
        let paragraph = self.internal();
        let metrics = paragraph.buffer.metrics();

        if paragraph.version != font_system.version
            || metrics.font_size != text.size.0 * paragraph.hint_factor
            || metrics.line_height
                != text.line_height.to_absolute(text.size).0 * paragraph.hint_factor
            || paragraph.font != text.font
            || paragraph.shaping != text.shaping
            || paragraph.wrapping != text.wrapping
            || paragraph.ellipsis != text.ellipsis
            || paragraph.align_x != text.align_x
            || paragraph.align_y != text.align_y
            || paragraph.hint.then_some(paragraph.hint_factor)
                != text::hint_factor(text.size, text.hint_factor)
        {
            core::text::Difference::Shape
        } else if paragraph.bounds != text.bounds {
            core::text::Difference::Bounds
        } else {
            core::text::Difference::None
        }
    }

    fn hint_factor(&self) -> Option<f32> {
        self.0.hint.then_some(self.0.hint_factor)
    }

    fn size(&self) -> Pixels {
        Pixels(self.0.buffer.metrics().font_size / self.0.hint_factor)
    }

    fn font(&self) -> Font {
        self.0.font
    }

    fn line_height(&self) -> LineHeight {
        LineHeight::Absolute(Pixels(
            self.0.buffer.metrics().line_height / self.0.hint_factor,
        ))
    }

    fn align_x(&self) -> Alignment {
        self.internal().align_x
    }

    fn align_y(&self) -> alignment::Vertical {
        self.internal().align_y
    }

    fn wrapping(&self) -> Wrapping {
        self.0.wrapping
    }

    fn ellipsis(&self) -> Ellipsis {
        self.0.ellipsis
    }

    fn shaping(&self) -> Shaping {
        self.0.shaping
    }

    fn bounds(&self) -> Size {
        self.0.bounds
    }

    fn min_bounds(&self) -> Size {
        self.internal().min_bounds
    }

    fn hit_test(&self, point: Point) -> Option<Hit> {
        let cursor = self
            .internal()
            .buffer
            .hit(point.x * self.0.hint_factor, point.y * self.0.hint_factor)?;

        Some(Hit::CharOffset(cursor.index))
    }

    fn hit_test_source_offset(&self, point: Point) -> Option<usize> {
        let internal = self.internal();
        let cursor = internal
            .buffer
            .hit(point.x * self.0.hint_factor, point.y * self.0.hint_factor)?;
        let preceding = internal
            .buffer
            .lines
            .iter()
            .take(cursor.line)
            .map(|line| line.text().len() + line.ending().as_str().len())
            .sum::<usize>();
        Some(preceding + cursor.index)
    }

    fn hit_span(&self, point: Point) -> Option<usize> {
        let internal = self.internal();

        let cursor = internal
            .buffer
            .hit(point.x * self.0.hint_factor, point.y * self.0.hint_factor)?;
        let line = internal.buffer.lines.get(cursor.line)?;

        if cursor.index >= line.text().len() {
            return None;
        }

        let index = match cursor.affinity {
            cosmic_text::Affinity::Before => cursor.index.saturating_sub(1),
            cosmic_text::Affinity::After => cursor.index,
        };

        let mut hit = None;
        let glyphs = line
            .layout_opt()
            .as_ref()?
            .iter()
            .flat_map(|line| line.glyphs.iter());

        for glyph in glyphs {
            if glyph.start <= index && index < glyph.end {
                hit = Some(glyph);
                break;
            }
        }

        Some(hit?.metadata)
    }

    fn span_bounds(&self, index: usize) -> Vec<Rectangle> {
        let internal = self.internal();

        let scale = 1.0 / internal.hint_factor;

        let mut bounds = Vec::new();
        let mut current = None;
        let mut current_baseline = 0.0;

        let mut y = 0.0;
        let buffer_height = internal.buffer.metrics().line_height;
        let glyphs = internal
            .buffer
            .lines
            .iter()
            .filter_map(|paragraph| paragraph.layout_opt().map(Vec::as_slice))
            .flat_map(|lines| lines.iter())
            .flat_map(move |line| {
                let line_height = line.line_height(buffer_height);
                let ink_height = line.max_ascent + line.max_descent;

                // The renderer centers the line's ink within the line
                // height and places the baseline `max_ascent` below the
                // top of the ink:
                let baseline = y + (line_height - ink_height) / 2.0 + line.max_ascent;

                let glyphs = line.glyphs.iter().map(move |glyph| (baseline, glyph));

                y += line_height;

                glyphs
            })
            .skip_while(|(_, glyph)| glyph.metadata != index)
            .take_while(|(_, glyph)| glyph.metadata == index);

        for (baseline, glyph) in glyphs {
            // Glyphs can be offset from the line's baseline (e.g. by complex
            // scripts); mirror the anchor used in `LayoutGlyph::physical`:
            let anchor = baseline + glyph.y - glyph.font_size * glyph.y_offset;
            let ink_top = anchor - glyph.ascender;
            let ink_bottom = anchor + glyph.descender;

            match current.as_mut() {
                None => {
                    current_baseline = baseline;
                    current = Some(
                        Rectangle::new(
                            Point::new(glyph.x, ink_top),
                            Size::new(glyph.w, ink_bottom - ink_top),
                        ) * scale,
                    );
                }
                Some(current) if baseline != current_baseline => {
                    bounds.push(*current);
                    current_baseline = baseline;
                    *current = Rectangle::new(
                        Point::new(glyph.x, ink_top),
                        Size::new(glyph.w, ink_bottom - ink_top),
                    ) * scale;
                }
                Some(current) => {
                    // Union the glyph's ink with the span's bounds on this
                    // line:
                    let left = current.x.min(glyph.x * scale);
                    let top = current.y.min(ink_top * scale);
                    let right = (current.x + current.width).max((glyph.x + glyph.w) * scale);
                    let bottom = (current.y + current.height).max(ink_bottom * scale);
                    *current = Rectangle::new(
                        Point::new(left, top),
                        Size::new(right - left, bottom - top),
                    );
                }
            }
        }

        bounds.extend(current);
        bounds
    }

    fn range_bounds(&self, range: std::ops::Range<usize>) -> Vec<Rectangle> {
        if range.start >= range.end {
            return Vec::new();
        }

        let internal = self.internal();
        let mut base = 0usize;
        let mut start = None;
        let mut end = None;
        for (line_i, line) in internal.buffer.lines.iter().enumerate() {
            let text_len = line.text().len();
            let ending_len = line.ending().as_str().len();
            if start.is_none() && range.start <= base + text_len {
                start = Some(cosmic_text::Cursor::new(
                    line_i,
                    range.start.saturating_sub(base),
                ));
            }
            if range.end <= base + text_len || line_i + 1 == internal.buffer.lines.len() {
                end = Some(cosmic_text::Cursor::new(
                    line_i,
                    range.end.saturating_sub(base).min(text_len),
                ));
                break;
            }
            base += text_len + ending_len;
        }
        let (Some(start), Some(end)) = (start, end) else {
            return Vec::new();
        };
        let scale = 1.0 / self.0.hint_factor;
        internal
            .buffer
            .layout_runs()
            // `LayoutRun::highlight` only compares glyph indices against the
            // cursors when the run sits on the cursor's own buffer line:
            //
            //     (start.line != line_i || c_end > start.index)
            //         && (end.line != line_i || c_start < end.index)
            //
            // For a run on any OTHER buffer line both halves are vacuously
            // true, so it reports the entire line as highlighted. That is by
            // design — `cosmic_text`'s own editor calls it only for runs in
            // `start.line..=end.line` (see `Editor::draw`) — but this method
            // used to hand it every run in the paragraph. A range confined to
            // one hard line therefore came back with a full-width rectangle
            // for every other hard line, which painted a read-only selection
            // over the whole document when the source contained `\n`
            // (enzUI #1854; a soft-wrapped paragraph is one buffer line and
            // so never exposed it).
            .filter(|run| run.line_i >= start.line && run.line_i <= end.line)
            // A soft-wrapped paragraph is one buffer line of many runs, and
            // `highlight` segments every glyph of a run into graphemes. On the
            // range's first and last lines, a run with no glyph inside the
            // range is skipped by byte offsets alone, so bounding a word in a
            // long paragraph walks one run, not the paragraph.
            .filter(|run| {
                (run.line_i != start.line || run.glyphs.iter().any(|glyph| glyph.end > start.index))
                    && (run.line_i != end.line
                        || run.glyphs.iter().any(|glyph| glyph.start < end.index))
            })
            .flat_map(|run| {
                let y = run.line_top;
                let height = run.line_height;
                run.highlight(start, end).map(move |(x, width)| {
                    Rectangle::new(Point::new(x, y), Size::new(width, height)) * scale
                })
            })
            .collect()
    }

    fn grapheme_bounds(&self) -> Option<Vec<core::text::paragraph::GraphemeBounds>> {
        use unicode_segmentation::UnicodeSegmentation;

        let internal = self.internal();
        let mut line_base = Vec::with_capacity(internal.buffer.lines.len());
        let mut base = 0usize;
        for line in &internal.buffer.lines {
            line_base.push(base);
            base += line.text().len() + line.ending().as_str().len();
        }
        let scale = 1.0 / self.0.hint_factor;
        let mut graphemes = Vec::new();
        // The same split as `LayoutRun::highlight`: a glyph's cluster is
        // divided evenly among its graphemes.
        for run in internal.buffer.layout_runs() {
            let base = line_base.get(run.line_i).copied().unwrap_or(0);
            for glyph in run.glyphs {
                let cluster = &run.text[glyph.start..glyph.end];
                let total = cluster.graphemes(true).count().max(1);
                let width = glyph.w / total as f32;
                let mut x = glyph.x;
                for (offset, grapheme) in cluster.grapheme_indices(true) {
                    let start = base + glyph.start + offset;
                    graphemes.push(core::text::paragraph::GraphemeBounds {
                        range: start..start + grapheme.len(),
                        bounds: Rectangle::new(
                            Point::new(x, run.line_top),
                            Size::new(width, run.line_height),
                        ) * scale,
                        rtl: glyph.level.is_rtl(),
                    });
                    x += width;
                }
            }
        }
        Some(graphemes)
    }

    fn logical_cursor(&self, offset: usize, affinity: Affinity) -> Option<CursorPosition> {
        let internal = self.internal();
        let buffer = &internal.buffer;

        // Map the source byte offset onto the buffer line it falls in. Buffer
        // lines are separated by hard line breaks; `offset` is a byte index into
        // the full content, so we walk the lines accumulating their lengths
        // (plus the line-ending bytes) until we find the one that contains it.
        let mut base = 0;
        let mut target = None;

        for (line_i, line) in buffer.lines.iter().enumerate() {
            let text_len = line.text().len();
            let ending_len = line.ending().as_str().len();

            // The offset belongs to this line if it lands within its text, or at
            // its trailing edge when this is the last line (so the logical end of
            // the paragraph resolves against the final line rather than falling
            // off the end).
            let is_last = line_i + 1 == buffer.lines.len();
            let within = offset <= base + text_len
                && (offset >= base)
                && (offset < base + text_len + ending_len || is_last);

            if within {
                target = Some((line_i, offset - base));
                break;
            }

            base += text_len + ending_len;
        }

        let (line_i, index) = target?;

        // Resolve the caret from a *glyph edge* rather than from
        // `cursor_position`, so that affinity actually disambiguates the two
        // valid positions at a direction boundary. `cursor_position` matches on
        // `index == glyph.start` and ignores affinity, so at a boundary both
        // affinities collapse to the same glyph; here we honour affinity
        // explicitly:
        //
        // * `Before` binds to the glyph whose cluster *ends* at `index` (the
        //   trailing edge of the preceding logical glyph). For an LTR glyph the
        //   trailing edge is its right edge; for RTL, its left edge.
        // * `After` binds to the glyph whose cluster *starts* at `index` (the
        //   leading edge of the following logical glyph). LTR leading edge is
        //   the left edge; RTL, the right edge.
        //
        // Both cases carry the bidi level of the glyph they bound to, which is
        // what makes the two boundary positions distinguishable.
        let mut resolved: Option<(f32, f32, u8)> = None;

        'runs: for run in buffer.layout_runs() {
            if run.line_i != line_i {
                continue;
            }

            // First: a glyph whose cluster strictly contains the index (offset
            // interior to a multi-byte cluster). Split proportionally.
            for glyph in run.glyphs {
                if index > glyph.start && index < glyph.end {
                    let frac =
                        (index - glyph.start) as f32 / (glyph.end - glyph.start).max(1) as f32;
                    let x = if glyph.level.is_rtl() {
                        glyph.x + glyph.w - glyph.w * frac
                    } else {
                        glyph.x + glyph.w * frac
                    };
                    resolved = Some((x, run.line_top, glyph.level.number()));
                    break 'runs;
                }
            }

            let edge = |glyph: &cosmic_text::LayoutGlyph, leading: bool| {
                // leading = start edge of the glyph in logical order.
                let at_left = if glyph.level.is_rtl() {
                    !leading
                } else {
                    leading
                };
                if at_left { glyph.x } else { glyph.x + glyph.w }
            };

            match affinity {
                Affinity::Before => {
                    if let Some(glyph) = run.glyphs.iter().find(|g| g.end == index) {
                        resolved = Some((edge(glyph, false), run.line_top, glyph.level.number()));
                        break;
                    }
                }
                Affinity::After => {
                    if let Some(glyph) = run.glyphs.iter().find(|g| g.start == index) {
                        resolved = Some((edge(glyph, true), run.line_top, glyph.level.number()));
                        break;
                    }
                }
            }
        }

        // Fallback for affinities that found no matching glyph edge (for
        // example an empty line, or a boundary where only the opposite side has
        // a glyph): defer to the shaping engine's own cursor placement.
        if resolved.is_none() {
            let ct_affinity = match affinity {
                Affinity::Before => cosmic_text::Affinity::Before,
                Affinity::After => cosmic_text::Affinity::After,
            };
            let cursor = cosmic_text::Cursor::new_with_affinity(line_i, index, ct_affinity);
            for run in buffer.layout_runs() {
                if run.line_i != line_i {
                    continue;
                }
                if let Some(x) = run.cursor_position(&cursor) {
                    let level = run
                        .glyphs
                        .iter()
                        .find(|g| index >= g.start && index < g.end)
                        .map(|g| g.level.number())
                        .unwrap_or(if run.rtl { 1 } else { 0 });
                    resolved = Some((x, run.line_top, level));
                    break;
                }
            }
        }

        let (x, line_top, level) = resolved?;

        Some(CursorPosition {
            point: Point::new(x / self.0.hint_factor, line_top / self.0.hint_factor),
            offset,
            affinity,
            level,
        })
    }

    fn logical_end_cursor(&self) -> Option<CursorPosition> {
        let buffer = &self.internal().buffer;

        // The logical end is after the last character of the last buffer line.
        // Its byte offset is the sum of every line's text plus line-ending
        // bytes, minus the trailing ending of the final line (there is no caret
        // position beyond a final newline within the same paragraph line).
        let mut offset = 0;
        let last = buffer.lines.len().checked_sub(1)?;
        for (line_i, line) in buffer.lines.iter().enumerate() {
            offset += line.text().len();
            if line_i != last {
                offset += line.ending().as_str().len();
            }
        }

        // `Before` affinity binds the end offset to the preceding (last logical)
        // glyph, which is what a trailing caret wants.
        self.logical_cursor(offset, Affinity::Before)
    }

    fn grapheme_position(&self, line: usize, index: usize) -> Option<Point> {
        use unicode_segmentation::UnicodeSegmentation;

        let run = self.internal().buffer.layout_runs().nth(line)?;

        // The caret before grapheme `index` (or after the last one) sits on
        // the leading edge of that grapheme within its glyph cluster: the
        // left edge in left-to-right text, the right edge in right-to-left
        // text (enzICED). Glyphs are stored in visual order, so the cluster
        // is found by byte offset, not by walking glyphs.
        let starts: Vec<usize> = run
            .text
            .grapheme_indices(false)
            .map(|(start, _)| start)
            .collect();
        let (byte, after) = match starts.get(index) {
            Some(&start) => (start, false),
            None => (*starts.last()?, true),
        };
        let glyph = run
            .glyphs
            .iter()
            .find(|glyph| glyph.start <= byte && byte < glyph.end)
            .or_else(|| run.glyphs.last())?;

        let in_cluster = run.text[glyph.start..glyph.end]
            .graphemes(false)
            .count()
            .max(1);
        let before = run.text[glyph.start..byte.max(glyph.start)]
            .graphemes(false)
            .count()
            + usize::from(after);
        let fraction = (before as f32 / in_cluster as f32).min(1.0);
        let advance = if glyph.level.is_rtl() {
            glyph.w * (1.0 - fraction)
        } else {
            glyph.w * fraction
        };

        Some(Point::new(
            (glyph.x + glyph.x_offset * glyph.font_size + advance) / self.0.hint_factor,
            (glyph.y - glyph.y_offset * glyph.font_size) / self.0.hint_factor,
        ))
    }
}

impl Default for Paragraph {
    fn default() -> Self {
        Self(Arc::new(Internal::default()))
    }
}

impl fmt::Debug for Paragraph {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let paragraph = self.internal();

        f.debug_struct("Paragraph")
            .field("font", &paragraph.font)
            .field("shaping", &paragraph.shaping)
            .field("horizontal_alignment", &paragraph.align_x)
            .field("vertical_alignment", &paragraph.align_y)
            .field("bounds", &paragraph.bounds)
            .field("min_bounds", &paragraph.min_bounds)
            .finish()
    }
}

impl PartialEq for Internal {
    fn eq(&self, other: &Self) -> bool {
        self.font == other.font
            && self.shaping == other.shaping
            && self.align_x == other.align_x
            && self.align_y == other.align_y
            && self.bounds == other.bounds
            && self.min_bounds == other.min_bounds
            && self.buffer.metrics() == other.buffer.metrics()
    }
}

impl Default for Internal {
    fn default() -> Self {
        Self {
            buffer: cosmic_text::Buffer::new_empty(cosmic_text::Metrics {
                font_size: 1.0,
                line_height: 1.0,
            }),
            font: Font::default(),
            shaping: Shaping::default(),
            wrapping: Wrapping::default(),
            ellipsis: Ellipsis::default(),
            align_x: Alignment::Default,
            align_y: alignment::Vertical::Top,
            bounds: Size::ZERO,
            min_bounds: Size::ZERO,
            version: text::Version::default(),
            hint: false,
            hint_factor: 1.0,
        }
    }
}

/// A weak reference to a [`Paragraph`].
#[derive(Debug, Clone)]
pub struct Weak {
    raw: sync::Weak<Internal>,
    /// The minimum bounds of the [`Paragraph`].
    pub min_bounds: Size,
    /// The horizontal alignment of the [`Paragraph`].
    pub align_x: Alignment,
    /// The vertical alignment of the [`Paragraph`].
    pub align_y: alignment::Vertical,
}

impl Weak {
    /// Tries to update the reference into a [`Paragraph`].
    pub fn upgrade(&self) -> Option<Paragraph> {
        self.raw.upgrade().map(Paragraph)
    }

    /// The address of the paragraph this refers to. It is not reused for
    /// another paragraph while this reference lives.
    pub fn as_ptr(&self) -> *const () {
        self.raw.as_ptr().cast()
    }
}

impl PartialEq for Weak {
    fn eq(&self, other: &Self) -> bool {
        match (self.raw.upgrade(), other.raw.upgrade()) {
            (Some(p1), Some(p2)) => p1 == p2,
            _ => false,
        }
    }
}

#[cfg(test)]
mod logical_cursor_tests {
    use super::*;
    use crate::core::text::{Affinity, Paragraph as _};
    use crate::core::{Font, Pixels, Size};

    fn para(content: &str) -> Paragraph {
        Paragraph::with_text(Text {
            content,
            bounds: Size::new(1000.0, 1000.0),
            size: Pixels(16.0),
            line_height: LineHeight::Relative(1.2),
            font: Font::default(),
            align_x: Alignment::Default,
            align_y: crate::core::alignment::Vertical::Top,
            shaping: Shaping::Advanced,
            wrapping: Wrapping::None,
            ellipsis: Ellipsis::default(),
            hint_factor: None,
        })
    }

    // Visual x of the rightmost glyph edge on line 0 (what the old max-x
    // heuristic would return for a "logical end").
    fn rightmost_glyph_edge(p: &Paragraph) -> f32 {
        p.buffer()
            .layout_runs()
            .flat_map(|run| run.glyphs.iter().map(|g| g.x + g.w).collect::<Vec<_>>())
            .fold(f32::MIN, f32::max)
    }

    // The Hebrew word "שלום" shapes to real RTL glyphs (bidi level 1) under the
    // fallback font, so these bidi assertions hold independent of glyph
    // availability: bidi *levels* come from unicode-bidi, not glyph coverage.
    const MIXED: &str = "abcשלום"; // "abc" (LTR) + Hebrew (RTL); logical end is Hebrew

    #[test]
    fn mixed_bidi_logical_end_is_visually_interior_not_rightmost() {
        let p = para(MIXED);
        let end = p.logical_end_cursor().expect("logical end resolves");

        // The logical end is the last Hebrew character. In visual order the
        // Hebrew run is reversed, so the logical-end glyph is the *leftmost*
        // Hebrew glyph, sitting to the RIGHT of the Latin "abc" but to the LEFT
        // of the other Hebrew glyphs -> visually interior.
        let rightmost = rightmost_glyph_edge(&p);

        // Offset is the byte length of the whole string.
        assert_eq!(end.offset, MIXED.len());
        // The glyph is RTL.
        assert!(
            end.is_rtl(),
            "logical-end glyph should be RTL, level={}",
            end.level
        );
        assert_eq!(end.level % 2, 1);

        // The caret is NOT at the rightmost glyph edge. This is the assertion
        // that fails under a max-x heuristic. The gap is large (~35px), well
        // clear of any downstream clamp.
        assert!(
            end.point.x < rightmost - 20.0,
            "logical end x={} must be well left of rightmost edge={}",
            end.point.x,
            rightmost
        );

        // And it is to the right of the Latin block (past "abc"), i.e. truly
        // interior rather than at the left edge.
        assert!(
            end.point.x > 20.0,
            "logical end x={} should sit past the Latin run",
            end.point.x
        );
    }

    #[test]
    fn affinities_distinguishable_at_direction_boundary() {
        // Offset 3 is the boundary between LTR "abc" (ends at 3) and the RTL
        // Hebrew run (starts at 3).
        let p = para(MIXED);
        let before = p
            .logical_cursor(3, Affinity::Before)
            .expect("before resolves");
        let after = p
            .logical_cursor(3, Affinity::After)
            .expect("after resolves");

        // If these collapse to the same point the affinity field is decorative.
        assert!(
            (before.point.x - after.point.x).abs() > 5.0,
            "affinities must differ: before={} after={}",
            before.point.x,
            after.point.x
        );

        // Before binds to the trailing edge of LTR "c" (level 0, ~right edge of
        // the Latin run); After binds to the RTL run (level 1).
        assert_eq!(before.level % 2, 0, "before side is LTR");
        assert_eq!(after.level % 2, 1, "after side is RTL");
        assert!(after.point.x > before.point.x);
    }

    #[test]
    fn pure_ltr_logical_end_is_rightmost() {
        let p = para("hello");
        let end = p.logical_end_cursor().expect("resolves");
        let rightmost = rightmost_glyph_edge(&p);

        assert_eq!(end.offset, 5);
        assert!(!end.is_rtl(), "LTR run, level={}", end.level);
        // For pure LTR the logical end IS the rightmost edge.
        assert!(
            (end.point.x - rightmost).abs() < 0.5,
            "LTR logical end x={} should equal rightmost={}",
            end.point.x,
            rightmost
        );
    }

    #[test]
    fn pure_rtl_logical_end_is_leftmost() {
        // Pure Hebrew: paragraph direction RTL, logical end is visually the
        // LEFTMOST glyph edge.
        let p = para("שלום");
        let end = p.logical_end_cursor().expect("resolves");

        assert_eq!(end.offset, "שלום".len());
        assert!(end.is_rtl(), "RTL run, level={}", end.level);

        // Leftmost glyph edge on the line.
        let leftmost = p
            .buffer()
            .layout_runs()
            .flat_map(|run| run.glyphs.iter().map(|g| g.x).collect::<Vec<_>>())
            .fold(f32::MAX, f32::min);
        assert!(
            (end.point.x - leftmost).abs() < 0.5,
            "RTL logical end x={} should equal leftmost glyph x={}",
            end.point.x,
            leftmost
        );
    }

    #[test]
    fn empty_string_resolves_to_origin() {
        let p = para("");
        let end = p.logical_end_cursor().expect("empty resolves");
        assert_eq!(end.offset, 0);
        assert_eq!(end.point.x, 0.0);
        assert_eq!(end.point.y, 0.0);
    }

    #[test]
    fn single_grapheme_end_is_glyph_right_edge() {
        let p = para("x");
        let end = p.logical_end_cursor().expect("resolves");
        assert_eq!(end.offset, 1);
        assert!(!end.is_rtl());
        let rightmost = rightmost_glyph_edge(&p);
        assert!((end.point.x - rightmost).abs() < 0.5);
        // A single glyph starts at x=0, so the end is strictly positive.
        assert!(end.point.x > 0.0);
    }

    #[test]
    fn out_of_range_offset_returns_none_within_content() {
        // An offset far past the content must not resolve (no panic, no bogus
        // point). Our line-walk only accepts offsets landing on a line.
        let p = para("hi");
        assert!(p.logical_cursor(999, Affinity::Before).is_none());
    }
}

#[cfg(test)]
mod ellipsize_invariant_tests {
    use super::*;
    use crate::core::text::Paragraph as _;
    use crate::core::{Font, Pixels, Size};

    const CONTENT: &str = "Alpha bravo charlie delta echo foxtrot golf hotel \
                           india juliet kilo lima mike november oscar papa \
                           quebec romeo sierra tango uniform victor whiskey.";

    const WIDTH: f32 = 180.0;
    const SHORT: f32 = 48.0;
    const TALL: f32 = 400.0;

    fn spec(bounds: Size, ellipsis: Ellipsis) -> Text<&'static str> {
        Text {
            content: CONTENT,
            bounds,
            size: Pixels(16.0),
            line_height: LineHeight::Relative(1.2),
            font: Font::default(),
            align_x: Alignment::Default,
            align_y: crate::core::alignment::Vertical::Top,
            shaping: Shaping::Advanced,
            wrapping: Wrapping::Word,
            ellipsis,
            hint_factor: None,
        }
    }

    fn plain(bounds: Size, ellipsis: Ellipsis) -> Paragraph {
        Paragraph::with_text(spec(bounds, ellipsis))
    }

    fn rich(bounds: Size, ellipsis: Ellipsis) -> Paragraph {
        let spans = [Span::<'_, ()>::new(CONTENT)];
        let base = spec(bounds, ellipsis);

        Paragraph::with_spans(Text {
            content: &spans[..],
            bounds: base.bounds,
            size: base.size,
            line_height: base.line_height,
            font: base.font,
            align_x: base.align_x,
            align_y: base.align_y,
            shaping: base.shaping,
            wrapping: base.wrapping,
            ellipsis: base.ellipsis,
            hint_factor: base.hint_factor,
        })
    }

    /// The invariant both call sites exist to maintain: the buffer's ellipsize
    /// state is a pure function of the requested strategy and the bounds the
    /// paragraph currently holds.
    ///
    /// Constructors establish it; `resize` must preserve it. Neither half is
    /// sufficient alone -- a constructor that skips it leaves a paragraph that
    /// never ellipsizes, and a `resize` that skips it leaves one ellipsizing at
    /// a height it no longer has.
    #[track_caller]
    fn assert_invariant(paragraph: &Paragraph, what: &str) {
        let internal = paragraph.internal();

        assert_eq!(
            paragraph.buffer().ellipsize(),
            text::to_ellipsize(
                internal.ellipsis,
                internal.bounds.height * internal.hint_factor,
            ),
            "{what}: buffer ellipsize does not match the requested strategy at \
             the current bounds",
        );
    }

    /// Laid-out shape, coarse enough to be font-independent: per run, the
    /// source line, the glyph count, and how many ellipsis markers it carries.
    ///
    /// cosmic-text emits the marker as a synthesised glyph with an empty source
    /// cluster (`start == end`), which is what distinguishes it from a real
    /// glyph without depending on the font's `…` coverage.
    fn shape(paragraph: &Paragraph) -> Vec<(usize, usize, usize)> {
        paragraph
            .buffer()
            .layout_runs()
            .map(|run| {
                let markers = run.glyphs.iter().filter(|g| g.start == g.end).count();

                (run.line_i, run.glyphs.len(), markers)
            })
            .collect()
    }

    fn marker_count(paragraph: &Paragraph) -> usize {
        shape(paragraph).iter().map(|&(_, _, m)| m).sum()
    }

    #[test]
    fn both_constructors_install_the_requested_strategy() {
        let bounds = Size::new(WIDTH, SHORT);

        assert_invariant(&plain(bounds, Ellipsis::End), "with_text");
        assert_invariant(&rich(bounds, Ellipsis::End), "with_spans");
    }

    #[test]
    fn resize_preserves_the_invariant_in_both_directions() {
        for ellipsis in [Ellipsis::Start, Ellipsis::Middle, Ellipsis::End] {
            for build in [plain as fn(Size, Ellipsis) -> Paragraph, rich] {
                let mut grown = build(Size::new(WIDTH, SHORT), ellipsis);
                grown.resize(Size::new(WIDTH, TALL));
                assert_invariant(&grown, "resize grow");

                let mut shrunk = build(Size::new(WIDTH, TALL), ellipsis);
                shrunk.resize(Size::new(WIDTH, SHORT));
                assert_invariant(&shrunk, "resize shrink");
            }
        }
    }

    #[test]
    fn a_resized_paragraph_lays_out_like_a_fresh_one() {
        for build in [plain as fn(Size, Ellipsis) -> Paragraph, rich] {
            let target = Size::new(WIDTH, SHORT);

            let mut resized = build(Size::new(WIDTH, TALL), Ellipsis::End);
            resized.resize(target);

            assert_eq!(
                shape(&resized),
                shape(&build(target, Ellipsis::End)),
                "a resized paragraph must lay out like one built at the same bounds",
            );
        }
    }

    #[test]
    fn resizing_from_an_unbounded_height_starts_ellipsizing() {
        // An unbounded paragraph cannot ellipsize: the limit is never exceeded.
        // Clamping it must therefore both refresh the limit and produce a
        // marker that was previously absent -- the case a stale limit hides
        // most completely, since nothing about the old value looks wrong.
        for build in [plain as fn(Size, Ellipsis) -> Paragraph, rich] {
            let mut paragraph = build(Size::new(WIDTH, f32::INFINITY), Ellipsis::End);

            assert_eq!(
                marker_count(&paragraph),
                0,
                "an unbounded paragraph must not ellipsize",
            );

            paragraph.resize(Size::new(WIDTH, SHORT));

            assert_invariant(&paragraph, "resize from unbounded");
            assert!(
                marker_count(&paragraph) > 0,
                "a paragraph clamped to a height it overflows must ellipsize",
            );
        }
    }

    #[test]
    fn no_ellipsis_stays_none_across_resize() {
        for build in [plain as fn(Size, Ellipsis) -> Paragraph, rich] {
            let mut paragraph = build(Size::new(WIDTH, TALL), Ellipsis::None);
            paragraph.resize(Size::new(WIDTH, SHORT));

            assert_eq!(
                paragraph.buffer().ellipsize(),
                cosmic_text::Ellipsize::None,
                "`Ellipsis::None` carries no height and must stay inert",
            );
            assert_eq!(marker_count(&paragraph), 0);
        }
    }

    #[test]
    fn unchanged_bounds_do_not_reach_resize() {
        // `Plain::update` only calls `resize` for `Difference::Bounds`, so the
        // warm path is preserved by `compare` reporting no difference at all.
        let bounds = Size::new(WIDTH, SHORT);
        let paragraph = plain(bounds, Ellipsis::End);

        assert_eq!(
            paragraph.compare(spec(bounds, Ellipsis::End).with_content(())),
            core::text::Difference::None,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::text::Paragraph as _;

    fn rich_paragraph(content: &[Span<'_, ()>]) -> Paragraph {
        let text = Text {
            content,
            bounds: Size::new(1000.0, f32::INFINITY),
            size: Pixels(20.0),
            line_height: LineHeight::Relative(1.5),
            font: Font::default(),
            align_x: Alignment::Default,
            align_y: alignment::Vertical::Top,
            shaping: Shaping::default(),
            wrapping: Wrapping::default(),
            ellipsis: Ellipsis::default(),
            hint_factor: None,
        };

        Paragraph::with_spans(text)
    }

    /// Bounds should cover the ink of the span, without stretching to the
    /// line height.
    #[test]
    fn span_bounds_cover_ink() {
        let paragraph = rich_paragraph(&[Span::new("a"), Span::new("a").size(40.0)]);

        let small = paragraph.span_bounds(0);
        let big = paragraph.span_bounds(1);

        assert_eq!(small.len(), 1);
        assert_eq!(big.len(), 1);

        let buffer = paragraph.buffer();
        let line = &buffer.lines[0].layout_opt().unwrap()[0];
        let line_height = line.line_height(buffer.metrics().line_height);
        let ink_height = line.max_ascent + line.max_descent;
        let centering = (line_height - ink_height) / 2.0;

        // The larger span determines the line's ink:
        assert!((big[0].y - centering).abs() < 1e-2);
        assert!((big[0].height - ink_height).abs() < 1e-2);

        // Both spans share the line's baseline:
        let baseline = centering + line.max_ascent;
        assert!((small[0].y + line.max_ascent / 2.0 - baseline).abs() < 1e-2);
        assert!((big[0].y + line.max_ascent - baseline).abs() < 1e-2);

        // The smaller span is strictly inside the line's ink box...
        assert!(small[0].y > centering);
        assert!(small[0].y + small[0].height < centering + ink_height);
        assert!((small[0].height - ink_height / 2.0).abs() < 1e-2);

        // ...and the ink stays inside the line box:
        if ink_height < line_height {
            assert!(big[0].height < line_height);
        }
    }

    /// Bounds should cover the horizontal extent of the span's glyphs.
    #[test]
    fn span_bounds_cover_width() {
        let paragraph = rich_paragraph(&[Span::new("Hello"), Span::new(", world!")]);

        let first = paragraph.span_bounds(0);
        let second = paragraph.span_bounds(1);

        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);

        let buffer = paragraph.buffer();
        let line = &buffer.lines[0].layout_opt().unwrap()[0];

        // The first span starts at the beginning of the line and is a
        // proper prefix of it:
        assert!((first[0].x - 0.0).abs() < 1e-2);
        assert!(first[0].width > 0.0);
        assert!(first[0].width < line.w);

        // The second span starts where the first span ends, and ends at
        // the end of the line:
        assert!((second[0].x - (first[0].x + first[0].width)).abs() < 1e-2);
        assert!((second[0].x + second[0].width - line.w).abs() < 1e-2);
    }

    /// A span on multiple lines produces one bounds per line.
    #[test]
    fn span_bounds_cover_each_line() {
        let paragraph = rich_paragraph(&[Span::new("ab\ncd")]);

        let bounds = paragraph.span_bounds(0);

        assert_eq!(bounds.len(), 2);

        let buffer = paragraph.buffer();
        let line_height = buffer.metrics().line_height;

        for bounds in &bounds {
            assert!((bounds.x - 0.0).abs() < 1e-2);
            assert!(bounds.height > 0.0);
            assert!(bounds.height < line_height);
        }

        // The lines are stacked without overlap, and both lines have the
        // same ink height:
        assert!(bounds[0].y + bounds[0].height <= bounds[1].y + 1e-2);
        assert!((bounds[0].height - bounds[1].height).abs() < 1e-2);
    }

    /// One walk of the layout gives each grapheme the bounds `range_bounds`
    /// gives it, across soft wraps and a hard line break.
    ///
    /// Sabotage: number every buffer line's graphemes from byte 0 and the
    /// second line's ranges point at the first line.
    #[test]
    fn grapheme_bounds_agree_with_range_bounds() {
        let first = "wrapping words in a long first line ".repeat(8);
        let content = format!("{first}\nsecond line");
        let paragraph = rich_paragraph(&[Span::new(content.as_str())]);

        let graphemes = paragraph
            .grapheme_bounds()
            .expect("cosmic paragraphs answer");
        let second = first.len() + 1;
        assert!(
            graphemes.iter().any(|g| g.range == (second..second + 1)),
            "the second line's graphemes carry their source offsets"
        );
        let wrapped = graphemes
            .iter()
            .filter(|g| g.range.end <= first.len())
            .map(|g| g.bounds.y.to_bits())
            .collect::<std::collections::BTreeSet<_>>();
        assert!(wrapped.len() > 1, "the first line wraps");
        for grapheme in &graphemes {
            let ranged = paragraph.range_bounds(grapheme.range.clone());
            assert_eq!(ranged.len(), 1, "{grapheme:?}");
            let delta = (ranged[0].x - grapheme.bounds.x).abs()
                + (ranged[0].y - grapheme.bounds.y).abs()
                + (ranged[0].width - grapheme.bounds.width).abs();
            assert!(delta < 1e-3, "{grapheme:?} vs {:?}", ranged[0]);
        }
    }
}

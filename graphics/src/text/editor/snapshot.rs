// Modified by enzUI contributors. See FORK.md.
//! Bounded immutable editor text and caller-owned shaping, independent of live editing state.

use super::*;
use crate::core::renderer::presentation::Error;

mod reconciliation;
pub use reconciliation::Geometry;

const MAX_LINES: usize = 4096;
const MAX_BYTES: usize = 1024 * 1024;
const MAX_SPANS: usize = 16384;

/// Immutable editor text within a bounded scroll envelope, without history or live editor references.
#[derive(Debug)]
pub struct Snapshot {
    identity: Arc<()>,
    revision: Arc<()>,
    version: text::Version,
    source_metrics: Option<ScrollMetrics>,
    lines: Vec<cosmic_text::BufferLine>,
    first_line: usize,
    total_lines: usize,
    text_bytes: usize,
    metrics: cosmic_text::Metrics,
    wrap: cosmic_text::Wrap,
    ellipsize: cosmic_text::Ellipsize,
    monospace_width: Option<f32>,
    tab_width: u16,
    hinting: cosmic_text::Hinting,
    hint_factor: f32,
    scroll: cosmic_text::Scroll,
    maximum_height: f32,
    cursor: cosmic_text::Cursor,
    selection: cosmic_text::Selection,
    scroll_corrections: Vec<(usize, f64)>,
    scroll_width: f32,
    viewport_width: f32,
}

/// Independently mutable shaping state for one immutable editor snapshot.
#[derive(Debug)]
pub struct Replay {
    identity: Arc<()>,
    revision: Arc<()>,
    editor: cosmic_text::Editor<'static>,
    first_line: usize,
    total_lines: usize,
    maximum_height: f32,
    hint_factor: f32,
    height: f32,
    cursor_captured: bool,
    scroll_index: ScrollIndex,
    width: f32,
}

impl Editor {
    /// Logical lines whose attributes must be materialized before responsive capture.
    pub fn snapshot_range(&self, maximum_height: f32) -> Result<Range<usize>, Error> {
        let internal = self.internal();
        let buffer = buffer_from_editor(&internal.editor);
        let metrics = buffer.metrics();
        let scroll = buffer.scroll();
        let factor = internal.hint_factor;
        if !maximum_height.is_finite()
            || maximum_height <= 0.0
            || !factor.is_finite()
            || factor <= 0.0
            || !metrics.line_height.is_finite()
            || metrics.line_height <= 0.0
            || !metrics.font_size.is_finite()
            || metrics.font_size <= 0.0
            || !scroll.vertical.is_finite()
            || !scroll.horizontal.is_finite()
            || scroll.line >= buffer.lines.len()
        {
            return Err(Error::Viewport);
        }
        let rows = ((maximum_height as f64 * factor as f64 + scroll.vertical.abs() as f64)
            / metrics.line_height as f64)
            .ceil()
            + 2.0;
        if rows > (MAX_LINES / 2) as f64 {
            return Err(Error::Budget);
        }
        let rows = rows as usize;
        let first_line = scroll.line.saturating_sub(rows);
        let last_line = scroll.line.saturating_add(rows).min(buffer.lines.len());
        Ok(first_line..last_line)
    }

    /// Captures at most 4096 logical lines, 1 MiB of text/attribute payload and 16384 spans.
    ///
    /// `maximum_height` is the largest supported logical viewport height. The envelope
    /// includes preceding lines needed for bottom-of-document scroll normalization.
    /// Per-span metrics and font features refuse, preserving the minimum-row-height bound.
    /// This copies existing attributes; callers must first materialize highlighting
    /// for the envelope. It does not run a highlighter or retain mutable editor state.
    pub fn snapshot(&self, maximum_height: f32) -> Result<Arc<Snapshot>, Error> {
        let range = self.snapshot_range(maximum_height)?;
        let internal = self.internal();
        let buffer = buffer_from_editor(&internal.editor);
        let metrics = buffer.metrics();
        let scroll = buffer.scroll();
        let factor = internal.hint_factor;
        let first_line = range.start;
        let last_line = range.end;
        let mut bytes = 0usize;
        let mut text_bytes = 0usize;
        let mut spans = 0usize;
        for line in &buffer.lines[first_line..last_line] {
            text_bytes = text_bytes
                .checked_add(line.text().len())
                .ok_or(Error::Budget)?;
            bytes = bytes.checked_add(line.text().len()).ok_or(Error::Budget)?;
            if bytes > MAX_BYTES {
                return Err(Error::Budget);
            }
            let defaults = line.attrs_list().defaults();
            attribute_budget(&defaults, &mut bytes)?;
            for (_, attributes) in line.attrs_list().spans_iter() {
                spans += 1;
                if spans > MAX_SPANS {
                    return Err(Error::Budget);
                }
                attribute_budget(&attributes.as_attrs(), &mut bytes)?;
            }
        }
        if internal
            .scroll_index
            .as_ref()
            .is_some_and(|index| index.nodes.len() > MAX_LINES)
        {
            return Err(Error::Budget);
        }
        let scroll_corrections = internal
            .scroll_index
            .as_ref()
            .map_or_else(Vec::new, |index| {
                index
                    .nodes
                    .iter()
                    .map(|node| (node.line, node.delta))
                    .collect()
            });
        let scroll_width = internal
            .scroll_index
            .as_ref()
            .map_or(0.0, |index| index.width);
        let lines = buffer.lines[first_line..last_line]
            .iter()
            .map(|line| {
                let mut copied = cosmic_text::BufferLine::new(
                    line.text().to_owned(),
                    line.ending(),
                    line.attrs_list().clone(),
                    cosmic_text::Shaping::Advanced,
                );
                let _ = copied.set_align(line.align());
                copied
            })
            .collect();
        let source_metrics = internal.resolved_scroll_metrics;
        Ok(Arc::new(Snapshot {
            identity: internal.identity.clone(),
            revision: internal.revision.clone(),
            version: internal.version,
            source_metrics,
            lines,
            first_line,
            total_lines: buffer.lines.len(),
            text_bytes,
            metrics,
            wrap: buffer.wrap(),
            ellipsize: buffer.ellipsize(),
            monospace_width: buffer.monospace_width(),
            tab_width: buffer.tab_width(),
            hinting: buffer.hinting(),
            hint_factor: factor,
            scroll,
            maximum_height,
            cursor: internal.editor.cursor(),
            selection: internal.editor.selection(),
            scroll_corrections,
            scroll_width,
            viewport_width: internal.bounds.width,
        }))
    }
}

pub(super) fn attribute_budget(
    attributes: &cosmic_text::Attrs<'_>,
    bytes: &mut usize,
) -> Result<(), Error> {
    if attributes.metrics_opt.is_some() || !attributes.font_features.features.is_empty() {
        return Err(Error::UnsupportedBackend);
    }
    let family = match attributes.family {
        cosmic_text::Family::Name(name) => name.len(),
        _ => 0,
    };
    *bytes = bytes
        .checked_add(family)
        .and_then(|bytes| bytes.checked_add(std::mem::size_of::<cosmic_text::AttrsOwned>()))
        .ok_or(Error::Budget)?;
    if *bytes > MAX_BYTES {
        return Err(Error::Budget);
    }
    Ok(())
}

impl Snapshot {
    /// Checks an external chrome binding against the model and metrics captured with these text bytes.
    pub fn matches_scroll_source(&self, identity: &Arc<()>, metrics: ScrollMetrics) -> bool {
        Arc::ptr_eq(&self.identity, identity) && self.source_metrics == Some(metrics)
    }
    /// Line count of the native editor model, distinct from the bounded captured envelope.
    pub fn source_line_count(&self) -> usize {
        self.total_lines
    }
    /// Bounded source-line range; text before its start is not copied.
    pub fn lines(&self) -> Range<usize> {
        self.first_line..self.first_line + self.lines.len()
    }

    /// Metrics hint applied to the captured editor, if enabled by its source policy.
    pub fn hint_factor(&self) -> Option<f32> {
        matches!(self.hinting, cosmic_text::Hinting::Enabled).then_some(self.hint_factor)
    }

    /// UTF-8 text bytes copied, excluding separately budgeted attributes.
    pub fn text_bytes(&self) -> usize {
        self.text_bytes
    }

    /// Adds the entire captured source, including lines not visible at the initial width.
    pub fn include_fonts<'a>(
        &'a self,
        envelope: &mut crate::text::font_envelope::FontEnvelope<'a>,
    ) -> Result<(), Error> {
        envelope.add_lines(&self.lines, self.tab_width)
    }

    /// Constructs shaping state using caller-owned fonts, without consulting the UI font system.
    pub fn replay(
        &self,
        fonts: &mut cosmic_text::FontSystem,
        bounds: Size,
    ) -> Result<Replay, Error> {
        let mut buffer = cosmic_text::Buffer::new_empty(self.metrics);
        buffer.lines = self.lines.clone();
        buffer.set_wrap(self.wrap);
        buffer.set_ellipsize(self.ellipsize);
        buffer.set_monospace_width(self.monospace_width);
        buffer.set_tab_width(self.tab_width);
        buffer.set_hinting(self.hinting);
        buffer.set_scroll(cosmic_text::Scroll {
            line: self.scroll.line - self.first_line,
            ..self.scroll
        });
        let project = |mut cursor: cosmic_text::Cursor| {
            if cursor.line < self.first_line {
                cursor.line = 0;
                cursor.index = 0;
            } else if cursor.line >= self.first_line + self.lines.len() {
                cursor.line = self.lines.len() - 1;
                cursor.index = self.lines[cursor.line].text().len();
            } else {
                cursor.line -= self.first_line;
            }
            cursor
        };
        let mut editor = cosmic_text::Editor::new(buffer);
        editor.set_cursor(project(self.cursor));
        editor.set_selection(match self.selection {
            cosmic_text::Selection::None => cosmic_text::Selection::None,
            cosmic_text::Selection::Normal(cursor) => {
                cosmic_text::Selection::Normal(project(cursor))
            }
            cosmic_text::Selection::Word(cursor) => cosmic_text::Selection::Word(project(cursor)),
            cosmic_text::Selection::Line(cursor) => cosmic_text::Selection::Line(project(cursor)),
        });
        let mut scroll_index = ScrollIndex::new(self.total_lines, self.metrics.line_height);
        for &(line, delta) in &self.scroll_corrections {
            let mut changed = false;
            scroll_index.root =
                Some(scroll_index.insert(scroll_index.root, line, delta, &mut changed));
        }
        scroll_index.width = self.scroll_width;
        let mut replay = Replay {
            identity: self.identity.clone(),
            revision: self.revision.clone(),
            editor,
            first_line: self.first_line,
            total_lines: self.total_lines,
            maximum_height: self.maximum_height,
            hint_factor: self.hint_factor,
            height: 0.0,
            cursor_captured: self.lines().contains(&self.cursor.line),
            scroll_index,
            width: self.viewport_width,
        };
        replay.reflow(fonts, bounds)?;
        Ok(replay)
    }
}

impl Replay {
    /// Reflows the captured envelope; heights outside its capture budget refuse before mutation.
    pub fn reflow(
        &mut self,
        fonts: &mut cosmic_text::FontSystem,
        bounds: Size,
    ) -> Result<(), Error> {
        if !bounds.width.is_finite()
            || bounds.width < 0.0
            || !bounds.height.is_finite()
            || bounds.height < 0.0
            || bounds.height > self.maximum_height
        {
            return Err(Error::Viewport);
        }
        let scaled = bounds * self.hint_factor;
        if !scaled.width.is_finite() || !scaled.height.is_finite() {
            return Err(Error::Viewport);
        }
        if self.width != bounds.width {
            self.scroll_index =
                ScrollIndex::new(self.total_lines, self.buffer().metrics().line_height);
        }
        let buffer = match self.editor.buffer_ref_mut() {
            cosmic_text::BufferRef::Owned(buffer) => buffer,
            cosmic_text::BufferRef::Arc(buffer) => Arc::make_mut(buffer),
            cosmic_text::BufferRef::Borrowed(_) => return Err(Error::UnsupportedBackend),
        };
        buffer.set_size(Some(scaled.width), Some(scaled.height));
        buffer.shape_until_scroll(fonts, false);
        self.height = scaled.height;
        self.width = bounds.width;
        Ok(())
    }

    /// Returns native sparse-index scroll geometry, refining only the captured visible rows.
    ///
    /// # Errors
    /// Refuses excess retained corrections instead of growing an unbounded scroll history.
    pub fn scroll_metrics(&mut self) -> Result<ScrollMetrics, Error> {
        let buffer = buffer_from_editor(&self.editor);
        if self
            .scroll_index
            .nodes
            .len()
            .saturating_add(buffer.lines.len())
            > MAX_LINES
        {
            return Err(Error::Budget);
        }
        refine_scroll_index_at(&mut self.scroll_index, buffer, self.height, self.first_line);
        if self.scroll_index.nodes.len() > MAX_LINES {
            return Err(Error::Budget);
        }
        let scroll = buffer.scroll();
        let first_line = self.first_line + scroll.line;
        let top = self.scroll_index.offset_of(first_line);
        Ok(ScrollMetrics {
            viewport: Size::new(self.width, self.height / self.hint_factor),
            content: Size::new(self.scroll_index.width, self.scroll_index.content_height())
                * (1.0 / self.hint_factor),
            offset: Vector::new(
                scroll.horizontal / self.hint_factor,
                (top + scroll.vertical) / self.hint_factor,
            ),
            first_line,
            first_line_top: top / self.hint_factor,
        })
    }

    /// Shares the current glyph buffer; later reflow detaches only while a consumer still holds it.
    pub fn shared_buffer(&mut self) -> Arc<cosmic_text::Buffer> {
        if let cosmic_text::BufferRef::Arc(buffer) = self.editor.buffer_ref() {
            return buffer.clone();
        }
        let empty = cosmic_text::Buffer::new_empty(self.buffer().metrics());
        let old = std::mem::replace(
            self.editor.buffer_ref_mut(),
            cosmic_text::BufferRef::Owned(empty),
        );
        let cosmic_text::BufferRef::Owned(buffer) = old else {
            unreachable!()
        };
        let buffer = Arc::new(buffer);
        *self.editor.buffer_ref_mut() = cosmic_text::BufferRef::Arc(buffer.clone());
        buffer
    }

    /// Highlight geometry in logical viewport coordinates, refusing more than 16384 rectangles.
    pub fn highlight_ranges(
        &self,
        ranges: &[(editor::Position, editor::Position)],
    ) -> Result<Vec<Vec<Rectangle>>, Error> {
        if ranges.len() > MAX_SPANS {
            return Err(Error::Budget);
        }
        let buffer = self.buffer();
        let end = self.first_line + buffer.lines.len();
        let mut rectangles = 0usize;
        ranges
            .iter()
            .map(|&(start, finish)| {
                if finish.line < self.first_line || start.line >= end || start.line > finish.line {
                    return Ok(Vec::new());
                }
                let project = |position: editor::Position| {
                    if position.line < self.first_line {
                        cosmic_text::Cursor::new(0, 0)
                    } else if position.line >= end {
                        cosmic_text::Cursor::new(
                            buffer.lines.len() - 1,
                            buffer.lines.last().unwrap().text().len(),
                        )
                    } else {
                        cosmic_text::Cursor::new(position.line - self.first_line, position.index)
                    }
                };
                let regions = selection_regions(
                    buffer,
                    project(start),
                    project(finish),
                    self.height,
                    self.hint_factor,
                );
                rectangles = rectangles.checked_add(regions.len()).ok_or(Error::Budget)?;
                if rectangles > MAX_SPANS {
                    return Err(Error::Budget);
                }
                Ok(regions)
            })
            .collect()
    }

    /// Visible logical-line heads in the source document's index space.
    pub fn line_number_rows(&self) -> Result<Vec<crate::core::text::editor::gutter::Row>, Error> {
        super::line_number_rows(
            self.buffer(),
            self.height,
            self.hint_factor,
            self.first_line,
        )
    }

    /// Current owned glyph layout, with line indices relative to [`Self::first_line`].
    pub fn buffer(&self) -> &cosmic_text::Buffer {
        buffer_from_editor(&self.editor)
    }

    /// Converts local layout line indices to the source document's line space.
    pub fn first_line(&self) -> usize {
        self.first_line
    }

    /// Source document line count, independent of the bounded copied envelope.
    pub fn total_lines(&self) -> usize {
        self.total_lines
    }

    /// Visible caret in logical viewport coordinates, or `None` for selection and offscreen cursors.
    pub fn caret(&self) -> Option<Point> {
        if !self.cursor_captured || self.editor.selection_bounds().is_some() {
            return None;
        }
        caret_point(self.buffer(), self.editor.cursor(), self.hint_factor)
    }

    /// Visible underline and strike-through geometry in logical pixels.
    ///
    /// Returns `Budget` before allocation if the result exceeds `maximum` or 16384 records.
    pub fn decorations(&self, maximum: usize) -> Result<Vec<editor::Decoration>, Error> {
        let maximum = maximum.min(MAX_SPANS);
        let mut count = 0usize;
        visit_decorations(self.buffer(), self.hint_factor, |_| {
            if count == maximum {
                return Err(Error::Budget);
            }
            count += 1;
            Ok(())
        })?;
        let mut decorations = Vec::new();
        decorations
            .try_reserve_exact(count)
            .map_err(|_| Error::Budget)?;
        visit_decorations(self.buffer(), self.hint_factor, |decoration| {
            decorations.push(decoration);
            Ok::<(), std::convert::Infallible>(())
        })
        .unwrap_or_else(|never| match never {});
        Ok(decorations)
    }

    /// Visible selection geometry from the same owned buffer used to draw the glyphs.
    pub fn selection_regions(&self) -> Vec<Rectangle> {
        self.editor
            .selection_bounds()
            .map_or_else(Vec::new, |(start, end)| {
                selection_regions(self.buffer(), start, end, self.height, self.hint_factor)
            })
    }
}

// Modified by enzUI contributors. See FORK.md.
use super::*;

const MAX_GLYPHS: usize = 16_384;

/// Bounded displayed editor geometry bound to an unchanged native model revision.
/// This is not presentation authority; the window must independently validate its frame lease.
#[derive(Debug)]
pub struct Geometry {
    source: Arc<Snapshot>,
    bounds: Size,
    scroll: cosmic_text::Scroll,
    index: ScrollIndex,
    metrics: ScrollMetrics,
    rows: Vec<Row>,
}

#[derive(Debug, PartialEq)]
struct Row {
    line: usize,
    rtl: bool,
    top: f32,
    baseline: f32,
    height: f32,
    width: f32,
    glyphs: Vec<Glyph>,
}

#[derive(Debug, PartialEq)]
struct Glyph {
    start: usize,
    end: usize,
    glyph: u16,
    level: u8,
    weight: u16,
    metrics: [f32; 6],
    metadata: usize,
}

fn rows(buffer: &cosmic_text::Buffer, origin: usize) -> Result<Vec<Row>, Error> {
    let mut rows = Vec::new();
    let mut glyphs = 0usize;
    for run in buffer.layout_runs() {
        glyphs = glyphs.checked_add(run.glyphs.len()).ok_or(Error::Budget)?;
        if glyphs > MAX_GLYPHS || rows.len() == MAX_LINES {
            return Err(Error::Budget);
        }
        let mut copied = Vec::new();
        copied
            .try_reserve_exact(run.glyphs.len())
            .map_err(|_| Error::Budget)?;
        for glyph in run.glyphs {
            let metrics = [
                glyph.x,
                glyph.y,
                glyph.w,
                glyph.font_size,
                glyph.x_offset,
                glyph.y_offset,
            ];
            if !metrics.into_iter().all(f32::is_finite) {
                return Err(Error::NonFinite);
            }
            copied.push(Glyph {
                start: glyph.start,
                end: glyph.end,
                glyph: glyph.glyph_id,
                level: glyph.level.number(),
                weight: glyph.font_weight.0,
                metrics,
                metadata: glyph.metadata,
            });
        }
        if ![run.line_top, run.line_y, run.line_height, run.line_w]
            .into_iter()
            .all(f32::is_finite)
        {
            return Err(Error::NonFinite);
        }
        rows.try_reserve(1).map_err(|_| Error::Budget)?;
        rows.push(Row {
            line: origin.checked_add(run.line_i).ok_or(Error::Budget)?,
            rtl: run.rtl,
            top: run.line_top,
            baseline: run.line_y,
            height: run.line_height,
            width: run.line_w,
            glyphs: copied,
        });
    }
    Ok(rows)
}

impl Replay {
    /// Copies bounded visible hit geometry and sparse scroll corrections without UI access.
    /// A different snapshot revision or more than 16384 visible glyphs is refused.
    pub fn input_geometry(&mut self, source: Arc<Snapshot>) -> Result<Geometry, Error> {
        if !Arc::ptr_eq(&self.identity, &source.identity)
            || !Arc::ptr_eq(&self.revision, &source.revision)
        {
            return Err(Error::RecordingState);
        }
        let metrics = self.scroll_metrics()?;
        let buffer = self.buffer();
        let scroll = cosmic_text::Scroll {
            line: self
                .first_line
                .checked_add(buffer.scroll().line)
                .ok_or(Error::Budget)?,
            ..buffer.scroll()
        };
        let rows = rows(buffer, self.first_line)?;
        if self.scroll_index.nodes.len() > MAX_LINES {
            return Err(Error::Budget);
        }
        Ok(Geometry {
            source,
            bounds: metrics.viewport,
            scroll,
            index: self.scroll_index.clone(),
            metrics,
            rows,
        })
    }
}

impl Geometry {
    /// Whether this geometry was produced from the exact retained source snapshot.
    pub fn matches_source(&self, source: &Arc<Snapshot>) -> bool {
        Arc::ptr_eq(&self.source, source)
    }

    /// Row, glyph and sparse-correction records charged by the backend's input-snapshot limit.
    /// This is not byte accounting for the shared source document, fonts or GPU resources.
    pub fn retained_items(&self) -> usize {
        self.rows.len()
            + self.index.nodes.len()
            + self.rows.iter().map(|row| row.glyphs.len()).sum::<usize>()
    }

    /// Native text viewport, excluding the widget's chrome and gutter padding.
    pub fn viewport(&self) -> Size {
        self.bounds
    }

    /// Displayed scroll metrics; not a new estimate from native widget state.
    pub fn scroll_metrics(&self) -> ScrollMetrics {
        self.metrics
    }
}

impl Editor {
    /// Reflows this editor alone to validated displayed geometry, preserving the document, undo history and caret/selection.
    ///
    /// Refuses changed model/font revisions, busy caches, shared readers or different hit geometry. On a post-reflow
    /// refusal, the caller must revoke input and restore native presentation before dispatching.
    /// No widget layout, highlighter, application callback or cursor-follow action is executed.
    pub fn reconcile_geometry(&mut self, geometry: &Geometry) -> Result<(), Error> {
        let source = &geometry.source;
        let internal = self.internal();
        if !Arc::ptr_eq(&internal.identity, &source.identity)
            || !Arc::ptr_eq(&internal.revision, &source.revision)
            || internal.version != source.version
            || buffer_from_editor(&internal.editor).lines.len() != source.total_lines
            || geometry.index.nodes.len() > MAX_LINES
        {
            return Err(Error::RecordingState);
        }
        let mut fonts = text::font_system()
            .try_write()
            .map_err(|_| Error::FontAssets)?;
        if fonts.version() != source.version {
            return Err(Error::FontAssets);
        }
        self.with_geometry_mut(|internal| {
            let buffer = buffer_mut_from_editor(&mut internal.editor);
            let scaled = geometry.bounds * internal.hint_factor;
            buffer.set_size(Some(scaled.width), Some(scaled.height));
            buffer.set_scroll(geometry.scroll);
            buffer.shape_until_scroll(fonts.raw(), false);
            internal.bounds = geometry.bounds;
            if buffer.scroll() != geometry.scroll || rows(buffer, 0)? != geometry.rows {
                return Err(Error::RecordingState);
            }
            internal.scroll_index = Some(geometry.index.clone());
            internal.resolved_scroll_metrics = Some(geometry.metrics);
            Ok(())
        })
        .map_err(|_| Error::RecordingState)?
    }
}

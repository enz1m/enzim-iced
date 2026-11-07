// Modified by enzUI contributors. See FORK.md.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

/// At most eight live allocation snapshots from one renderer, including external clones.
#[derive(Debug, Default)]
pub struct SampleBudget(Arc<AtomicUsize>);

/// Sampled layout, not by itself an input lease or compositor receipt.
///
/// Receipt and draw identity must be validated before projecting semantic geometry.
/// Hit testing and editor interaction additionally require native state reconciliation.
pub struct Sample {
    program: Arc<Program>,
    revision: Option<Arc<()>>,
    allocations: Box<[(u64, Bounds, Option<Bounds>)]>,
    native_local: Box<[Bounds]>,
    drivers: Box<[(u64, u8, f32)]>,
    viewport: [f32; 2],
    native_payload: Option<Arc<dyn std::any::Any + Send + Sync>>,
    _lease: Lease,
}
impl std::fmt::Debug for Sample {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LayoutSample")
            .field("allocations", &self.allocations.len())
            .field("drivers", &self.drivers.len())
            .field("viewport", &self.viewport)
            .finish_non_exhaustive()
    }
}
#[derive(Debug)]
struct Lease(Arc<AtomicUsize>);
impl Drop for Lease {
    fn drop(&mut self) {
        let _ = self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
impl SampleBudget {
    /// Copies one successful evaluation without borrowing widget state or text/font locks.
    ///
    /// Refuses when no evaluation is valid or eight snapshots are still owned.
    pub fn capture(&self, state: &State) -> Result<Arc<Sample>, Error> {
        if !state.initialized {
            return Err(Error::RecordingState);
        }
        let program = state.program.as_ref().ok_or(Error::RecordingState)?;
        let _ = self
            .0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                (used < 8).then_some(used + 1)
            })
            .map_err(|_| Error::Budget)?;
        let lease = Lease(self.0.clone());
        let mut allocations = Vec::new();
        allocations
            .try_reserve_exact(program.nodes.len())
            .map_err(|_| Error::Budget)?;
        for (index, node) in program.nodes.iter().enumerate() {
            allocations.push((node.id, state.absolute[index], state.clips[index]));
        }
        allocations.sort_unstable_by_key(|entry| entry.0);
        let mut native_local = Vec::new();
        native_local
            .try_reserve_exact(program.nodes.len())
            .map_err(|_| Error::Budget)?;
        native_local.extend_from_slice(&state.local);
        let mut drivers = Vec::new();
        drivers
            .try_reserve_exact(program.expressions.len())
            .map_err(|_| Error::Budget)?;
        for (index, expression) in program.expressions.iter().enumerate() {
            if let Expression::Driver { node, channel } = *expression {
                drivers.push((node, channel, state.values[index]));
            }
        }
        drivers.sort_unstable_by_key(|entry| (entry.0, entry.1));
        drivers.dedup_by_key(|entry| (entry.0, entry.1));
        Ok(Arc::new(Sample {
            program: program.clone(),
            revision: state.text_revision.clone(),
            allocations: allocations.into_boxed_slice(),
            native_local: native_local.into_boxed_slice(),
            drivers: drivers.into_boxed_slice(),
            viewport: state.viewport,
            native_payload: None,
            _lease: lease,
        }))
    }
    /// Number of snapshots still retained by any consumer, not the number of Arc handles.
    pub fn live(&self) -> usize {
        self.0.load(Ordering::Acquire)
    }
}
impl Sample {
    /// Attaches bounded native metadata before publication, requiring exclusive sample ownership.
    /// The sample slot must be reserved before constructing this payload; the backend must validate
    /// its type, source and resource budget before native reconciliation.
    pub fn set_native_payload(&mut self, payload: Arc<dyn std::any::Any + Send + Sync>) {
        self.native_payload = Some(payload);
    }

    /// Borrows typed backend data without invoking a callback or consulting mutable widget state.
    pub fn native_payload<T: std::any::Any + Send + Sync>(&self) -> Option<&T> {
        self.native_payload.as_ref()?.downcast_ref()
    }

    /// Whether bounded backend metadata was attached before sample publication.
    pub fn has_native_payload(&self) -> bool {
        self.native_payload.is_some()
    }

    /// Whether allocation projection can describe geometry without unrelated paint properties.
    /// This does not reconcile native interaction state or authorize input dispatch.
    pub fn geometry_reconcilable(&self) -> bool {
        self.program.bindings.is_empty()
    }

    /// Root allocation identity of this exact captured domain.
    pub fn root_node(&self) -> Option<u64> {
        self.program.nodes.last().map(|node| node.id)
    }

    /// Exact editor allocation identities requiring a successful native reconciliation report.
    pub fn editor_nodes(&self) -> impl Iterator<Item = u64> + '_ {
        self.program.editor_nodes().map(|(node, _)| node)
    }

    /// Exact scroll allocation identities requiring native offset/configuration validation.
    pub fn scroll_nodes(&self) -> impl Iterator<Item = u64> + '_ {
        self.program.scroll_nodes().map(|(node, _)| node)
    }

    /// Whether allocation projection alone covers this program's interaction geometry.
    ///
    /// Editors and scroll owners require native state reconciliation. Unrelated
    /// paint-property bindings have no supported reconciliation path.
    pub fn layout_input_only(&self) -> bool {
        self.program.bindings.is_empty()
            && self.program.editor_nodes().next().is_none()
            && self.program.scroll_nodes().next().is_none()
    }

    /// Checks draw/program identity against a retained source witness, not its sampled values.
    pub fn same_source_as(&self, other: &Self) -> bool {
        self.same_source(&other.program, other.revision.as_ref())
    }

    /// Sorted unique layout-driver identities; unrelated paint tracks are not included.
    pub fn driver_nodes(&self) -> impl Iterator<Item = u64> + '_ {
        self.drivers
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                (index == 0 || self.drivers[index - 1].0 != entry.0).then_some(entry.0)
            })
    }

    /// Current logical viewport dimensions used by the sampled layout.
    pub fn viewport(&self) -> [f32; 2] {
        self.viewport
    }
    /// Evaluated allocation and ancestor scroll clip, before unrelated paint transforms.
    pub fn allocation(&self, node: u64) -> Option<(Bounds, Option<Bounds>)> {
        let index = self
            .allocations
            .binary_search_by_key(&node, |entry| entry.0)
            .ok()?;
        Some((self.allocations[index].1, self.allocations[index].2))
    }
    /// Actual evaluated driver value, not a new sample of an animation clock.
    pub fn driver(&self, node: u64, channel: u8) -> Option<f32> {
        self.drivers
            .binary_search_by_key(&(node, channel), |entry| (entry.0, entry.1))
            .ok()
            .map(|i| self.drivers[i].2)
    }
    /// Projects sampled allocation onto the exact native layout hierarchy without running widgets.
    ///
    /// Scroll offsets remain with native scroll owners; applying painted offsets here would
    /// subtract them twice. This does not refresh editor shaping or apply unrelated paint
    /// transforms. Callers must validate presentation ownership and the draw revision before
    /// using the result for input or accessibility.
    pub fn project_layout(
        &self,
        source: &crate::layout::Node,
    ) -> Result<crate::layout::Node, Error> {
        fn visit(
            sample: &Sample,
            source: &crate::layout::Node,
            index: usize,
            depth: usize,
            seen: &mut usize,
        ) -> Result<crate::layout::Node, Error> {
            if depth >= MAX_DEPTH || *seen >= MAX_ITEMS {
                return Err(Error::Budget);
            }
            // An undeclared wrapper was lowered as its child; it takes the
            // child's projected allocation and keeps covering it.
            if let Some(child) = source.transparent_child() {
                let child = visit(sample, child, index, depth + 1, seen)?;
                let bounds = child.bounds();
                return Ok(source.projected(bounds, vec![child.move_to(crate::Point::ORIGIN)]));
            }
            *seen += 1;
            let expected = &sample.program.nodes[index];
            if source.presentation_node().map(|id| id.0) != Some(expected.id)
                || source.children().len() != expected.children.len()
            {
                return Err(Error::SceneIdentity(expected.id));
            }
            let bounds = sample.native_local[index];
            let mut children = Vec::new();
            children
                .try_reserve_exact(expected.children.len())
                .map_err(|_| Error::Budget)?;
            for (child, index) in source.children().iter().zip(&expected.children) {
                children.push(visit(sample, child, *index, depth + 1, seen)?);
            }
            let mut projected = source.projected(
                crate::Rectangle {
                    x: bounds.x,
                    y: bounds.y,
                    width: bounds.width,
                    height: bounds.height,
                },
                children,
            );
            if let Layout::Clip(declaration) = &expected.layout
                && let Some(reveal) = declaration.reveal
            {
                projected.presented_reveal_progress = Some(match reveal.driver {
                    Some(driver) => sample
                        .driver(driver, 0)
                        .ok_or(Error::MissingDriver(driver))?,
                    None => reveal.captured,
                });
            }
            Ok(projected)
        }
        let mut seen = 0;
        let node = visit(self, source, self.program.nodes.len() - 1, 0, &mut seen)?;
        if seen != self.program.nodes.len() {
            return Err(Error::RecordingState);
        }
        Ok(node)
    }

    /// Checks the exact source program and draw revision, including redraw-only replacements.
    pub fn same_source(&self, program: &Arc<Program>, revision: Option<&Arc<()>>) -> bool {
        Arc::ptr_eq(&self.program, program)
            && match (self.revision.as_ref(), revision) {
                (None, None) => true,
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                _ => false,
            }
    }
}

#[cfg(test)]
mod geometry_tests {
    use super::*;

    /// A projection keeps the undeclared wrapper that lowering looked
    /// through, and lays it over its child's projected allocation.
    ///
    /// Sabotage: drop the `transparent_child` branch in `project_layout` and
    /// the wrapper is refused as a foreign scene.
    #[test]
    fn projection_keeps_an_undeclared_wrapper_over_its_child() {
        use crate::layout::{self, Limits, Node, Placed};
        use crate::{Length, Point, Rectangle, Size};

        let child = layout::empty(&Limits::NONE, Length::Fixed(10.0), Length::Fixed(10.0));
        let wrapper =
            Node::with_children(Size::new(10.0, 10.0), vec![child]).move_to(Point::new(4.0, 6.0));
        let program = Placed::of(&wrapper)
            .presentation_program(
                crate::renderer::NodeId(900),
                0,
                Rectangle::with_size(Size::new(100.0, 100.0)),
            )
            .unwrap();
        let mut state = State::default();
        let _ = state
            .sample(
                &program,
                [100.0, 100.0],
                &[Input {
                    node: 900,
                    values: [100.0; 8],
                    available: 1,
                }],
            )
            .unwrap();
        let sample = SampleBudget::default().capture(&state).unwrap();

        let projected = sample.project_layout(&wrapper).unwrap();

        assert_eq!(projected.children().len(), 1);
        assert_eq!(projected.children()[0].position(), Point::ORIGIN);
        assert_eq!(projected.size(), projected.children()[0].size());
    }

    #[test]
    fn moving_geometry_properties_require_more_than_allocation_projection() {
        for property in [false, true] {
            let bounds = Bounds {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            };
            let program = Arc::new(
                Program::new(
                    vec![
                        Expression::Constant(0.0),
                        Expression::Driver {
                            node: 90,
                            channel: 0,
                        },
                    ],
                    vec![Node {
                        id: 1,
                        layout: Layout::Fixed { recorded: bounds },
                        children: vec![],
                    }],
                    if property {
                        vec![Binding {
                            node: 0,
                            target: Target::Opacity,
                            expression: 1,
                        }]
                    } else {
                        vec![]
                    },
                    [0, 0],
                    bounds,
                )
                .unwrap(),
            );
            let mut state = State::default();
            let _ = state
                .sample(
                    &program,
                    [100.0, 100.0],
                    &[Input {
                        node: 90,
                        values: [0.5; 8],
                        available: 1,
                    }],
                )
                .unwrap();
            let sample = SampleBudget::default().capture(&state).unwrap();
            assert_eq!(sample.geometry_reconcilable(), !property);
        }
    }
}

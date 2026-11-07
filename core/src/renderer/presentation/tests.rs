// Modified by enzUI contributors. See FORK.md.
use super::*;

fn rect(x: f32, y: f32, width: f32, height: f32) -> Bounds {
    Bounds {
        x,
        y,
        width,
        height,
    }
}

fn input(node: u64, value: f32) -> Input {
    Input {
        node,
        values: [value, 0.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0],
        available: 0x0f,
    }
}

fn flow() -> Arc<Program> {
    Arc::new(
        Program::new(
            vec![
                Expression::Constant(0.0),
                Expression::ViewportWidth,
                Expression::Driver {
                    node: 900,
                    channel: 0,
                },
                Expression::Driver {
                    node: 901,
                    channel: 0,
                },
                Expression::Subtract(1, 2),
                Expression::Constant(7.0),
            ],
            vec![
                Node {
                    id: 1,
                    layout: Layout::Fixed {
                        recorded: rect(0.0, 0.0, 80.0, 20.0),
                    },
                    children: vec![],
                },
                Node {
                    id: 2,
                    layout: Layout::Fixed {
                        recorded: rect(90.0, 0.0, 80.0, 30.0),
                    },
                    children: vec![],
                },
                Node {
                    id: 3,
                    layout: Layout::Fixed {
                        recorded: rect(180.0, 0.0, 80.0, 10.0),
                    },
                    children: vec![],
                },
                Node {
                    id: 4,
                    layout: Layout::Flow {
                        width: Some(4),
                        height: None,
                        column_gap: 3,
                        row_gap: 5,
                    },
                    children: vec![0, 1, 2],
                },
            ],
            vec![],
            [2, 0],
            rect(0.0, 0.0, 400.0, 240.0),
        )
        .unwrap(),
    )
}

#[test]
fn responsive_width_reflows_fixed_paint_without_scaling_it() {
    let program = flow();
    let mut state = State::default();
    let first = state
        .sample(
            &program,
            [400.0, 240.0],
            &[input(900, 0.0), input(901, 10.0)],
        )
        .unwrap();
    assert_eq!(first[2].bounds, rect(180.0, 0.0, 80.0, 10.0));
    let narrow = state
        .sample(
            &program,
            [400.0, 240.0],
            &[input(900, 200.0), input(901, 10.0)],
        )
        .unwrap();
    assert_eq!(narrow[0].bounds, rect(200.0, 0.0, 80.0, 20.0));
    assert_eq!(narrow[1].bounds, rect(290.0, 0.0, 80.0, 30.0));
    assert_eq!(narrow[2].bounds, rect(200.0, 37.0, 80.0, 10.0));
    assert_eq!(narrow[2].translation, [20.0, 37.0]);
    let mut positions = BTreeSet::new();
    for width in 0..=100 {
        let frame = state
            .sample(
                &program,
                [400.0, 240.0],
                &[input(900, width as f32), input(901, 10.0)],
            )
            .unwrap();
        let _ = positions.insert(frame[0].bounds.x.to_bits());
        assert_eq!(frame[0].bounds.width, 80.0);
        assert_eq!(frame[1].bounds.height, 30.0);
    }
    assert_eq!(positions.len(), 101);
}

#[test]
fn custom_gap_and_width_drivers_compose_in_one_owned_program() {
    let program = flow();
    let mut state = State::default();
    let tight = state
        .sample(
            &program,
            [400.0, 240.0],
            &[input(900, 120.0), input(901, 0.0)],
        )
        .unwrap();
    assert_eq!(tight[2].bounds, rect(280.0, 0.0, 80.0, 10.0));
    let spaced = state
        .sample(
            &program,
            [400.0, 240.0],
            &[input(900, 120.0), input(901, 25.0)],
        )
        .unwrap();
    assert_eq!(spaced[1].bounds.x, 225.0);
    assert_eq!(spaced[2].bounds, rect(120.0, 37.0, 80.0, 10.0));
}

#[test]
fn unchanged_samples_reuse_layout_and_storage() {
    let program = flow();
    let mut state = State::default();
    let inputs = [input(900, 100.0), input(901, 5.0)];
    let pointer = state
        .sample(&program, [400.0, 240.0], &inputs)
        .unwrap()
        .as_ptr();
    assert_eq!(state.work().layout_nodes, 4);
    let capacity = state.placements.capacity();
    for _ in 0..100 {
        let next = state
            .sample(&program, [400.0, 240.0], &inputs)
            .unwrap()
            .as_ptr();
        assert_eq!(pointer, next);
        assert_eq!(state.work().layout_nodes, 0);
        assert_eq!(state.placements.capacity(), capacity);
    }
}

#[test]
fn paint_property_changes_do_not_trigger_layout() {
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
                layout: Layout::Fixed {
                    recorded: rect(0.0, 0.0, 50.0, 20.0),
                },
                children: vec![],
            }],
            vec![Binding {
                node: 0,
                target: Target::Opacity,
                expression: 1,
            }],
            [0, 0],
            rect(0.0, 0.0, 100.0, 100.0),
        )
        .unwrap(),
    );
    let mut state = State::default();
    let _ = state
        .sample(&program, [100.0, 100.0], &[input(90, 1.0)])
        .unwrap();
    let frame = state
        .sample(&program, [100.0, 100.0], &[input(90, 0.25)])
        .unwrap();
    assert_eq!(frame[0].opacity, 0.25);
    assert_eq!(state.work().layout_nodes, 0);
}

#[test]
fn replacement_releases_the_previous_program_and_recomputes_one_coherent_revision() {
    let mut state = State::default();
    let old = flow();
    let weak = Arc::downgrade(&old);
    let _ = state
        .sample(&old, [400.0, 240.0], &[input(900, 100.0), input(901, 10.0)])
        .unwrap();
    drop(old);
    assert!(weak.upgrade().is_some());
    let next = Arc::new(
        Program::new(
            vec![Expression::Constant(0.0)],
            vec![Node {
                id: 7,
                layout: Layout::Fixed {
                    recorded: rect(30.0, 20.0, 120.0, 15.0),
                },
                children: vec![],
            }],
            vec![],
            [0, 0],
            rect(0.0, 0.0, 400.0, 240.0),
        )
        .unwrap(),
    );
    let frame = state.sample(&next, [400.0, 240.0], &[]).unwrap();
    assert_eq!(frame.len(), 1);
    assert_eq!(frame[0].node, 7);
    assert_eq!(frame[0].translation, [-30.0, -20.0]);
    assert!(weak.upgrade().is_none());
    assert_eq!(state.work().layout_nodes, 1);
    state.clear();
    assert!(state.program.is_none());
    assert_eq!(state.local.capacity(), 0);
}

#[test]
fn fixed_leaf_width_is_a_capability_boundary_not_a_scaled_fallback() {
    let program = flow();
    let mut state = State::default();
    assert_eq!(
        state.sample(
            &program,
            [400.0, 240.0],
            &[input(900, 350.0), input(901, 0.0)]
        ),
        Err(Error::ReflowRequired(1))
    );
    let recovered = state
        .sample(
            &program,
            [400.0, 240.0],
            &[input(900, 0.0), input(901, 10.0)],
        )
        .unwrap();
    assert_eq!(recovered[2].bounds.width, 80.0);
    assert_eq!(recovered[2].bounds.x, 180.0);
}

#[test]
fn missing_nonfinite_reordered_and_unavailable_drivers_refuse() {
    let program = flow();
    let mut state = State::default();
    assert_eq!(
        state.sample(&program, [400.0, 240.0], &[]),
        Err(Error::MissingDriver(900))
    );
    assert_eq!(
        state.sample(
            &program,
            [400.0, 240.0],
            &[input(900, f32::NAN), input(901, 10.0)]
        ),
        Err(Error::NonFinite)
    );
    assert_eq!(
        state.sample(
            &program,
            [400.0, 240.0],
            &[input(901, 0.0), input(900, 0.0)]
        ),
        Err(Error::InputOrder)
    );
    let mut unavailable = input(900, 0.0);
    unavailable.available = 0;
    assert_eq!(
        state.sample(&program, [400.0, 240.0], &[unavailable, input(901, 10.0)]),
        Err(Error::MissingDriver(900))
    );
    let _ = state
        .sample(
            &program,
            [400.0, 240.0],
            &[input(900, 0.0), input(901, 10.0)],
        )
        .unwrap();
    assert_eq!(state.work().layout_nodes, 4);
}

#[test]
fn admission_rejects_cycles_aliases_and_excessive_populations() {
    let leaf = Node {
        id: 1,
        layout: Layout::Fixed {
            recorded: rect(0.0, 0.0, 1.0, 1.0),
        },
        children: vec![],
    };
    let clip = rect(0.0, 0.0, 100.0, 100.0);
    assert!(matches!(
        Program::new(
            vec![Expression::Add(0, 0)],
            vec![leaf.clone()],
            vec![],
            [0, 0],
            clip
        ),
        Err(Error::Expression(0))
    ));
    assert!(matches!(
        Program::new(
            vec![Expression::Constant(0.0)],
            vec![leaf.clone(), leaf.clone()],
            vec![],
            [0, 0],
            clip
        ),
        Err(Error::DuplicateNode(1))
    ));
    let mut cycle = leaf.clone();
    cycle.children.push(0);
    assert!(matches!(
        Program::new(
            vec![Expression::Constant(0.0)],
            vec![cycle],
            vec![],
            [0, 0],
            clip
        ),
        Err(Error::Node(0))
    ));
    assert!(matches!(
        Program::new(
            vec![Expression::Constant(0.0); MAX_ITEMS + 1],
            vec![leaf],
            vec![],
            [0, 0],
            clip
        ),
        Err(Error::Budget)
    ));
}

#[test]
fn recorded_membership_and_clip_are_not_inferred_from_geometry_similarity() {
    let program = flow();
    program
        .validate_scene(&[1, 2, 3, 100], rect(0.0, 0.0, 400.0, 240.0))
        .unwrap();
    assert_eq!(
        program.validate_scene(&[1, 2], rect(0.0, 0.0, 400.0, 240.0)),
        Err(Error::SceneIdentity(3))
    );
    assert_eq!(
        program.validate_scene(&[1, 2, 3, 900], rect(0.0, 0.0, 400.0, 240.0)),
        Err(Error::SceneIdentity(900))
    );
    assert_eq!(
        program.validate_scene(&[1, 2, 3], rect(0.0, 0.0, 399.0, 240.0)),
        Err(Error::ClipCoverage(4))
    );
}

#[test]
fn nested_flow_measures_height_before_positioning_following_siblings() {
    let program = Arc::new(
        Program::new(
            vec![
                Expression::Constant(0.0),
                Expression::ViewportWidth,
                Expression::Constant(5.0),
            ],
            vec![
                Node {
                    id: 1,
                    layout: Layout::Fixed {
                        recorded: rect(0.0, 0.0, 60.0, 20.0),
                    },
                    children: vec![],
                },
                Node {
                    id: 2,
                    layout: Layout::Fixed {
                        recorded: rect(65.0, 0.0, 60.0, 30.0),
                    },
                    children: vec![],
                },
                Node {
                    id: 3,
                    layout: Layout::Flow {
                        width: Some(1),
                        height: None,
                        column_gap: 2,
                        row_gap: 2,
                    },
                    children: vec![0, 1],
                },
                Node {
                    id: 4,
                    layout: Layout::Fixed {
                        recorded: rect(0.0, 35.0, 60.0, 10.0),
                    },
                    children: vec![],
                },
                Node {
                    id: 5,
                    layout: Layout::Stack {
                        axis: Axis::Vertical,
                        width: Some(1),
                        height: None,
                        gap: 2,
                    },
                    children: vec![2, 3],
                },
            ],
            vec![],
            [0, 0],
            rect(0.0, 0.0, 200.0, 200.0),
        )
        .unwrap(),
    );
    let mut state = State::default();
    let wide = state.sample(&program, [200.0, 200.0], &[]).unwrap();
    assert_eq!(wide[2].bounds.y, 35.0);
    let narrow = state.sample(&program, [100.0, 200.0], &[]).unwrap();
    assert_eq!(narrow[1].bounds, rect(0.0, 25.0, 60.0, 30.0));
    assert_eq!(narrow[2].bounds.y, 60.0);
}

#[test]
fn shared_immutable_program_has_independent_window_storage() {
    let program = flow();
    let a = program.clone();
    let b = program.clone();
    let one = std::thread::spawn(move || {
        let mut state = State::default();
        state
            .sample(&a, [400.0, 240.0], &[input(900, 0.0), input(901, 10.0)])
            .unwrap()[2]
            .bounds
    });
    let two = std::thread::spawn(move || {
        let mut state = State::default();
        state
            .sample(&b, [400.0, 240.0], &[input(900, 200.0), input(901, 10.0)])
            .unwrap()[2]
            .bounds
    });
    assert_eq!(one.join().unwrap(), rect(180.0, 0.0, 80.0, 10.0));
    assert_eq!(two.join().unwrap(), rect(200.0, 37.0, 80.0, 10.0));
}

#[test]
fn scalar_dag_padding_and_paint_offsets_use_the_current_viewport() {
    let program = Arc::new(
        Program::new(
            vec![
                Expression::Constant(0.0),
                Expression::ViewportWidth,
                Expression::ViewportHeight,
                Expression::Driver {
                    node: 900,
                    channel: 0,
                },
                Expression::Constant(2.0),
                Expression::Multiply(3, 4),
                Expression::Max(0, 5),
                Expression::Constant(20.0),
                Expression::Min(6, 7),
                Expression::Constant(0.1),
                Expression::Multiply(2, 9),
                Expression::Constant(1.0),
                Expression::Multiply(3, 9),
                Expression::Max(0, 12),
                Expression::Min(11, 13),
                Expression::Add(4, 4),
            ],
            vec![
                Node {
                    id: 1,
                    layout: Layout::Fixed {
                        recorded: rect(0.0, 0.0, 20.0, 10.0),
                    },
                    children: vec![],
                },
                Node {
                    id: 2,
                    layout: Layout::Padding {
                        width: Some(1),
                        height: None,
                        insets: [8, 4, 4, 4],
                    },
                    children: vec![0],
                },
            ],
            vec![
                Binding {
                    node: 0,
                    target: Target::OffsetX,
                    expression: 15,
                },
                Binding {
                    node: 0,
                    target: Target::OffsetY,
                    expression: 10,
                },
                Binding {
                    node: 0,
                    target: Target::Opacity,
                    expression: 14,
                },
            ],
            [0, 0],
            rect(0.0, 0.0, 200.0, 200.0),
        )
        .unwrap(),
    );
    let mut state = State::default();
    let frame = state
        .sample(&program, [200.0, 100.0], &[input(900, 3.0)])
        .unwrap();
    assert_eq!(frame[0].bounds, rect(10.0, 12.0, 20.0, 10.0));
    assert!((frame[0].opacity - 0.3).abs() < f32::EPSILON);
    let frame = state
        .sample(&program, [200.0, 200.0], &[input(900, 30.0)])
        .unwrap();
    assert_eq!(frame[0].bounds, rect(24.0, 22.0, 20.0, 10.0));
    assert_eq!(frame[0].opacity, 1.0);
}

#[test]
fn failed_sample_invalidates_partial_results_before_recovery() {
    let program = flow();
    let mut state = State::default();
    let _ = state
        .sample(
            &program,
            [400.0, 240.0],
            &[input(900, 200.0), input(901, 10.0)],
        )
        .unwrap();
    assert_eq!(
        state.sample(
            &program,
            [400.0, 240.0],
            &[input(900, 0.0), input(901, f32::NAN)]
        ),
        Err(Error::NonFinite)
    );
    let recovered = state
        .sample(
            &program,
            [400.0, 240.0],
            &[input(900, 0.0), input(901, 10.0)],
        )
        .unwrap();
    assert_eq!(recovered[2].bounds, rect(180.0, 0.0, 80.0, 10.0));
    assert_eq!(state.work().layout_nodes, 4);
}

#[test]
fn paint_offsets_require_both_declared_clip_and_current_viewport_coverage() {
    let build = |clip_width| {
        Arc::new(
            Program::new(
                vec![
                    Expression::Constant(0.0),
                    Expression::Driver {
                        node: 900,
                        channel: 0,
                    },
                ],
                vec![Node {
                    id: 1,
                    layout: Layout::Fixed {
                        recorded: rect(0.0, 0.0, 20.0, 10.0),
                    },
                    children: vec![],
                }],
                vec![Binding {
                    node: 0,
                    target: Target::OffsetX,
                    expression: 1,
                }],
                [0, 0],
                rect(0.0, 0.0, clip_width, 100.0),
            )
            .unwrap(),
        )
    };
    let mut state = State::default();
    let program = build(100.0);
    assert!(
        state
            .sample(&program, [100.0, 100.0], &[input(900, 80.0)])
            .is_ok()
    );
    assert_eq!(
        state.sample(&program, [90.0, 100.0], &[input(900, 80.0)]),
        Err(Error::ClipCoverage(1))
    );
    assert_eq!(
        state.sample(&build(90.0), [100.0, 100.0], &[input(900, 80.0)]),
        Err(Error::ClipCoverage(1))
    );
}

struct TextMetrics {
    calls: usize,
    natural: Option<[f32; 2]>,
}

impl TextMeasurer for TextMetrics {
    fn measure(&mut self, _: u64, width: f32, _: Option<f32>) -> Result<[f32; 2], Error> {
        self.calls += 1;
        Ok(self
            .natural
            .unwrap_or([width.min(60.0), (200.0 / width).ceil() * 10.0]))
    }
}

fn text_stack(shrink_width: bool, height: Option<usize>) -> Arc<Program> {
    Arc::new(
        Program::new(
            vec![
                Expression::Constant(0.0),
                Expression::Driver {
                    node: 900,
                    channel: 0,
                },
                Expression::Constant(5.0),
                Expression::Constant(30.0),
            ],
            vec![
                Node {
                    id: 1,
                    layout: Layout::Text {
                        recorded: rect(0.0, 0.0, 200.0, 10.0),
                        width: None,
                        height,
                        shrink_width,
                        max_height: None,
                        overflow: TextOverflow::Refuse,
                    },
                    children: vec![],
                },
                Node {
                    id: 2,
                    layout: Layout::Fixed {
                        recorded: rect(0.0, 15.0, 40.0, 10.0),
                    },
                    children: vec![],
                },
                Node {
                    id: 3,
                    layout: Layout::Stack {
                        axis: Axis::Vertical,
                        width: Some(1),
                        height: None,
                        gap: 2,
                    },
                    children: vec![0, 1],
                },
            ],
            vec![],
            [0, 0],
            rect(0.0, 0.0, 400.0, 240.0),
        )
        .unwrap(),
    )
}

#[test]
fn text_measurement_reflows_siblings_and_reuses_unchanged_inputs() {
    let program = text_stack(false, None);
    let mut state = State::default();
    let revision = Arc::new(());
    let mut text = TextMetrics {
        calls: 0,
        natural: None,
    };
    let first = state
        .sample_with_text(
            &program,
            [400.0, 240.0],
            &[input(900, 200.0)],
            &revision,
            &mut text,
        )
        .unwrap();
    assert_eq!(first[1].bounds.y, 15.0);
    let wrapped = state
        .sample_with_text(
            &program,
            [400.0, 240.0],
            &[input(900, 80.0)],
            &revision,
            &mut text,
        )
        .unwrap()
        .to_vec();
    assert_eq!(wrapped[0].bounds.height, 30.0);
    assert_eq!(wrapped[1].bounds.y, 35.0);
    assert_eq!(state.work().text_measurements, 1);
    let repeated = state
        .sample_with_text(
            &program,
            [400.0, 240.0],
            &[input(900, 80.0)],
            &revision,
            &mut text,
        )
        .unwrap();
    assert_eq!(repeated, wrapped);
    assert_eq!(text.calls, 2);
    assert_eq!(state.work().layout_nodes, 0);
    assert_eq!(state.work().text_measurements, 0);
}

#[test]
fn text_revision_remeasures_when_program_and_drivers_are_unchanged() {
    let program = text_stack(false, None);
    let mut state = State::default();
    let first = Arc::new(());
    let mut text = TextMetrics {
        calls: 0,
        natural: Some([60.0, 20.0]),
    };
    let _ = state
        .sample_with_text(
            &program,
            [400.0, 240.0],
            &[input(900, 200.0)],
            &first,
            &mut text,
        )
        .unwrap();
    text.natural = Some([60.0, 40.0]);
    let next = Arc::new(());
    let updated = state
        .sample_with_text(
            &program,
            [400.0, 240.0],
            &[input(900, 200.0)],
            &next,
            &mut text,
        )
        .unwrap();
    assert_eq!(updated[1].bounds.y, 45.0);
    assert_eq!(text.calls, 2);
    assert_eq!(state.work().text_measurements, 1);
}

#[test]
fn text_measurement_refuses_nonfinite_geometry_and_recovers() {
    let program = text_stack(false, None);
    let mut state = State::default();
    let revision = Arc::new(());
    let mut text = TextMetrics {
        calls: 0,
        natural: Some([f32::NAN, 20.0]),
    };
    assert_eq!(
        state.sample_with_text(
            &program,
            [400.0, 240.0],
            &[input(900, 200.0)],
            &revision,
            &mut text
        ),
        Err(Error::ReflowRequired(1))
    );
    text.natural = Some([60.0, 20.0]);
    let recovered = state
        .sample_with_text(
            &program,
            [400.0, 240.0],
            &[input(900, 200.0)],
            &revision,
            &mut text,
        )
        .unwrap();
    assert_eq!(recovered[1].bounds.y, 25.0);
    assert_eq!(text.calls, 2);
}

#[test]
fn text_leaf_requires_leaf_topology_and_backend_measurement() {
    let program = text_stack(false, None);
    let mut state = State::default();
    assert_eq!(
        state.sample(&program, [400.0, 240.0], &[input(900, 200.0)]),
        Err(Error::TextIdentity(1))
    );
    let mut nodes = program.nodes.clone();
    nodes[1] = Node {
        id: 2,
        layout: nodes[0].layout.clone(),
        children: vec![0],
    };
    assert!(
        Program::new(
            program.expressions.clone(),
            nodes,
            vec![],
            [0, 0],
            program.clip
        )
        .is_err()
    );
    let mut text = TextMetrics {
        calls: 0,
        natural: None,
    };
    let _ = state
        .sample_with_text(
            &program,
            [400.0, 240.0],
            &[input(900, 200.0)],
            &Arc::new(()),
            &mut text,
        )
        .unwrap();
    assert_eq!(
        state.sample(&program, [400.0, 240.0], &[input(900, 200.0)]),
        Err(Error::TextIdentity(1))
    );
}

#[test]
fn text_shrink_width_and_explicit_height_use_intrinsic_metrics() {
    let program = text_stack(true, Some(3));
    let mut state = State::default();
    let revision = Arc::new(());
    let mut text = TextMetrics {
        calls: 0,
        natural: Some([60.0, 20.0]),
    };
    let layout = state
        .sample_with_text(
            &program,
            [400.0, 240.0],
            &[input(900, 200.0)],
            &revision,
            &mut text,
        )
        .unwrap();
    assert_eq!(
        [layout[0].bounds.width, layout[0].bounds.height],
        [60.0, 30.0]
    );
    text.natural = Some([201.0, 20.0]);
    assert_eq!(
        state.sample_with_text(
            &program,
            [400.0, 240.0],
            &[input(900, 200.0)],
            &Arc::new(()),
            &mut text
        ),
        Err(Error::ReflowRequired(1))
    );
    text.natural = Some([60.0, 31.0]);
    assert_eq!(
        state.sample_with_text(
            &program,
            [400.0, 240.0],
            &[input(900, 200.0)],
            &Arc::new(()),
            &mut text
        ),
        Err(Error::ReflowRequired(1))
    );
}

#[test]
fn scene_clipped_text_constrains_intrinsic_metrics_without_scaling_glyphs() {
    let original = text_stack(false, None);
    let mut nodes = original.nodes.clone();
    let Layout::Text {
        max_height,
        overflow,
        ..
    } = &mut nodes[0].layout
    else {
        unreachable!()
    };
    *max_height = Some(3);
    *overflow = TextOverflow::SceneClip;
    let program = Arc::new(
        Program::new(
            original.expressions.clone(),
            nodes,
            vec![],
            [0, 0],
            original.clip,
        )
        .unwrap(),
    );
    struct Metrics {
        height: Option<f32>,
    }
    impl TextMeasurer for Metrics {
        fn measure(&mut self, _: u64, _: f32, height: Option<f32>) -> Result<[f32; 2], Error> {
            self.height = height;
            Ok([1000.0, 80.0])
        }
    }
    let mut text = Metrics { height: None };
    let mut state = State::default();
    let placements = state
        .sample_with_text(
            &program,
            [400.0, 240.0],
            &[input(900, 150.0)],
            &Arc::new(()),
            &mut text,
        )
        .unwrap();
    assert_eq!(
        [placements[0].bounds.width, placements[0].bounds.height],
        [150.0, 30.0]
    );
    assert_eq!(placements[1].bounds.y, 35.0);
    assert_eq!(text.height, Some(30.0));
}

#[test]
fn scene_clipped_text_requires_the_recordings_actual_clip_coverage() {
    let original = text_stack(false, None);
    let larger = rect(0.0, 0.0, 800.0, 480.0);
    original.validate_scene(&[1, 2], larger).unwrap();
    let mut nodes = original.nodes.clone();
    let Layout::Text { overflow, .. } = &mut nodes[0].layout else {
        unreachable!()
    };
    *overflow = TextOverflow::SceneClip;
    let program = Program::new(
        original.expressions.clone(),
        nodes,
        vec![],
        [0, 0],
        original.clip,
    )
    .unwrap();
    program.validate_scene(&[1, 2], original.clip).unwrap();
    assert_eq!(
        program.validate_scene(&[1, 2], larger),
        Err(Error::ClipCoverage(1))
    );
}

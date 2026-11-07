// Modified by enzUI contributors. See FORK.md.
use crate::core::renderer::docked::Tenure;

pub(crate) fn eligible(recording: &crate::core::renderer::Recording, required: bool) -> bool {
    !required || recording.presentation().is_some()
}

pub(crate) fn record<C, R>(
    owned: bool,
    context: &mut C,
    quiesce: impl FnOnce(&mut C),
    draw: impl FnOnce(&mut C) -> R,
) -> R {
    if owned {
        quiesce(context);
    }
    draw(context)
}

pub(crate) fn record_replacement<C, R, E>(
    owned: bool,
    running: bool,
    context: &mut C,
    stage: impl FnOnce(&mut C) -> Result<R, E>,
    quiesce: impl FnOnce(&mut C),
    draw: impl FnOnce(&mut C) -> R,
) -> R {
    if owned
        && running
        && let Ok(recording) = stage(context)
    {
        return recording;
    }
    record(owned, context, quiesce, draw)
}

pub(crate) fn prepare_ui_frame<C>(
    context: &mut C,
    reclaim: impl FnOnce(&mut C) -> bool,
    recording: impl FnOnce(&C) -> Option<crate::core::renderer::Recording>,
    retire: impl FnOnce(&mut C),
) -> bool {
    if !reclaim(context) {
        return false;
    }
    if recording(context).as_ref().is_some_and(|recording| {
        recording
            .docked_tenure()
            .is_some_and(|tenure| !tenure.activate())
    }) {
        retire(context);
    }
    true
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PresentationOwner {
    Ui,
    Lane,
}

pub(crate) fn present_or_handover<C, E>(
    recording: Option<&crate::core::renderer::Recording>,
    handover: bool,
    context: &mut C,
    mut start: impl FnMut(&mut C) -> bool,
    present: impl FnOnce(&mut C) -> Result<(), E>,
) -> Result<PresentationOwner, E> {
    let early = handover
        && recording.is_some_and(|recording| {
            recording.presentation().is_some()
                && recording.docked_tenure().is_some_and(Tenure::is_current)
        });
    if early && start(context) {
        return Ok(PresentationOwner::Lane);
    }
    present(context)?;
    if handover && !early {
        let _ = start(context);
    }
    Ok(PresentationOwner::Ui)
}

pub(crate) fn frame_program(
    running: bool,
    recording: Option<&crate::core::renderer::Recording>,
) -> Option<std::sync::Arc<crate::core::renderer::presentation::Program>> {
    let recording = recording
        .filter(|recording| running && recording.docked_tenure().is_some_and(Tenure::is_current))?;
    recording.presentation().cloned()
}

pub(crate) fn owned_frame_request(
    deadline: Option<crate::core::time::Instant>,
) -> crate::core::window::RedrawRequest {
    deadline.map_or(
        crate::core::window::RedrawRequest::Wait,
        crate::core::window::RedrawRequest::At,
    )
}

pub(crate) fn finish_frame<C, E>(
    context: &mut C,
    requested: Option<crate::core::window::RedrawRequest>,
    deadline: Option<crate::core::time::Instant>,
    present: impl FnOnce(&mut C) -> Result<PresentationOwner, E>,
    schedule: impl FnOnce(&mut C, crate::core::window::RedrawRequest),
) -> Result<PresentationOwner, E> {
    let result = present(context);
    if let Some(requested) = requested {
        let request = if matches!(&result, Ok(PresentationOwner::Lane)) {
            owned_frame_request(deadline)
        } else {
            requested
        };
        schedule(context, request);
    }
    result
}

pub(crate) fn preparation_delay(
    recording: &crate::core::renderer::Recording,
    animations: &[crate::core::renderer::TrackAnimation],
    active: &[crate::graphics::presentation::TrackGeneration],
    restart_allowed: bool,
    now: crate::core::time::Instant,
) -> Option<crate::core::time::Duration> {
    if !restart_allowed
        || recording.presentation().is_none()
        || !recording.docked_tenure().is_some_and(Tenure::is_current)
        || animations.is_empty()
        || animations.len() > 8
        || animations.iter().any(|animation| {
            !animation.is_usable()
                || animation.start != animations[0].start
                || active.iter().any(|current| {
                    current.node == animation.node && current.generation == animation.generation
                })
        })
    {
        return None;
    }
    now.checked_duration_since(animations[0].start)
        .filter(|delay| !delay.is_zero())
}

#[cfg(test)]
mod tests {
    use super::eligible;
    use crate::core::{
        Shell,
        renderer::{NodeId, Recording, Track},
        shell::Waker,
        window::Headless,
    };

    #[test]
    fn frame_schedule_handover_does_not_enqueue_a_stale_ui_redraw() {
        use crate::core::window::RedrawRequest;
        for lane in [true, false] {
            let mut events = Vec::new();
            let result: Result<_, ()> = super::finish_frame(
                &mut events,
                Some(RedrawRequest::NextFrame),
                None,
                |events| {
                    events.push("present");
                    Ok(if lane {
                        super::PresentationOwner::Lane
                    } else {
                        super::PresentationOwner::Ui
                    })
                },
                |events, request| {
                    assert_eq!(events.as_slice(), &["present"]);
                    if request == RedrawRequest::NextFrame {
                        events.push("queued-ui-redraw");
                    }
                },
            );
            assert!(result.is_ok());
            assert_eq!(
                events,
                if lane {
                    vec!["present"]
                } else {
                    vec!["present", "queued-ui-redraw"]
                }
            );
        }
    }

    #[test]
    fn frame_schedule_failure_preserves_the_requested_retry_and_absence_stays_absent() {
        use crate::core::window::RedrawRequest;
        let mut requests = Vec::new();
        let failure: Result<super::PresentationOwner, u8> = super::finish_frame(
            &mut requests,
            Some(RedrawRequest::NextFrame),
            None,
            |_| Err(7),
            Vec::push,
        );
        assert_eq!(failure, Err(7));
        assert_eq!(requests, vec![RedrawRequest::NextFrame]);
        let result: Result<_, ()> = super::finish_frame(
            &mut requests,
            None,
            None,
            |_| Ok(super::PresentationOwner::Ui),
            |_, _| panic!("no request was made"),
        );
        assert!(result.is_ok());
        assert_eq!(requests.len(), 1);
    }

    #[test]
    fn layout_ownership_docked_publication_survives_merge() {
        let window = Headless;
        let mut parent_messages = crate::core::shell::Bus::<()>::new();
        let mut child_messages = crate::core::shell::Bus::<()>::new();
        let mut parent = Shell::new(&window, Waker::noop(), &mut parent_messages);
        let mut child = parent.local(&mut child_messages);
        child.publish_docked_tracks([Track::identity(NodeId(9)), Track::identity(NodeId(10))]);
        assert!(child.requires_layout_ownership());
        parent.merge(child, |message| message);
        assert!(parent.requires_layout_ownership());
        assert_eq!(parent.take_tracks().len(), 2);
        assert!(parent.requires_layout_ownership());
    }

    #[test]
    fn layout_ownership_plain_and_empty_publications_keep_legacy_semantics() {
        let window = Headless;
        let mut messages = crate::core::shell::Bus::<()>::new();
        let mut shell = Shell::new(&window, Waker::noop(), &mut messages);
        shell.publish_docked_tracks([]);
        assert!(!shell.requires_layout_ownership());
        shell.publish_track(Track::identity(NodeId(9)));
        assert!(!shell.requires_layout_ownership());
        assert!(eligible(&Recording::new(()), false));
    }

    #[test]
    fn layout_ownership_docked_request_refuses_transform_only_cached_scene() {
        use crate::core::renderer::presentation::{Bounds, Expression, Layout, Node, Program};
        let plain = Recording::new(());
        assert!(!eligible(&plain, true));
        let bounds = Bounds {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        };
        let program = Program::new(
            vec![Expression::Constant(0.0)],
            vec![Node {
                id: 1,
                layout: Layout::Fixed { recorded: bounds },
                children: vec![],
            }],
            vec![],
            [0, 0],
            bounds,
        )
        .unwrap();
        let owned = Recording::new(()).with_presentation(std::sync::Arc::new(program));
        assert!(eligible(&owned, true));
    }
    struct Publisher {
        overlay: bool,
    }
    struct OverlayPublisher;

    impl crate::core::Widget<(), (), ()> for Publisher {
        fn size(&self) -> crate::core::Size<crate::core::Length> {
            crate::core::Size::new(crate::core::Length::Fill, crate::core::Length::Fill)
        }
        fn layout(
            &mut self,
            tree: &mut crate::core::widget::Tree,
            _: &(),
            limits: &crate::core::layout::Limits,
        ) {
            crate::core::layout::Node::new(limits.max()).apply(tree);
        }
        fn draw(
            &self,
            _: &crate::core::widget::Tree,
            _: &mut (),
            _: &(),
            _: &crate::core::renderer::Style,
            _: crate::core::Layout,
            _: crate::core::mouse::Cursor,
            _: &crate::core::Rectangle,
        ) {
        }
        fn update(
            &mut self,
            _: &mut crate::core::widget::Tree,
            _: &crate::core::Event,
            _: crate::core::Layout,
            _: crate::core::mouse::Cursor,
            _: &(),
            shell: &mut Shell<'_, ()>,
            _: &crate::core::Rectangle,
        ) {
            if !self.overlay {
                shell.publish_docked_tracks([Track::identity(NodeId(9))]);
            }
        }
        fn overlay<'a>(
            &'a mut self,
            _: &'a mut crate::core::widget::Tree,
            _: crate::core::Layout,
            _: &(),
            _: &crate::core::Rectangle,
            _: crate::core::Vector,
            _: crate::core::Size,
        ) -> Vec<crate::core::overlay::Element<'a, (), (), ()>> {
            self.overlay
                .then(|| crate::core::overlay::Element::new(Box::new(OverlayPublisher)))
                .into_iter()
                .collect()
        }
    }
    impl crate::core::Overlay<(), (), ()> for OverlayPublisher {
        fn draw(
            &self,
            _: &mut (),
            _: &(),
            _: &crate::core::renderer::Style,
            _: crate::core::mouse::Cursor,
        ) {
        }
        fn update(
            &mut self,
            _: &crate::core::Event,
            _: crate::core::mouse::Cursor,
            _: &(),
            shell: &mut Shell<'_, ()>,
        ) {
            shell.publish_docked_tracks([Track::identity(NodeId(9))]);
        }
    }
    fn dispatched(overlay: bool) {
        use crate::runtime::user_interface::{Cache, State, UserInterface};
        let mut renderer = ();
        let mut ui = UserInterface::build(
            crate::core::Element::new(Publisher { overlay }),
            crate::core::Size::new(100.0, 100.0),
            Cache::new(),
            &mut renderer,
        );
        let (state, statuses) = ui.update(
            &Headless,
            &Waker::noop(),
            &[crate::core::Event::Mouse(
                crate::core::mouse::Event::CursorMoved {
                    position: crate::core::Point::new(10.0, 10.0),
                },
            )],
            crate::core::mouse::Cursor::Unavailable,
            &mut renderer,
            &mut crate::core::shell::Bus::new(),
        );
        assert_eq!(statuses.len(), 1);
        let State::Updated {
            requires_layout_ownership,
            tracks,
            ..
        } = state
        else {
            panic!("fixture changed its structure")
        };
        assert!(requires_layout_ownership);
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].node, NodeId(9));
    }
    #[test]
    fn layout_ownership_base_dispatch_carries_the_typed_requirement() {
        dispatched(false);
    }
    #[test]
    fn layout_ownership_overlay_dispatch_carries_the_typed_requirement() {
        dispatched(true);
    }
    #[test]
    fn layout_ownership_replacement_quiesces_before_expiring_the_old_tenure() {
        use crate::core::{
            Length, Padding, Size,
            layout::{Limits, Node},
        };
        let recorder = crate::core::renderer::docked::Recorder::default();
        recorder.observe_layout();
        let tenure = recorder.tenure();
        let mut quiesced = false;
        let result = super::record(
            true,
            &mut quiesced,
            |quiesced| {
                assert!(tenure.is_current());
                *quiesced = true;
            },
            |quiesced| {
                assert!(*quiesced, "recording must not expire a live lane's tenure");
                let _ = Node::new(Size::new(1.0, 1.0)).with_clip_presentation(
                    Limits::new(Size::ZERO, Size::new(1.0, 1.0)),
                    Size::new(Length::Fill, Length::Fill),
                    Padding::ZERO,
                );
                recorder.observe_layout();
                assert!(!tenure.is_current());
                42
            },
        );
        assert_eq!(result, 42);
    }

    #[test]
    fn layout_ownership_transform_only_replacement_does_not_quiesce() {
        assert_eq!(
            super::record(
                false,
                &mut (),
                |_| panic!("legacy scene unexpectedly quiesced"),
                |_| 42
            ),
            42
        );
    }
    fn handover_recording() -> Recording {
        let recorder = crate::core::renderer::docked::Recorder::default();
        recorder.observe_layout();
        Recording::new(())
            .with_presentation(driver_program(&[1]))
            .with_docked_tenure(recorder.tenure())
    }

    #[test]
    fn layout_ownership_handoff_owned_recording_skips_preliminary_ui_submission() {
        let mut calls = Vec::new();
        let owner = super::present_or_handover(
            Some(&handover_recording()),
            true,
            &mut calls,
            |calls| {
                calls.push("lane");
                true
            },
            |_| -> Result<(), ()> { panic!("owned recording unnecessarily submitted on UI") },
        )
        .unwrap();
        assert_eq!(owner, super::PresentationOwner::Lane);
        assert_eq!(calls, ["lane"]);
    }

    #[test]
    fn layout_ownership_handoff_transform_only_keeps_ui_baseline() {
        let mut calls = Vec::new();
        let owner = super::present_or_handover(
            Some(&Recording::new(())),
            true,
            &mut calls,
            |calls| {
                calls.push("lane");
                true
            },
            |calls| {
                calls.push("ui");
                Ok::<_, ()>(())
            },
        )
        .unwrap();
        assert_eq!(owner, super::PresentationOwner::Ui);
        assert_eq!(calls, ["ui", "lane"]);
    }

    #[test]
    fn layout_ownership_handoff_program_without_tenure_keeps_ui_baseline() {
        let recording = Recording::new(()).with_presentation(driver_program(&[1]));
        let mut calls = Vec::new();
        let _ = super::present_or_handover(
            Some(&recording),
            true,
            &mut calls,
            |calls| {
                calls.push("lane");
                true
            },
            |calls| {
                calls.push("ui");
                Ok::<_, ()>(())
            },
        )
        .unwrap();
        assert_eq!(calls, ["ui", "lane"]);
    }

    #[test]
    fn layout_ownership_handoff_refused_owned_transfer_presents_without_retry() {
        let mut calls = Vec::new();
        let owner = super::present_or_handover(
            Some(&handover_recording()),
            true,
            &mut calls,
            |calls| {
                calls.push("lane-refused");
                false
            },
            |calls| {
                calls.push("ui");
                Ok::<_, ()>(())
            },
        )
        .unwrap();
        assert_eq!(owner, super::PresentationOwner::Ui);
        assert_eq!(calls, ["lane-refused", "ui"]);
    }

    #[test]
    fn layout_ownership_handoff_failed_ui_submission_does_not_transfer() {
        let result = super::present_or_handover(
            Some(&Recording::new(())),
            true,
            &mut (),
            |_| panic!("failed native submission must retain its surface"),
            |_| Err("surface lost"),
        );
        assert_eq!(result, Err("surface lost"));
    }

    #[test]
    fn layout_ownership_handoff_ineligible_plan_never_transfers() {
        let result = super::present_or_handover(
            Some(&handover_recording()),
            false,
            &mut (),
            |_| panic!("non-handover plan attempted a transfer"),
            |_| Ok::<_, ()>(()),
        );
        assert_eq!(result, Ok(super::PresentationOwner::Ui));
    }

    #[test]
    fn layout_ownership_handoff_tenure_without_program_keeps_ui_baseline() {
        let recorder = crate::core::renderer::docked::Recorder::default();
        recorder.observe_layout();
        let recording = Recording::new(()).with_docked_tenure(recorder.tenure());
        let mut calls = Vec::new();
        let _ = super::present_or_handover(
            Some(&recording),
            true,
            &mut calls,
            |calls| {
                calls.push("lane");
                true
            },
            |calls| {
                calls.push("ui");
                Ok::<_, ()>(())
            },
        )
        .unwrap();
        assert_eq!(calls, ["ui", "lane"]);
    }

    #[test]
    fn layout_ownership_handoff_expired_tenure_keeps_ui_baseline() {
        use crate::core::{
            Length, Padding, Size,
            layout::{Limits, Node},
        };
        let recorder = crate::core::renderer::docked::Recorder::default();
        recorder.observe_layout();
        let recording = Recording::new(())
            .with_presentation(driver_program(&[1]))
            .with_docked_tenure(recorder.tenure());
        let _ = Node::new(Size::new(1.0, 1.0)).with_clip_presentation(
            Limits::new(Size::ZERO, Size::new(1.0, 1.0)),
            Size::new(Length::Fill, Length::Fill),
            Padding::ZERO,
        );
        recorder.observe_layout();
        assert!(!recording.docked_tenure().unwrap().is_current());
        let mut calls = Vec::new();
        let _ = super::present_or_handover(
            Some(&recording),
            true,
            &mut calls,
            |calls| {
                calls.push("lane");
                true
            },
            |calls| {
                calls.push("ui");
                Ok::<_, ()>(())
            },
        )
        .unwrap();
        assert_eq!(calls, ["ui", "lane"]);
    }

    #[test]
    fn clock_rebase_preserves_fractional_sample_and_future_curve_exactly() {
        use crate::core::animation::{Animation, Easing};
        use std::time::{Duration, Instant};
        let start = Instant::now();
        let original = Animation::new(false)
            .duration(Duration::from_millis(180))
            .easing(Easing::EaseOut)
            .go(true, start);
        let mut shifted = original.clone();
        let delay = Duration::from_micros(123_456);
        assert!(shifted.shift_clock(delay));
        for elapsed in [0, 1234, 37001, 89012, 180000, 250000] {
            let at = start + Duration::from_micros(elapsed);
            assert_eq!(
                shifted.interpolate(0.0, 1.0, at + delay),
                original.interpolate(0.0, 1.0, at)
            );
            assert_eq!(shifted.is_animating(at + delay), original.is_animating(at));
        }
    }

    #[test]
    fn clock_rebase_subsequent_reversal_has_no_repeated_delay() {
        use crate::core::animation::{Animation, Easing};
        use std::time::{Duration, Instant};
        let start = Instant::now();
        let mut original = Animation::new(false)
            .duration(Duration::from_millis(180))
            .easing(Easing::EaseOut)
            .go(true, start);
        let mut shifted = original.clone();
        let delay = Duration::from_millis(100);
        assert!(shifted.shift_clock(delay));
        let reverse = start + Duration::from_millis(70);
        original.go_mut(false, reverse);
        shifted.go_mut(false, reverse + delay);
        for elapsed in [0, 1, 20, 70, 180] {
            let at = reverse + Duration::from_millis(elapsed);
            assert_eq!(
                shifted.interpolate(0.0, 1.0, at + delay),
                original.interpolate(0.0, 1.0, at)
            );
        }
    }

    #[test]
    fn clock_rebase_overflow_refuses_without_mutation() {
        use crate::core::animation::Animation;
        use std::time::{Duration, Instant};
        let start = Instant::now();
        let mut animation = Animation::new(false).go(true, start);
        assert!(animation.shift_clock(Duration::MAX));
        let before = animation.interpolate(0.0, 1.0, start + Duration::from_secs(1));
        assert!(!animation.can_shift_clock(Duration::from_nanos(1)));
        assert!(!animation.shift_clock(Duration::from_nanos(1)));
        assert_eq!(
            animation.interpolate(0.0, 1.0, start + Duration::from_secs(1)),
            before
        );
    }

    fn driver_program(
        drivers: &[u64],
    ) -> std::sync::Arc<crate::core::renderer::presentation::Program> {
        use crate::core::{
            Length, Size,
            layout::{Limits, responsive::Rule},
            renderer::presentation::{Bounds, Expression, Layout, Node, Program, Responsive},
        };
        let mut expressions = drivers
            .iter()
            .map(|node| Expression::Driver {
                node: *node,
                channel: 0,
            })
            .collect::<Vec<_>>();
        let origin = expressions.len();
        expressions.push(Expression::Constant(0.0));
        let mut nodes = vec![Node {
            id: 100,
            layout: Layout::Fixed {
                recorded: Bounds {
                    x: 0.0,
                    y: 0.0,
                    width: 10.0,
                    height: 10.0,
                },
            },
            children: vec![],
        }];
        for (expression, driver) in drivers.iter().enumerate().rev() {
            let child = nodes.len() - 1;
            nodes.push(Node {
                id: 100 + u64::try_from(nodes.len()).unwrap(),
                layout: Layout::Responsive(std::sync::Arc::new(Responsive {
                    limits: Limits::new(Size::ZERO, Size::new(100.0, 100.0)),
                    rule: Rule::Reserve {
                        resting: 100.0,
                        progress: 0.5,
                        align_end: false,
                    },
                    child: Size::new(Length::Shrink, Length::Shrink),
                    driver: Some(*driver),
                    progress: Some(expression),
                })),
                children: vec![child],
            });
        }
        std::sync::Arc::new(
            Program::new(
                expressions,
                nodes,
                vec![],
                [origin, origin],
                Bounds {
                    x: 0.0,
                    y: 0.0,
                    width: 100.0,
                    height: 100.0,
                },
            )
            .unwrap(),
        )
    }
    fn proposed_animation(node: u64) -> crate::core::renderer::TrackAnimation {
        crate::core::renderer::TrackAnimation {
            node: NodeId(node),
            generation: 9,
            start: std::time::Instant::now(),
            duration: std::time::Duration::from_millis(180),
            keyframes: vec![
                (
                    0.0,
                    Track {
                        translation: crate::core::Vector::new(0.5, 0.0),
                        ..Track::identity(NodeId(node))
                    },
                ),
                (1.0, Track::identity(NodeId(node))),
            ],
        }
    }
    fn proposed_capture() -> crate::core::renderer::docked::Capture {
        crate::core::renderer::docked::Capture {
            layout: Err(crate::core::renderer::presentation::Error::DockedOwnershipRequired),
            animation: proposed_animation(1),
            additional: vec![],
            stationary: vec![],
            bounds: crate::core::Rectangle {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            },
        }
    }
    #[test]
    fn docked_publication_binds_away_unreferenced_proposals_before_shell_publication() {
        let program = driver_program(&[1]);
        let mut capture = proposed_capture();
        capture.additional.push(proposed_animation(3));
        capture.stationary.push(Track::identity(NodeId(2)));
        let capture = capture.bind_layout(Ok(program.clone()));
        assert!(capture.layout.is_ok());
        assert!(capture.covers_program(&program));
        assert!(capture.additional.is_empty() && capture.stationary.is_empty());
        let mut messages = crate::core::shell::Bus::<()>::new();
        let mut shell = Shell::new(&Headless, Waker::noop(), &mut messages);
        assert!(capture.publish(&mut shell));
        assert!(shell.requires_layout_ownership());
        assert_eq!(
            shell
                .take_tracks()
                .iter()
                .map(|track| track.node.0)
                .collect::<Vec<_>>(),
            vec![1]
        );
        assert_eq!(shell.take_animations().len(), 1);
    }
    #[test]
    fn docked_publication_reselects_only_a_declared_moving_driver() {
        let mut capture = proposed_capture();
        capture.animation = proposed_animation(3);
        capture.additional.push(proposed_animation(1));
        let capture = capture.bind_layout(Ok(driver_program(&[1])));
        assert!(capture.layout.is_ok());
        assert_eq!(capture.animation.node, NodeId(1));
        assert!(capture.additional.is_empty());
        let mut capture = proposed_capture();
        capture.animation = proposed_animation(3);
        capture.stationary.push(Track::identity(NodeId(1)));
        assert!(
            capture
                .bind_layout(Ok(driver_program(&[1])))
                .layout
                .is_err()
        );
    }
    #[test]
    fn docked_publication_refuses_missing_required_and_malformed_driver_sets() {
        let program = driver_program(&[1, 2]);
        assert!(
            proposed_capture()
                .bind_layout(Ok(program.clone()))
                .layout
                .is_err()
        );
        let mut raw = proposed_capture();
        raw.layout = Ok(program);
        let mut messages = crate::core::shell::Bus::<()>::new();
        let mut shell = Shell::new(&Headless, Waker::noop(), &mut messages);
        assert!(!raw.publish(&mut shell));
        assert!(shell.take_tracks().is_empty() && shell.take_animations().is_empty());
        let mut oversized = proposed_capture();
        oversized.additional = (3..11).map(proposed_animation).collect();
        assert!(
            oversized
                .bind_layout(Ok(driver_program(&[1])))
                .layout
                .is_err()
        );
    }
    #[test]
    fn docked_publication_preserves_lowerer_refusal_without_publishing() {
        use crate::core::renderer::presentation::Error;
        let capture = proposed_capture().bind_layout(Err(Error::UnsupportedBackend));
        assert!(matches!(capture.layout, Err(Error::UnsupportedBackend)));
        let mut messages = crate::core::shell::Bus::<()>::new();
        let mut shell = Shell::new(&Headless, Waker::noop(), &mut messages);
        assert!(!capture.publish(&mut shell));
        assert!(!shell.requires_layout_ownership());
    }
}

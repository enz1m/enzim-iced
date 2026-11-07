// Modified by enzUI contributors. See FORK.md.
//! Bounded observations of compositor-confirmed owned frames, without retaining scene resources.

use super::{FrameKey, FrameOwner, Outcome, Quality, Resolved};
use std::sync::{Arc, Mutex, OnceLock, Weak};

/// A raw compositor receipt for an owned-layout frame, not input or surface-transfer authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Receipt {
    /// Exact joined window, surface, ownership, scene and submission identity.
    pub key: FrameKey,
    /// The compositor's POSIX timestamp clock.
    pub clock_id: u32,
    /// Presentation time in that clock, without a clock-domain conversion.
    pub at_ns: u64,
    /// Advertised output refresh period; always nonzero for these observations.
    pub refresh_ns: u32,
    /// The single output on which this frame was presented.
    pub output_id: u32,
}

/// One latest receipt; it owns no allocation snapshot, paint, font, widget or GPU resource.
///
/// Contention may skip an observation. Callers must bound their wait and independently
/// verify complete cadence rather than interpreting this slot as a frame history.
#[derive(Debug, Default)]
pub struct Watch {
    latest: Mutex<Option<Receipt>>,
}

impl Watch {
    /// Returns a receipt strictly newer than `after_ns` in the requested clock and window.
    /// `None` as the window accepts the first qualifying window; it does not combine clocks.
    pub fn after(&self, clock_id: u32, after_ns: u64, window: Option<u64>) -> Option<Receipt> {
        self.latest.try_lock().ok()?.filter(|receipt| {
            receipt.clock_id == clock_id
                && receipt.at_ns > after_ns
                && window.is_none_or(|window| receipt.key.window_id == window)
        })
    }

    fn record(&self, resolved: &Resolved) {
        let Outcome::Presented(stamp) = &resolved.outcome else {
            return;
        };
        if resolved.quality != Quality::CompositorPresented
            || resolved.frame.owner != FrameOwner::Lane
            || stamp.refresh_ns == 0
            || stamp.output_ids.len() != 1
            || stamp.nanoseconds >= 1_000_000_000
            || resolved.frame.tracks.is_empty()
            || resolved.frame.tracks.len() > 8
        {
            return;
        }
        let Some(layout) = &resolved.frame.layout else {
            return;
        };
        if !layout
            .driver_nodes()
            .eq(resolved.frame.tracks.iter().map(|track| track.node.0))
        {
            return;
        }
        let Some(at_ns) = stamp
            .seconds
            .checked_mul(1_000_000_000)
            .and_then(|value| value.checked_add(u64::from(stamp.nanoseconds)))
        else {
            return;
        };
        let Ok(mut latest) = self.latest.try_lock() else {
            return;
        };
        if latest
            .is_some_and(|previous| previous.clock_id != stamp.clock_id || previous.at_ns >= at_ns)
        {
            return;
        }
        *latest = Some(Receipt {
            key: resolved.frame.key,
            clock_id: stamp.clock_id,
            at_ns,
            refresh_ns: stamp.refresh_ns,
            output_id: stamp.output_ids[0],
        });
    }
}

static OBSERVER: OnceLock<Mutex<Weak<Watch>>> = OnceLock::new();

/// Observes future joined frames, including from windows that already exist.
///
/// Subscribers share a bounded slot. The publisher holds only a weak reference;
/// dropping the last subscriber releases the watch. Poisoned registration returns `None`.
pub fn subscribe() -> Option<Arc<Watch>> {
    let mut slot = OBSERVER
        .get_or_init(|| Mutex::new(Weak::new()))
        .lock()
        .ok()?;
    if let Some(watch) = slot.upgrade() {
        return Some(watch);
    }
    let watch = Arc::new(Watch::default());
    *slot = Arc::downgrade(&watch);
    Some(watch)
}

pub(super) fn publish(resolved: &Resolved) {
    let Some(slot) = OBSERVER.get() else {
        return;
    };
    let watch = slot.try_lock().ok().and_then(|slot| slot.upgrade());
    if let Some(watch) = watch {
        watch.record(resolved);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{
        Length, Size,
        layout::{Limits, responsive::Rule},
        renderer::{
            NodeId,
            presentation::{
                Bounds, Expression, Input, Layout, Node, Program, Responsive, SampleBudget, State,
            },
        },
    };
    use crate::presentation::{
        PresentationSink, PresentationStamp, SubmittedFrame, TrackGeneration,
    };

    fn joined() -> Resolved {
        let bounds = Bounds {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        };
        let program = Arc::new(
            Program::new(
                vec![
                    Expression::Driver {
                        node: 1,
                        channel: 0,
                    },
                    Expression::Constant(0.0),
                ],
                vec![
                    Node {
                        id: 100,
                        // A reserved child keeps its resting width.
                        layout: Layout::Fixed {
                            recorded: Bounds {
                                width: 100.0,
                                height: 10.0,
                                ..bounds
                            },
                        },
                        children: vec![],
                    },
                    Node {
                        id: 101,
                        layout: Layout::Responsive(Arc::new(Responsive {
                            limits: Limits::new(Size::ZERO, Size::new(100.0, 100.0)),
                            rule: Rule::Reserve {
                                resting: 100.0,
                                progress: 0.5,
                                align_end: false,
                            },
                            child: Size::new(Length::Shrink, Length::Shrink),
                            driver: Some(1),
                            progress: Some(0),
                        })),
                        children: vec![0],
                    },
                ],
                vec![],
                [1, 1],
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
                    node: 1,
                    values: [0.5; 8],
                    available: 1,
                }],
            )
            .unwrap();
        let mut frame = SubmittedFrame::new(
            FrameKey {
                window_id: 7,
                surface_generation: 2,
                ownership_generation: 3,
                scene_generation: 4,
                submitted_sequence: 5,
            },
            std::time::Instant::now(),
            FrameOwner::Lane,
            vec![TrackGeneration {
                node: NodeId(1),
                generation: 9,
            }],
        );
        frame.layout = Some(SampleBudget::default().capture(&state).unwrap());
        let sink = PresentationSink::new(2);
        assert!(sink.register(frame.clone()));
        sink.complete_exact(
            &frame,
            Outcome::Presented(PresentationStamp {
                clock_id: 4,
                seconds: 10,
                nanoseconds: 100,
                refresh_ns: 16_666_666,
                sequence: 1,
                flags_raw: 0,
                output_ids: Arc::from([6]),
            }),
        )
        .unwrap()
    }

    #[test]
    fn presentation_watch_uses_exact_clock_window_and_strict_timestamp() {
        let watch = Watch::default();
        let frame = joined();
        watch.record(&frame);
        let receipt = watch.after(4, 10_000_000_000, Some(7)).unwrap();
        assert_eq!(receipt.key, frame.frame.key);
        assert_eq!(receipt.at_ns, 10_000_000_100);
        assert!(watch.after(1, 0, Some(7)).is_none());
        assert!(watch.after(4, 0, Some(8)).is_none());
        assert!(watch.after(4, receipt.at_ns, Some(7)).is_none());
    }

    #[test]
    fn presentation_watch_refuses_submission_discard_unowned_and_unbound_drivers() {
        let original = joined();
        for case in 0..7 {
            let mut frame = original.clone();
            match case {
                0 => frame.quality = Quality::SurfacePresented,
                1 => frame.outcome = Outcome::Discarded,
                2 => frame.frame.owner = FrameOwner::Ui,
                3 => frame.frame.layout = None,
                4 => {
                    frame.frame.tracks = Arc::from([TrackGeneration {
                        node: NodeId(2),
                        generation: 9,
                    }]);
                }
                5 => {
                    let Outcome::Presented(stamp) = &mut frame.outcome else {
                        unreachable!()
                    };
                    stamp.refresh_ns = 0;
                }
                6 => {
                    let Outcome::Presented(stamp) = &mut frame.outcome else {
                        unreachable!()
                    };
                    stamp.output_ids = Arc::from([6, 7]);
                }
                _ => unreachable!(),
            }
            let watch = Watch::default();
            watch.record(&frame);
            assert!(watch.after(4, 0, None).is_none(), "case {case}");
            watch.record(&original);
            assert!(watch.after(4, 0, None).is_some());
        }
    }

    #[test]
    fn presentation_watch_rejects_out_of_order_clock_change_and_overflow() {
        let watch = Watch::default();
        let original = joined();
        watch.record(&original);
        for case in 0..3 {
            let mut frame = original.clone();
            let Outcome::Presented(stamp) = &mut frame.outcome else {
                unreachable!()
            };
            match case {
                0 => stamp.nanoseconds = 99,
                1 => stamp.clock_id = 1,
                2 => stamp.seconds = u64::MAX,
                _ => unreachable!(),
            }
            watch.record(&frame);
            assert_eq!(watch.after(4, 0, None).unwrap().at_ns, 10_000_000_100);
            assert!(watch.after(1, 0, None).is_none());
        }
    }

    #[test]
    fn presentation_watch_does_not_retain_allocation_or_scene_resources() {
        let watch = Watch::default();
        let frame = joined();
        let allocation = Arc::downgrade(frame.frame.layout.as_ref().unwrap());
        watch.record(&frame);
        drop(frame);
        assert!(allocation.upgrade().is_none());
        assert!(watch.after(4, 0, None).is_some());
    }

    #[test]
    fn presentation_watch_publisher_updates_without_reader_thread_progress() {
        let watch = Arc::new(Watch::default());
        let worker_watch = watch.clone();
        let frame = joined();
        let (done, receiver) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            worker_watch.record(&frame);
            done.send(()).unwrap();
        });
        receiver
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(watch.after(4, 0, None).is_some());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !worker.is_finished() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        worker.join().unwrap();
    }

    struct GeometryProbe {
        sink: std::sync::Weak<PresentationSink>,
        count: Arc<std::sync::atomic::AtomicUsize>,
        unlocked: Arc<std::sync::atomic::AtomicBool>,
    }
    impl std::task::Wake for GeometryProbe {
        fn wake(self: Arc<Self>) {
            use std::sync::atomic::Ordering::SeqCst;
            let _ = self.count.fetch_add(1, SeqCst);
            self.unlocked.store(
                self.sink.upgrade().unwrap().inner.try_lock().is_ok(),
                SeqCst,
            );
        }
    }

    #[test]
    fn moving_geometry_wake_requires_confirmed_newest_frame_and_releases_on_close() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
        let resolved = joined();
        let sink = Arc::new(PresentationSink::new(8));
        let count = Arc::new(AtomicUsize::new(0));
        let unlocked = Arc::new(AtomicBool::new(false));
        let probe = Arc::new(GeometryProbe {
            sink: Arc::downgrade(&sink),
            count: count.clone(),
            unlocked: unlocked.clone(),
        });
        let weak = Arc::downgrade(&probe);
        sink.set_geometry_waker(std::task::Waker::from(probe));
        let mut frame = resolved.frame;
        assert!(sink.register(frame.clone()));
        let _ = sink.complete_exact(&frame, Outcome::Discarded).unwrap();
        assert_eq!(count.load(SeqCst), 0);
        frame.key.submitted_sequence += 1;
        assert!(sink.register(frame.clone()));
        let _ = sink
            .complete_exact(&frame, resolved.outcome.clone())
            .unwrap();
        assert_eq!(count.load(SeqCst), 1);
        assert!(unlocked.load(SeqCst));
        frame.key.submitted_sequence += 1;
        assert!(sink.register(frame.clone()));
        let Outcome::Presented(mut stamp) = resolved.outcome else {
            unreachable!()
        };
        stamp.seconds -= 1;
        let _ = sink
            .complete_exact(&frame, Outcome::Presented(stamp))
            .unwrap();
        assert_eq!(count.load(SeqCst), 1);
        assert!(weak.upgrade().is_some());
        let _ = sink.close();
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn moving_geometry_wake_refuses_ui_and_layoutless_receipts() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
        let resolved = joined();
        let sink = Arc::new(PresentationSink::new(4));
        let count = Arc::new(AtomicUsize::new(0));
        sink.set_geometry_waker(std::task::Waker::from(Arc::new(GeometryProbe {
            sink: Arc::downgrade(&sink),
            count: count.clone(),
            unlocked: Arc::new(AtomicBool::new(false)),
        })));
        let mut frame = resolved.frame;
        frame.owner = FrameOwner::Ui;
        assert!(sink.register(frame.clone()));
        let _ = sink
            .complete_exact(&frame, resolved.outcome.clone())
            .unwrap();
        frame.key.submitted_sequence += 1;
        frame.owner = FrameOwner::Lane;
        frame.layout = None;
        assert!(sink.register(frame.clone()));
        let _ = sink.complete_exact(&frame, resolved.outcome).unwrap();
        assert_eq!(count.load(SeqCst), 0);
    }
}

// Modified by enzUI contributors. See FORK.md.
use super::*;
use crate::core::layout::Measure as _;
use crate::core::renderer::{Headless, Recording};
use crate::core::{Color, Font, Rectangle, Renderer as _, Size};
use crate::graphics::{Antialiasing, Shell};
use crate::{Engine, Renderer, core};
use std::sync::mpsc;
use std::time::Duration;

const SIZE: Size<u32> = Size::new(320, 120);

fn engine() -> Engine {
    pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY),
            flags: wgpu::InstanceFlags::empty(),
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: None,
            })
            .await
            .expect("a real adapter is required for text ownership verification");
        eprintln!("owned-text adapter: {:?}", adapter.get_info());
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("owned text resources"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits {
                    max_bind_groups: 2,
                    ..wgpu::Limits::default()
                },
                memory_hints: wgpu::MemoryHints::MemoryUsage,
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
            })
            .await
            .expect("device");
        Engine::new(
            &adapter,
            device,
            queue,
            if crate::graphics::color::GAMMA_CORRECTION {
                wgpu::TextureFormat::Rgba8UnormSrgb
            } else {
                wgpu::TextureFormat::Rgba8Unorm
            },
            Some(Antialiasing::MSAAx4),
            Shell::headless(),
        )
    })
}

fn bounds() -> Rectangle {
    Rectangle::with_size(Size::new(SIZE.width as f32, SIZE.height as f32))
}

fn paint(renderer: &mut Renderer, font: Font) -> crate::graphics::text::Paragraph {
    use core::text::{Paragraph as _, Renderer as _};
    let definition = core::text::Text {
        content: "Owned glyphs — AAVV iii WWW",
        bounds: Size::new(300.0, 50.0),
        size: 22.0.into(),
        line_height: core::text::LineHeight::Relative(1.2),
        font,
        align_x: core::text::Alignment::Left,
        align_y: core::alignment::Vertical::Top,
        shaping: core::text::Shaping::Advanced,
        hint_factor: None,
        wrapping: core::text::Wrapping::Word,
        ellipsis: core::text::Ellipsis::None,
    };
    renderer.fill_text(
        definition.with_content(definition.content.to_owned()),
        core::Point::new(10.0, 10.0),
        Color::WHITE,
        bounds(),
    );
    let paragraph = crate::graphics::text::Paragraph::with_text(core::text::Text {
        content: "A retained hinted paragraph",
        size: 16.0.into(),
        hint_factor: Some(1.0),
        bounds: Size::new(300.0, 40.0),
        ..definition
    });
    renderer.fill_paragraph(
        &paragraph,
        core::Point::new(10.0, 65.0),
        Color::WHITE,
        bounds(),
    );
    paragraph
}

fn record(renderer: &mut Renderer, font: Font) -> Recording {
    renderer.reset(bounds());
    assert!(renderer.start_recording(bounds()));
    let _paragraph = paint(renderer, font);
    renderer
        .finish_recording()
        .expect("owned captured text and font assets")
}

fn pixels(renderer: &mut Renderer) -> Vec<u8> {
    <Renderer as Headless>::screenshot(renderer, SIZE, 1.0, Color::BLACK)
}

#[test]
fn resource_reclamation_ui_capture_cache_is_non_owning() {
    let engine = engine();
    let mut ui = Renderer::new(engine, core::renderer::Settings::default());
    let recording = record(&mut ui, Font::new("DejaVu Sans"));
    let scene = recording
        .payload::<crate::Scene>()
        .expect("wgpu recording should contain a scene");
    let fonts = scene
        .fonts
        .as_ref()
        .expect("text recording should retain font assets");

    assert_eq!(Arc::strong_count(fonts), 1);
}

#[test]
fn resource_reclamation_renderer_retirement_clears_owned_font_clone() {
    let engine = engine();
    let mut ui = Renderer::new(engine.clone(), core::renderer::Settings::default());
    let mut lane = Renderer::new_presentation(engine, core::renderer::Settings::default());
    let recording = record(&mut ui, Font::new("DejaVu Sans"));

    lane.reset(bounds());
    assert!(lane.replay_tracks(&recording, &[]));
    #[cfg(feature = "image")]
    let _allocation = {
        lane.set_image_cache_policy(crate::ImageCachePolicy::new(7, 1_234));
        let device = lane.engine.device.clone();
        let queue = lane.engine.queue.clone();
        let handle = core::image::Handle::from_rgba(1, 1, vec![255; 4]);
        lane.image_cache
            .get_mut()
            .load_image(&device, &queue, &handle)
            .expect("one-pixel media should upload")
    };
    let retained = lane.retained_resource_statistics();
    assert!(retained.font_resources >= 1);
    assert!(retained.font_bytes >= 1);
    #[cfg(feature = "image")]
    {
        assert!(retained.cached_media_entries >= 1);
        assert!(retained.cached_media_bytes >= 4);
        assert!(retained.atlas_allocations >= 1);
        assert!(retained.atlas_bytes >= 4);
    }

    drop(recording);
    lane.retire_presentation_resources();
    let retired = lane.retained_resource_statistics();
    assert_eq!(retired.font_resources, 0);
    assert_eq!(retired.font_bytes, 0);
    assert_eq!(retired.media_resources, 0);
    assert_eq!(retired.media_bytes, 0);
    assert_eq!(retired.cached_media_entries, 0);
    assert_eq!(retired.cached_media_bytes, 0);
    assert_eq!(retired.atlas_allocations, 0);
    assert_eq!(retired.atlas_bytes, 0);
    assert_eq!(retired.pending_media_jobs, 0);
    assert_eq!(retired.pending_callbacks, 0);
    assert_eq!(retired.pending_callback_bytes, 0);
    #[cfg(feature = "image")]
    {
        let cache = lane.image_cache_statistics();
        assert_eq!(cache.grace_frames, 7);
        assert_eq!(cache.stale_byte_budget, 1_234);
    }
}

#[test]
fn recorded_text_paints_while_ui_font_and_atlas_locks_are_held() {
    let engine = engine();
    let mut ui = Renderer::new(engine.clone(), core::renderer::Settings::default());
    let mut lane = Renderer::new_presentation(engine.clone(), core::renderer::Settings::default());
    let recording = record(&mut ui, Font::new("DejaVu Sans"));
    let expected = pixels(&mut ui);
    let ink = expected
        .chunks_exact(4)
        .filter(|pixel| pixel[0] != 0)
        .count();
    assert!(
        ink > 200 && ink < (SIZE.width * SIZE.height / 2) as usize,
        "ink={ink}"
    );
    ui.reset(bounds());
    let _paragraph = paint(&mut ui, Font::new("DejaVu Sans"));
    assert!(pixels(&mut ui) == expected, "same-input UI pixels changed");

    let font_lock = font_system().write().unwrap();
    let atlas_lock = engine.text_pipeline.atlas.write().unwrap();
    let (completed, result) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        lane.reset(bounds());
        assert!(lane.replay_tracks(&recording, &[]));
        let image = pixels(&mut lane);
        drop(lane);
        let _ = completed.send(image);
    });
    let image = result.recv_timeout(Duration::from_secs(5));
    drop(atlas_lock);
    drop(font_lock);
    drop(worker);
    assert!(
        image.expect("renderer waited on a UI-owned resource") == expected,
        "independent text pixels differ from direct UI drawing"
    );
}

#[test]
fn changed_font_assets_replace_one_replay_revision_without_retaining_history() {
    let engine = engine();
    let mut ui = Renderer::new(engine.clone(), core::renderer::Settings::default());
    let mut lane = Renderer::new_presentation(engine, core::renderer::Settings::default());
    let first = record(&mut ui, Font::new("DejaVu Sans"));
    let repeated = record(&mut ui, Font::new("DejaVu Sans"));
    let assets = first
        .payload::<crate::Scene>()
        .unwrap()
        .fonts
        .as_ref()
        .unwrap();
    assert!(Arc::ptr_eq(
        assets,
        repeated
            .payload::<crate::Scene>()
            .unwrap()
            .fonts
            .as_ref()
            .unwrap()
    ));
    let retired = Arc::downgrade(assets);
    lane.reset(bounds());
    assert!(lane.replay_tracks(&first, &[]));
    drop(first);
    drop(repeated);
    let replacement = record(
        &mut ui,
        Font {
            weight: core::font::Weight::Bold,
            ..Font::new("DejaVu Sans")
        },
    );
    let prior = retired
        .upgrade()
        .expect("the active replay still owns its fonts");
    assert_ne!(
        prior.glyph_fonts,
        replacement
            .payload::<crate::Scene>()
            .unwrap()
            .fonts
            .as_ref()
            .unwrap()
            .glyph_fonts,
        "the fixture must select different raster font resources"
    );
    drop(prior);
    lane.reset(bounds());
    assert!(lane.replay_tracks(&replacement, &[]));
    assert!(retired.upgrade().is_none());
}

#[test]
fn paragraph_reflows_and_paints_without_ui_resources_or_mutating_the_recording() {
    use core::text::{Paragraph as _, Renderer as _};
    let engine = engine();
    let mut ui = Renderer::new(engine.clone(), core::renderer::Settings::default());
    let mut lane = Renderer::new_presentation(engine.clone(), core::renderer::Settings::default());
    let definition = core::text::Text {
        content: "Real layout wraps words without scaling glyphs; Arabic العربية.",
        bounds: Size::new(280.0, 100.0),
        size: 14.0.into(),
        line_height: core::text::LineHeight::Relative(1.2),
        font: Font::default(),
        align_x: core::text::Alignment::Left,
        align_y: core::alignment::Vertical::Top,
        shaping: core::text::Shaping::Advanced,
        hint_factor: Some(1.0),
        wrapping: core::text::Wrapping::Word,
        ellipsis: core::text::Ellipsis::None,
    };
    let source = crate::graphics::text::Paragraph::with_text(definition);
    let original_bounds = source.min_bounds();
    ui.reset(bounds());
    assert!(ui.start_recording(bounds()));
    ui.fill_paragraph(
        &source,
        core::Point::new(10.0, 10.0),
        Color::WHITE,
        bounds(),
    );
    let recording = ui.finish_recording().expect("paragraph and font snapshot");
    let widths = [120.0, 280.0, 160.0];
    let mut expected = Vec::new();
    let mut heights = Vec::new();
    for width in widths {
        let paragraph = crate::graphics::text::Paragraph::with_text(core::text::Text {
            bounds: Size::new(width, 100.0),
            ..definition
        });
        heights.push(paragraph.min_bounds().height);
        ui.reset(bounds());
        ui.fill_paragraph(
            &paragraph,
            core::Point::new(10.0, 10.0),
            Color::WHITE,
            bounds(),
        );
        let image = pixels(&mut ui);
        assert!(image.chunks_exact(4).filter(|pixel| pixel[0] != 0).count() > 100);
        expected.push(image);
    }
    assert!(heights[0] > heights[1]);
    assert!(
        expected[0] != expected[1],
        "width must change the painted text"
    );
    let font_lock = font_system().write().unwrap();
    let atlas_lock = engine.text_pipeline.atlas.write().unwrap();
    let mut paragraph = source.clone();
    let (completed, result) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        lane.reset(bounds());
        assert!(lane.replay_tracks(&recording, &[]));
        let mut actual = Vec::new();
        for width in widths {
            let fonts = &mut lane.text.raster_fonts.as_mut().expect("owned fonts").system;
            paragraph.resize_owned(fonts, Size::new(width, 100.0));
            lane.reset(bounds());
            lane.fill_paragraph(
                &paragraph,
                core::Point::new(10.0, 10.0),
                Color::WHITE,
                bounds(),
            );
            actual.push(pixels(&mut lane));
        }
        drop(lane);
        let _ = completed.send(actual);
    });
    let actual = result.recv_timeout(Duration::from_secs(5));
    drop(atlas_lock);
    drop(font_lock);
    drop(worker);
    assert!(
        actual.expect("owned reflow waited on the UI") == expected,
        "owned reflow pixels differ from fresh UI paragraphs"
    );
    assert_eq!(source.min_bounds(), original_bounds);
}

#[test]
fn presentation_program_reflows_text_and_following_paint_during_ui_resource_contention() {
    use core::renderer::presentation::{Axis, Bounds, Expression, Layout, Node, Program};
    use core::renderer::{NodeId, Track};
    use core::text::{Paragraph as _, Renderer as _};
    let engine = engine();
    let mut ui = Renderer::new(engine.clone(), core::renderer::Settings::default());
    let mut lane = Renderer::new_presentation(engine.clone(), core::renderer::Settings::default());
    let definition = core::text::Text {
        content: "Real layout wraps words without scaling glyphs; Arabic العربية.",
        bounds: Size::new(280.0, 100.0),
        size: 14.0.into(),
        line_height: core::text::LineHeight::Relative(1.2),
        font: Font::default(),
        align_x: core::text::Alignment::Left,
        align_y: core::alignment::Vertical::Top,
        shaping: core::text::Shaping::Advanced,
        hint_factor: Some(1.0),
        wrapping: core::text::Wrapping::Word,
        ellipsis: core::text::Ellipsis::None,
    };
    let source = crate::graphics::text::Paragraph::with_text(definition);
    let original_height = source.min_bounds().height;
    let program = Arc::new(
        Program::new(
            vec![
                Expression::Constant(10.0),
                Expression::Driver {
                    node: 900,
                    channel: 0,
                },
                Expression::Constant(5.0),
            ],
            vec![
                Node {
                    id: 1,
                    layout: Layout::Text {
                        recorded: Bounds {
                            x: 10.0,
                            y: 10.0,
                            width: 280.0,
                            height: original_height,
                        },
                        width: None,
                        height: None,
                        shrink_width: false,
                        max_height: None,
                        overflow: crate::core::renderer::presentation::TextOverflow::Refuse,
                    },
                    children: vec![],
                },
                Node {
                    id: 2,
                    layout: Layout::Fixed {
                        recorded: Bounds {
                            x: 10.0,
                            y: 15.0 + original_height,
                            width: 80.0,
                            height: 10.0,
                        },
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
            Bounds {
                x: 0.0,
                y: 0.0,
                width: 320.0,
                height: 120.0,
            },
        )
        .unwrap(),
    );
    fn following(renderer: &mut Renderer, y: f32) {
        renderer.fill_quad(
            core::renderer::Quad {
                bounds: Rectangle {
                    x: 10.0,
                    y,
                    width: 80.0,
                    height: 10.0,
                },
                ..Default::default()
            },
            Color::from_rgb(0.0, 0.0, 1.0),
        );
    }
    // Paint outside every program node is retained only as declared fixed
    // layout paint; a node the program does not own declares exactly that.
    fn fixed(renderer: &mut Renderer) {
        renderer.fill_layout_quad(
            NodeId(99),
            core::renderer::Quad {
                bounds: Rectangle {
                    x: 220.0,
                    y: 105.0,
                    width: 80.0,
                    height: 10.0,
                },
                ..Default::default()
            },
            Color::WHITE.into(),
        );
    }
    ui.reset(bounds());
    assert!(ui.start_recording(bounds()));
    ui.record_presentation(program).unwrap();
    ui.mark_recording(NodeId(1));
    ui.fill_paragraph(
        &source,
        core::Point::new(10.0, 10.0),
        Color::WHITE,
        bounds(),
    );
    ui.mark_recording(NodeId::STATIC);
    ui.mark_recording(NodeId(2));
    following(&mut ui, 15.0 + original_height);
    ui.mark_recording(NodeId::STATIC);
    fixed(&mut ui);
    let recording = ui.finish_recording().expect("text program recording");
    let original = pixels(&mut ui);
    let widths = [280.0, 120.0, 120.0, 160.0];
    let mut expected = Vec::new();
    for width in widths {
        let paragraph = crate::graphics::text::Paragraph::with_text(core::text::Text {
            bounds: Size::new(width, 100.0),
            ..definition
        });
        ui.reset(bounds());
        ui.fill_paragraph(
            &paragraph,
            core::Point::new(10.0, 10.0),
            Color::WHITE,
            bounds(),
        );
        following(&mut ui, 15.0 + paragraph.min_bounds().height);
        fixed(&mut ui);
        expected.push(pixels(&mut ui));
    }
    assert!(
        expected[0] == original,
        "same-input recording must match direct paint"
    );
    assert!(
        expected[0] != expected[1],
        "text and following paint must move"
    );
    let font_lock = font_system().write().unwrap();
    let atlas_lock = engine.text_pipeline.atlas.write().unwrap();
    let (completed, result) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut actual = Vec::new();
        let mut work = Vec::new();
        for width in widths {
            lane.reset(bounds());
            assert!(
                lane.replay_tracks(
                    &recording,
                    &[Track {
                        translation: core::Vector::new(width, 0.0),
                        ..Track::identity(NodeId(900))
                    }]
                ),
                "replay refusal: {:?}",
                lane.presentation_refusal()
            );
            actual.push(pixels(&mut lane));
            work.push(lane.presentation_work());
        }
        drop(lane);
        let _ = completed.send((actual, work));
    });
    let actual = result.recv_timeout(Duration::from_secs(5));
    drop(atlas_lock);
    drop(font_lock);
    drop(worker);
    let (images, work) = actual.expect("program replay waited on UI text resources");
    assert!(
        images == expected,
        "program replay differs from direct text/sibling paint"
    );
    assert_eq!(work[0].text_measurements, 1);
    assert_eq!(work[1].text_measurements, 1);
    assert_eq!(work[2].text_measurements, 0);
    assert_eq!(work[2].layout_nodes, 0);
    assert_eq!(source.min_bounds().height, original_height);
}

#[test]
fn builtin_text_layout_and_draw_supply_default_presentation_metadata() {
    use core::layout::{Layout, Limits};
    use core::renderer::{NodeId, Track};
    let engine = engine();
    let mut ui = Renderer::new(engine.clone(), core::renderer::Settings::default());
    let mut lane = Renderer::new_presentation(engine.clone(), core::renderer::Settings::default());
    let mut element: core::Element<'_, (), core::Theme, Renderer> =
        core::widget::Text::new("Real layout wraps words without scaling glyphs; Arabic العربية.")
            .size(14)
            .width(core::Length::Fill)
            .color(Color::WHITE)
            .into();
    let mut tree = core::widget::Tree::new(&element);
    let theme = core::Theme::Dark;
    let style = core::renderer::Style::default();
    let node = element
        .as_widget_mut()
        .measure_snapshot(
            &mut tree,
            &ui,
            &Limits::new(Size::ZERO, Size::new(280.0, 100.0)),
        )
        .move_to(core::Point::new(10.0, 10.0));
    let program = Layout::of(&node)
        .presentation_program(&tree, NodeId(900), 0, bounds())
        .unwrap();
    let leaf = program.leaves().next().unwrap();
    assert_ne!(leaf & (1 << 63), 0);
    ui.reset(bounds());
    assert!(ui.start_recording(bounds()));
    ui.record_presentation(program).unwrap();
    element.as_widget().draw(
        &tree,
        &mut ui,
        &theme,
        &style,
        Layout::of(&node),
        core::mouse::Cursor::Unavailable,
        &bounds(),
    );
    let recording = ui
        .finish_recording()
        .expect("ordinary Text supplies its paint identity");
    let initial = pixels(&mut ui);
    let widths = [280.0, 120.0, 160.0];
    let mut expected = Vec::new();
    for width in widths {
        let node = element
            .as_widget_mut()
            .measure_snapshot(
                &mut tree,
                &ui,
                &Limits::new(Size::ZERO, Size::new(width, 100.0)),
            )
            .move_to(core::Point::new(10.0, 10.0));
        let same_state = Layout::of(&node)
            .presentation_program(&tree, NodeId(900), 0, bounds())
            .unwrap();
        assert_eq!(same_state.leaves().next(), Some(leaf));
        ui.reset(bounds());
        element.as_widget().draw(
            &tree,
            &mut ui,
            &theme,
            &style,
            Layout::of(&node),
            core::mouse::Cursor::Unavailable,
            &bounds(),
        );
        expected.push(pixels(&mut ui));
    }
    assert!(
        initial == expected[0],
        "same-input ordinary Text pixels changed"
    );
    assert!(
        expected[0] != expected[1],
        "ordinary Text must visibly reflow"
    );
    let font_lock = font_system().write().unwrap();
    let atlas_lock = engine.text_pipeline.atlas.write().unwrap();
    let (completed, result) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut images = Vec::new();
        for width in widths {
            lane.reset(bounds());
            assert!(
                lane.replay_tracks(
                    &recording,
                    &[Track {
                        translation: core::Vector::new(width, 0.0),
                        ..Track::identity(NodeId(900))
                    }]
                ),
                "replay refusal: {:?}",
                lane.presentation_refusal()
            );
            images.push(pixels(&mut lane));
        }
        drop(lane);
        let _ = completed.send(images);
    });
    let images = result.recv_timeout(Duration::from_secs(5));
    drop(atlas_lock);
    drop(font_lock);
    drop(worker);
    assert!(
        images.expect("default Text replay waited on UI resources") == expected,
        "default Text replay differs from ordinary widget layout/draw"
    );
}

#[test]
fn builtin_text_replay_preserves_finite_height_limits_and_clipped_long_words() {
    use core::layout::{Layout, Limits};
    use core::renderer::{NodeId, Track};
    let engine = engine();
    let mut ui = Renderer::new(engine.clone(), core::renderer::Settings::default());
    for content in [
        "W".repeat(100),
        "wrap words across several visual rows ".repeat(20),
    ] {
        let mut lane =
            Renderer::new_presentation(engine.clone(), core::renderer::Settings::default());
        let mut element: core::Element<'_, (), core::Theme, Renderer> =
            core::widget::Text::new(content)
                .size(14)
                .width(core::Length::Fill)
                .color(Color::WHITE)
                .into();
        let mut tree = core::widget::Tree::new(&element);
        let theme = core::Theme::Dark;
        let style = core::renderer::Style::default();
        let node = element
            .as_widget_mut()
            .measure_snapshot(
                &mut tree,
                &ui,
                &Limits::new(Size::ZERO, Size::new(280.0, 32.0)),
            )
            .move_to(core::Point::new(10.0, 10.0));
        let program = Layout::of(&node)
            .presentation_program(&tree, NodeId(900), 0, bounds())
            .unwrap();
        ui.reset(bounds());
        assert!(ui.start_recording(bounds()));
        ui.record_presentation(program).unwrap();
        element.as_widget().draw(
            &tree,
            &mut ui,
            &theme,
            &style,
            Layout::of(&node),
            core::mouse::Cursor::Unavailable,
            &bounds(),
        );
        let recording = ui
            .finish_recording()
            .expect("ordinary constrained Text recording");
        let initial = pixels(&mut ui);
        assert!(
            initial
                .chunks_exact(4)
                .filter(|pixel| pixel[0] != 0)
                .count()
                > 100
        );
        let widths = [280.0, 120.0, 160.0];
        let mut expected = Vec::new();
        for width in widths {
            let node = element
                .as_widget_mut()
                .measure_snapshot(
                    &mut tree,
                    &ui,
                    &Limits::new(Size::ZERO, Size::new(width, 32.0)),
                )
                .move_to(core::Point::new(10.0, 10.0));
            assert!(node.size().height <= 32.0);
            ui.reset(bounds());
            element.as_widget().draw(
                &tree,
                &mut ui,
                &theme,
                &style,
                Layout::of(&node),
                core::mouse::Cursor::Unavailable,
                &bounds(),
            );
            expected.push(pixels(&mut ui));
        }
        assert!(initial == expected[0], "same-input bounded Text changed");
        let font_lock = font_system().write().unwrap();
        let atlas_lock = engine.text_pipeline.atlas.write().unwrap();
        let (completed, result) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut images = Vec::new();
            for width in widths {
                lane.reset(bounds());
                assert!(
                    lane.replay_tracks(
                        &recording,
                        &[Track {
                            translation: core::Vector::new(width, 0.0),
                            ..Track::identity(NodeId(900))
                        }]
                    ),
                    "bounded text refused: {:?}",
                    lane.presentation_refusal()
                );
                images.push(pixels(&mut lane));
            }
            drop(lane);
            let _ = completed.send(images);
        });
        let images = result.recv_timeout(Duration::from_secs(5));
        drop(atlas_lock);
        drop(font_lock);
        drop(worker);
        assert!(
            images.expect("bounded Text replay did not finish") == expected,
            "bounded Text replay differs from ordinary layout/draw"
        );
    }
}

fn zero_width_text_verify(padded: bool) {
    use core::renderer::{NodeId, Track};
    use core::{Element, Layout, Length};
    let engine = engine();
    let mut ui = Renderer::new(engine.clone(), core::renderer::Settings::default());
    let mut lane = Renderer::new_presentation(engine, core::renderer::Settings::default());
    let mut element: Element<'_, (), core::Theme, Renderer> = core::widget::Text::new("Documents")
        .font(Font::new("DejaVu Sans"))
        .size(14)
        .color(Color::WHITE)
        .width(if padded { Length::Shrink } else { Length::Fill })
        .into();
    let mut tree = core::widget::Tree::new(&element);
    let layout = |element: &mut Element<'_, (), core::Theme, Renderer>,
                  tree: &mut core::widget::Tree,
                  ui: &Renderer,
                  width| {
        let limits = core::layout::Limits::new(Size::ZERO, Size::new(width, 100.0));
        let node = if padded {
            core::layout::padded_node(
                &limits,
                Length::Fill,
                Length::Shrink,
                core::Padding {
                    left: 64.0,
                    ..core::Padding::ZERO
                },
                |limits| element.as_widget_mut().measure_snapshot(tree, ui, limits),
            )
        } else {
            element.as_widget_mut().measure_snapshot(tree, ui, &limits)
        };
        node.move_to(core::Point::new(10.0, 10.0))
    };
    let paint = |element: &Element<'_, (), core::Theme, Renderer>,
                 tree: &core::widget::Tree,
                 ui: &mut Renderer,
                 node: &core::layout::Node| {
        let outer = Layout::of(node);
        let child = if padded {
            let content = &node.children()[0];
            Layout::new(content.size()).move_to(
                outer.position() + core::Vector::new(content.position().x, content.position().y),
            )
        } else {
            outer
        };
        element.as_widget().draw(
            tree,
            ui,
            &core::Theme::Dark,
            &core::renderer::Style::default(),
            child,
            core::mouse::Cursor::Unavailable,
            &bounds(),
        );
        child.bounds()
    };
    let node = layout(&mut element, &mut tree, &ui, 200.0);
    let program = core::layout::Placed::of(&node)
        .presentation_program(NodeId(900), 0, bounds())
        .unwrap();
    let leaf = program.text_nodes().next().unwrap().0;
    ui.reset(bounds());
    assert!(ui.start_recording(bounds()));
    ui.record_presentation(program).unwrap();
    let _ = paint(&element, &tree, &mut ui, &node);
    let recording = ui.finish_recording().unwrap();
    let initial = pixels(&mut ui);
    assert!(
        initial
            .chunks_exact(4)
            .filter(|pixel| pixel[0] > 200)
            .count()
            > 40
    );
    let widths = if padded {
        [200.0, 64.0, 32.0, 16.0, 32.0, 200.0]
    } else {
        [200.0, 32.0, 0.0, 0.0, 32.0, 200.0]
    };
    let mut expected = Vec::new();
    let mut allocations = Vec::new();
    for width in widths {
        let node = layout(&mut element, &mut tree, &ui, width);
        ui.reset(bounds());
        allocations.push(paint(&element, &tree, &mut ui, &node));
        expected.push(pixels(&mut ui));
    }
    assert!(initial == expected[0] && initial == expected[5]);
    // Native `Text` paints nothing once its allocation and paragraph bounds
    // collapse to zero width, whether it is the root or a padded child.
    assert!(initial != expected[2]);
    assert!(
        expected[2].chunks_exact(4).all(|pixel| pixel[0] <= 200),
        "a zero-width native allocation paints no ink"
    );
    assert!(allocations[0].width > 0.0);
    assert_eq!(allocations[2].width, 0.0);
    drop(element);
    drop(tree);
    drop(ui);
    let fonts = crate::graphics::text::font_system().write().unwrap();
    let (send, receive) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut actual = Vec::new();
        for width in widths {
            lane.reset(bounds());
            assert!(
                lane.replay_tracks(
                    &recording,
                    &[Track {
                        translation: core::Vector::new(width, 0.0),
                        ..Track::identity(NodeId(900))
                    }]
                ),
                "zero-width replay at {width}: {:?}",
                lane.presentation_refusal()
            );
            let sample = lane.layout_sample().unwrap().unwrap();
            let allocation = sample.allocation(leaf).unwrap().0;
            actual.push((pixels(&mut lane), allocation));
        }
        let _ = send.send(actual);
    });
    let actual = receive.recv_timeout(Duration::from_secs(10));
    drop(fonts);
    if actual.is_ok() {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !worker.is_finished() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            worker.is_finished(),
            "owned zero-width worker destruction exceeded its deadline"
        );
        worker.join().unwrap();
    } else {
        drop(worker);
    }
    let actual = actual.expect("owned zero-width text must not consult UI fonts");
    for (index, ((pixels, allocation), expected)) in actual.into_iter().zip(expected).enumerate() {
        assert_eq!(allocation.width, allocations[index].width);
        assert_eq!(allocation.height, allocations[index].height);
        let differences = pixels
            .iter()
            .zip(expected)
            .filter(|(a, b)| **a != *b)
            .count();
        assert_eq!(
            differences, 0,
            "zero-width native pixel mismatch at {index}"
        );
    }
}

#[test]
fn zero_width_text_root_matches_native_pixels_through_collapse_and_restore() {
    zero_width_text_verify(false);
}

#[test]
fn zero_width_text_padded_child_matches_native_pixels_through_collapse_and_restore() {
    zero_width_text_verify(true);
}

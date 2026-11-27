// Modified by enzUI contributors. See FORK.md.
//! An editor painted again only where its frames differ shows, on every
//! frame, exactly what a full paint shows.

use crate::core::renderer::{self, Renderer as _};
use crate::core::text::editor::{Action, Edit, Editor as _, Motion, Selection};
use crate::core::text::{self, LineHeight, Paragraph as _, Wrapping};
use crate::core::{Background, Color, Font, Pixels, Point, Rectangle, Size};
use crate::graphics::text::{Editor, Paragraph};
use crate::graphics::{Antialiasing, Shell, Viewport};
use crate::{Engine, Renderer};

const WINDOW: Size = Size::new(420.0, 300.0);
const EDITOR: Rectangle = Rectangle {
    x: 16.0,
    y: 12.0,
    width: 380.0,
    height: 270.0,
};
const TEXT_SIZE: f32 = 16.0;
const BACKGROUND: Color = Color::from_rgb(0.08, 0.08, 0.1);

/// Lines in several scripts: fallback faces for CJK, emoji and Arabic, and
/// combining marks stacked far above and below their line.
const DOCUMENT: &str = "Plain ASCII line, with descenders: gjpqy\n\
    Ångström façade naïve Ǻ Ḯ ẞ\n\
    漢字かな交じり文 한국어 中文\n\
    Emoji 🙂👍🏽🎉🧑‍🚀 inline\n\
    Under the marks\n\
    Z\u{30c}\u{30c}\u{30c}\u{30c}\u{30c}\u{30c}\u{30c}\u{30c}\u{30c}\u{30c}\u{30c}\u{30c}\u{30c}\u{30c}\u{30c}\u{30c}a\u{323}\u{323}\u{323}\u{323}\u{323}\u{323}\u{323}\u{323}\u{323}\u{323}\u{323}\u{323}\u{323}\u{323}\u{323}\u{323}lgo stacked marks\n\
    مرحبا بالعالم RTL mixed\n\
    हिन्दी देवनागरी\n\
    A long line that wraps once the editor wraps words, so a change in wrapping moves every line after it.\n\
    tail line one\n\
    tail line two\n\
    tail line three\n\
    tail line four\n\
    tail line five";

struct Scene {
    editor: Editor,
    wrapping: Wrapping,
    width: f32,
    caret: bool,
    preedit: Option<Paragraph>,
}

impl Scene {
    fn new() -> Self {
        Self {
            editor: Editor::with_text(DOCUMENT),
            wrapping: Wrapping::None,
            width: EDITOR.width,
            caret: true,
            preedit: None,
        }
    }

    fn perform(&mut self, action: Action) {
        let _ = self.editor.perform(action);
    }

    fn update(&mut self) {
        self.editor.update(
            Size::new(self.width, EDITOR.height),
            Font::DEFAULT,
            Pixels(TEXT_SIZE),
            LineHeight::default(),
            self.wrapping,
            text::Alignment::Default,
            None,
            &mut text::parser::PlainText,
        );
    }

    /// Paints as the text editor widget does: selection under the text, the
    /// caret and an input method's preedit over it.
    fn draw(&self, renderer: &mut Renderer) {
        let window = Rectangle::with_size(WINDOW);
        renderer.reset(window);
        fill(renderer, window, BACKGROUND);
        let editor = Rectangle::new(EDITOR.position(), Size::new(self.width, EDITOR.height));
        fill(renderer, editor, Color::from_rgb(0.14, 0.14, 0.17));
        let origin = editor.position();
        let line = LineHeight::default().to_absolute(Pixels(TEXT_SIZE)).0;
        let caret = match self.editor.selection() {
            Selection::Range(ranges) => {
                for range in ranges {
                    if let Some(range) = (range + (origin - Point::ORIGIN)).intersection(&editor) {
                        fill(renderer, range, Color::from_rgb(0.2, 0.35, 0.6));
                    }
                }
                None
            }
            Selection::Caret(caret) => Some(caret + (origin - Point::ORIGIN)),
        };
        crate::core::text::Renderer::fill_editor(
            renderer,
            &self.editor,
            origin,
            Color::from_rgb(0.92, 0.92, 0.9),
            editor,
        );
        if let Some(caret) = caret.filter(|_| self.caret) {
            fill(
                renderer,
                Rectangle::new(caret, Size::new(1.0, line)),
                Color::WHITE,
            );
        }
        if let (Some(preedit), Some(caret)) = (&self.preedit, caret) {
            let bounds = Rectangle::new(caret, preedit.min_bounds());
            fill(renderer, bounds, Color::from_rgb(0.25, 0.25, 0.3));
            crate::core::text::Renderer::fill_paragraph(
                renderer,
                preedit,
                caret,
                Color::from_rgb(1.0, 0.85, 0.4),
                editor,
            );
            fill(
                renderer,
                Rectangle::new(
                    Point::new(bounds.x, bounds.y + bounds.height - 1.0),
                    Size::new(bounds.width, 1.0),
                ),
                Color::from_rgb(1.0, 0.85, 0.4),
            );
        }
    }
}

fn fill(renderer: &mut Renderer, bounds: Rectangle, color: Color) {
    renderer.fill_quad(
        renderer::Quad {
            bounds,
            ..renderer::Quad::default()
        },
        Background::Color(color),
    );
}

fn preedit(content: &str) -> Paragraph {
    Paragraph::with_text(crate::core::Text {
        content,
        bounds: Size::INFINITE,
        size: Pixels(TEXT_SIZE),
        line_height: LineHeight::default(),
        font: Font::DEFAULT,
        align_x: text::Alignment::Left,
        align_y: crate::core::alignment::Vertical::Top,
        shaping: text::Shaping::Advanced,
        wrapping: Wrapping::None,
        ellipsis: text::Ellipsis::default(),
        hint_factor: None,
    })
}

fn engine() -> Engine {
    pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY),
            flags: wgpu::InstanceFlags::empty(),
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
            .expect("an adapter is required to render");
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .expect("device");
        Engine::new(
            &adapter,
            device,
            queue,
            wgpu::TextureFormat::Rgba8Unorm,
            Some(Antialiasing::MSAAx4),
            Shell::headless(),
        )
    })
}

fn target(renderer: &Renderer, size: Size<u32>) -> wgpu::Texture {
    renderer
        .engine
        .device
        .create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: size.width,
                height: size.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
}

fn read(renderer: &Renderer, texture: &wgpu::Texture) -> Vec<u8> {
    super::read_pixels(&renderer.engine, texture)
}

/// How many pixels differ at all, and the box around them.
fn differing(width: u32, a: &[u8], b: &[u8]) -> (usize, Option<[u32; 4]>) {
    let mut count = 0;
    let mut span: Option<[u32; 4]> = None;
    for (index, (a, b)) in a.chunks(4).zip(b.chunks(4)).enumerate() {
        if a != b {
            count += 1;
            let (x, y) = (index as u32 % width, index as u32 / width);
            span = Some(span.map_or([x, y, x, y], |[l, t, r, b]| {
                [l.min(x), t.min(y), r.max(x), b.max(y)]
            }));
        }
    }
    (count, span)
}

type Step = (&'static str, fn(&mut Scene));

/// Steps that each change the editor as a person or an input method does.
fn steps() -> Vec<Step> {
    vec![
        ("first frame", |_| {}),
        ("caret hidden", |scene| {
            scene.caret = false;
        }),
        ("caret shown", |scene| {
            scene.caret = true;
        }),
        ("caret to line end", |scene| {
            scene.perform(Action::Move(Motion::End));
        }),
        ("type a letter", |scene| {
            scene.perform(Action::Edit(Edit::Insert('x')));
        }),
        ("type a word", |scene| {
            for c in " typed".chars() {
                scene.perform(Action::Edit(Edit::Insert(c)));
            }
        }),
        ("type emoji", |scene| {
            scene.perform(Action::Edit(Edit::Insert('🎉')));
        }),
        ("type CJK", |scene| {
            scene.perform(Action::Edit(Edit::Insert('字')));
        }),
        ("backspace", |scene| {
            scene.perform(Action::Edit(Edit::Backspace));
        }),
        ("delete back over emoji", |scene| {
            scene.perform(Action::Move(Motion::Left));
            scene.perform(Action::Edit(Edit::Delete));
        }),
        ("break the line", |scene| {
            scene.perform(Action::Edit(Edit::Enter));
        }),
        ("join the lines", |scene| {
            scene.perform(Action::Edit(Edit::Backspace));
        }),
        ("caret where stacked marks reach", |scene| {
            scene.perform(Action::Move(Motion::DocumentStart));
            for _ in 0..3 {
                scene.perform(Action::Move(Motion::Down));
            }
        }),
        ("type where stacked marks reach", |scene| {
            scene.perform(Action::Edit(Edit::Insert('e')));
        }),
        ("caret under stacked marks", |scene| {
            scene.perform(Action::Move(Motion::Down));
            scene.perform(Action::Move(Motion::Home));
        }),
        ("blink under stacked marks", |scene| {
            scene.caret = false;
        }),
        ("blink back under stacked marks", |scene| {
            scene.caret = true;
        }),
        ("preedit under stacked marks", |scene| {
            scene.preedit = Some(preedit("にほんご"));
        }),
        ("preedit changes", |scene| {
            scene.preedit = Some(preedit("日本語"));
        }),
        ("preedit commits", |scene| {
            scene.preedit = None;
            for c in "日本語".chars() {
                scene.perform(Action::Edit(Edit::Insert(c)));
            }
        }),
        ("type under stacked marks", |scene| {
            scene.perform(Action::Edit(Edit::Insert('k')));
        }),
        ("type before stacked marks", |scene| {
            scene.perform(Action::Move(Motion::Down));
            scene.perform(Action::Move(Motion::Home));
            scene.perform(Action::Edit(Edit::Insert('m')));
        }),
        ("type below stacked marks", |scene| {
            scene.perform(Action::Move(Motion::Down));
            scene.perform(Action::Edit(Edit::Insert('n')));
        }),
        ("select down", |scene| {
            scene.perform(Action::Select(Motion::Down));
        }),
        ("select right", |scene| {
            scene.perform(Action::Select(Motion::Right));
        }),
        ("select the word", |scene| {
            scene.perform(Action::SelectWord);
        }),
        ("clear the selection", |scene| {
            scene.perform(Action::Move(Motion::Left));
        }),
        ("scroll down", |scene| {
            scene.perform(Action::Scroll { lines: 2 });
        }),
        ("scroll up", |scene| {
            scene.perform(Action::Scroll { lines: -1 });
        }),
        ("words wrap", |scene| {
            scene.wrapping = Wrapping::Word;
        }),
        ("narrower", |scene| {
            scene.width = EDITOR.width - 90.0;
        }),
        ("type while wrapped", |scene| {
            scene.perform(Action::Move(Motion::DocumentStart));
            scene.perform(Action::Edit(Edit::Insert('w')));
        }),
        ("no wrapping", |scene| {
            scene.wrapping = Wrapping::None;
            scene.width = EDITOR.width;
        }),
        ("undo", |scene| {
            scene.perform(Action::Edit(Edit::Undo));
        }),
    ]
}

/// Paints every step into a kept canvas, painting again only what changed,
/// and compares each frame with a full paint by a renderer whose canvas is
/// dropped before every frame. Returns how many frames were painted in part
/// and the largest share of the window one of them painted.
fn compare(scale: f32) -> (usize, f64) {
    let viewport = Viewport::with_physical_size(
        Size::new(
            (WINDOW.width * scale).round() as u32,
            (WINDOW.height * scale).round() as u32,
        ),
        renderer::Scale {
            window: scale,
            application: 1.0,
        },
    );
    let size = viewport.physical_size();
    let engine = engine();
    let mut window = Renderer::new(engine.clone(), renderer::Settings::default());
    window.track_frame_damage(true);
    let mut full = Renderer::new(engine, renderer::Settings::default());
    full.track_frame_damage(true);
    let kept = target(&window, size);
    let fresh = target(&full, size);
    let mut scene = Scene::new();
    let mut partial = 0;
    let mut largest: f64 = 0.0;

    for (name, step) in steps() {
        step(&mut scene);
        scene.update();
        let before = window.partial_frames();
        scene.draw(&mut window);
        let _ = window.present_damaged(BACKGROUND, &kept, &viewport, false);
        if window.partial_frames() > before {
            partial += 1;
            let painted: u64 = window
                .take_present_damage()
                .unwrap_or_default()
                .iter()
                .map(|[_, _, w, h]| u64::from(*w) * u64::from(*h))
                .sum();
            largest = largest.max(painted as f64 / f64::from(size.width * size.height));
        }

        full.release_frame_targets();
        scene.draw(&mut full);
        let _ = full.present_damaged(BACKGROUND, &fresh, &viewport, false);

        let (count, span) = differing(size.width, &read(&window, &kept), &read(&full, &fresh));
        assert_eq!(
            count, 0,
            "scale {scale}, {name}: {count} pixels differ within {span:?}"
        );
    }
    (partial, largest)
}

/// Sabotage: a fixed margin of one line above and below each editor line
/// leaves the stacked marks stale; text prepared only inside the painted
/// region drops the marks a neighbouring line reaches into it.
#[test]
fn an_editor_painted_in_part_matches_a_full_paint() {
    let (partial, largest) = compare(1.0);
    assert!(partial >= 20, "{partial} frames were painted in part");
    assert!(
        largest < 0.5,
        "a partial frame painted {largest:.2} of the window"
    );
}

#[test]
fn an_editor_painted_in_part_matches_a_full_paint_at_fractional_scale() {
    let (partial, largest) = compare(1.5);
    assert!(partial >= 20, "{partial} frames were painted in part");
    assert!(
        largest < 0.5,
        "a partial frame painted {largest:.2} of the window"
    );
}

/// Caret blinks and typing paint a small part of the editor.
#[test]
fn a_caret_blink_and_a_keystroke_paint_one_line() {
    let viewport = Viewport::with_physical_size(
        Size::new(WINDOW.width as u32, WINDOW.height as u32),
        renderer::Scale {
            window: 1.0,
            application: 1.0,
        },
    );
    let mut window = Renderer::new(engine(), renderer::Settings::default());
    window.track_frame_damage(true);
    let kept = target(&window, viewport.physical_size());
    let mut scene = Scene::new();
    let mut painted = |scene: &mut Scene| {
        scene.update();
        scene.draw(&mut window);
        let _ = window.present_damaged(BACKGROUND, &kept, &viewport, false);
        window
            .take_present_damage()
            .map(|regions| regions.iter().map(|[_, _, w, h]| w * h).sum::<u32>())
    };
    assert_eq!(painted(&mut scene), None, "the first frame is whole");
    scene.caret = false;
    let blink = painted(&mut scene).expect("a blink is painted in part");
    scene.perform(Action::Edit(Edit::Insert('x')));
    let keystroke = painted(&mut scene).expect("a keystroke is painted in part");
    let line = LineHeight::default().to_absolute(Pixels(TEXT_SIZE)).0;
    assert!(blink as f32 <= 4.0 * line, "a blink painted {blink} px");
    assert!(
        keystroke as f32 <= 2.5 * line * EDITOR.width,
        "a keystroke painted {keystroke} px"
    );
}

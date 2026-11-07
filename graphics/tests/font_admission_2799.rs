// Modified by enzUI contributors. See FORK.md.
//! Focused invalid-font acceptance for enzICED #2799, at the atomic admission
//! boundary `iced_graphics::text::FontSystem::load_font`.
//!
//! The process-global font system is shared, so this is one ordered test: it
//! must not race a sibling that holds a paragraph warm. It exercises the public
//! boundary directly (the runtime task and winit's scene gate are thin layers
//! above it) and asserts the two consequences the issue is about — the returned
//! result, and whether the font generation advances — plus the retained-text
//! invalidation that `Paragraph::compare` performs off that generation.
//!
//! The fixture is an Arsenal face (`google/fonts/ofl/arsenal`) renamed to a
//! private family so it cannot collide with a host system font; the adjacent
//! `font_admission_2799.OFL.txt` records its redistribution terms.

use std::borrow::Cow;

use iced_graphics::core::alignment;
use iced_graphics::core::font;
use iced_graphics::core::text::{self, Difference, LineHeight, Paragraph as _};
use iced_graphics::core::{Font, Pixels, Size};
use iced_graphics::text::cosmic_text::fontdb;
use iced_graphics::text::{Paragraph, font_system};

const FIXTURE: &[u8] = include_bytes!("font_admission_2799.ttf");
const FAMILY: &str = "Enz Row Count Arsenal";
const TRUNCATED: usize = 200;
const MARKER: &[u8] = b"MARKER-b8f1-not-a-font";
const EMPTY: &[u8] = b"";

const SIZE: Size = Size {
    width: 320.0,
    height: 120.0,
};
const TEXT: &str = "Warm paragraph";
const TEXT_SIZE: f32 = 24.0;

fn load(bytes: Cow<'static, [u8]>) -> Result<(), font::Error> {
    font_system()
        .write()
        .expect("write font system")
        .load_font(bytes)
}

fn generation() -> iced_graphics::text::Version {
    font_system().read().expect("read font system").version()
}

fn family_present(family: &str) -> bool {
    font_system()
        .read()
        .expect("read font system")
        .families()
        .any(|name| name == family)
}

fn faces_in(family: &str) -> usize {
    let mut system = font_system().write().expect("write font system");

    system
        .raw()
        .db()
        .faces()
        .filter(|face| face.families.iter().any(|(name, _)| name == family))
        .count()
}

fn selected(family: &str) -> Option<fontdb::ID> {
    let mut system = font_system().write().expect("write font system");

    system.raw().db().query(&fontdb::Query {
        families: &[fontdb::Family::Name(family)],
        ..fontdb::Query::default()
    })
}

fn spec<Content>(content: Content) -> text::Text<Content> {
    text::Text {
        content,
        bounds: SIZE,
        size: Pixels(TEXT_SIZE),
        line_height: LineHeight::Absolute(Pixels(TEXT_SIZE * 1.25)),
        font: Font::DEFAULT,
        align_x: text::Alignment::Left,
        align_y: alignment::Vertical::Top,
        shaping: text::Shaping::Advanced,
        wrapping: text::Wrapping::Word,
        ellipsis: text::Ellipsis::None,
        hint_factor: None,
    }
}

#[test]
fn invalid_bytes_report_failure_and_only_a_new_face_advances_the_generation() {
    assert!(
        !family_present(FAMILY),
        "the private family must begin unresolved"
    );

    let baseline = generation();
    let warm = Paragraph::with_text(spec(TEXT));
    assert_eq!(
        warm.compare(spec(())),
        Difference::None,
        "the paragraph is warm before any load"
    );

    // Invalid loads: each must report failure and change nothing.
    for (bytes, offered) in [
        (Cow::Borrowed(EMPTY), 0usize),
        (Cow::Owned(FIXTURE[..TRUNCATED].to_vec()), TRUNCATED),
        (Cow::Borrowed(MARKER), MARKER.len()),
    ] {
        assert_eq!(
            load(bytes),
            Err(font::Error::InvalidData { bytes: offered }),
            "invalid bytes must report failure, not a silent success"
        );
        assert_eq!(
            generation(),
            baseline,
            "a rejected load must not advance the font generation"
        );
        assert_eq!(
            warm.compare(spec(())),
            Difference::None,
            "a rejected load must leave retained text warm"
        );
        assert!(
            !family_present(FAMILY),
            "a rejected load must not register the family"
        );
    }

    // Valid load through the same borrowed pointer the duplicate reuses below.
    assert_eq!(
        load(Cow::Borrowed(FIXTURE)),
        Ok(()),
        "a real font is admitted"
    );
    let after_valid = generation();
    assert_ne!(
        after_valid, baseline,
        "a new face must advance the font generation"
    );
    assert!(family_present(FAMILY), "the admitted family is selectable");
    assert_eq!(faces_in(FAMILY), 1, "the fixture registers a single face");
    assert!(
        selected(FAMILY).is_some(),
        "the admitted family resolves to a face"
    );
    assert_eq!(
        warm.compare(spec(())),
        Difference::Shape,
        "a new face must retire paragraphs shaped before it"
    );

    // A paragraph shaped after the admission is itself warm.
    let reshaped = Paragraph::with_text(spec(TEXT));
    assert_eq!(reshaped.compare(spec(())), Difference::None);

    // Duplicate borrowed load: admitted already, so it mutates nothing.
    assert_eq!(
        load(Cow::Borrowed(FIXTURE)),
        Ok(()),
        "loading the same borrowed bytes again is not a failure"
    );
    assert_eq!(
        generation(),
        after_valid,
        "a duplicate borrowed load must not advance the generation"
    );
    assert_eq!(
        reshaped.compare(spec(())),
        Difference::None,
        "a duplicate load must leave retained text warm"
    );
}

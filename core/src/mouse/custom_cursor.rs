// Modified by enzUI contributors. See FORK.md.
use std::sync::{Arc, Mutex};

/// A cursor drawn from an image (enzICED); show it with
/// [`Interaction::Custom`](super::Interaction::Custom).
///
/// The handle is small and `Copy`, so `mouse_interaction` can return it every
/// frame. Images live for the rest of the program; making the same image
/// again returns the same handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CustomCursor(u32);

/// The image behind a [`CustomCursor`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorImage {
    /// Straight (not premultiplied) RGBA, row by row, `width * height * 4`
    /// bytes.
    pub rgba: Arc<[u8]>,
    /// The width in pixels.
    pub width: u16,
    /// The height in pixels.
    pub height: u16,
    /// The pixel that points, from the top left.
    pub hotspot: (u16, u16),
}

/// Why an image cannot be a cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorImageError {
    /// The bytes are not `width * height * 4` long.
    WrongSize {
        /// `width * height * 4`.
        expected: usize,
        /// The length given.
        actual: usize,
    },
    /// The image is empty or larger than 256 pixels on a side.
    BadDimensions,
    /// The hotspot is outside the image.
    HotspotOutside,
}

impl std::fmt::Display for CursorImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongSize { expected, actual } => {
                write!(f, "cursor image has {actual} bytes, expected {expected}")
            }
            Self::BadDimensions => f.write_str("cursor image must be 1 to 256 pixels on a side"),
            Self::HotspotOutside => f.write_str("cursor hotspot is outside the image"),
        }
    }
}

impl std::error::Error for CursorImageError {}

static IMAGES: Mutex<Vec<Arc<CursorImage>>> = Mutex::new(Vec::new());

impl CustomCursor {
    /// A cursor from straight RGBA pixels, with the hotspot at `hotspot`.
    ///
    /// # Errors
    /// The bytes do not match the size, the image is empty or larger than
    /// 256 pixels on a side, or the hotspot is outside it.
    pub fn from_rgba(
        rgba: impl Into<Vec<u8>>,
        width: u16,
        height: u16,
        hotspot: (u16, u16),
    ) -> Result<Self, CursorImageError> {
        let rgba = rgba.into();
        if width == 0 || height == 0 || width > 256 || height > 256 {
            return Err(CursorImageError::BadDimensions);
        }
        let expected = usize::from(width) * usize::from(height) * 4;
        if rgba.len() != expected {
            return Err(CursorImageError::WrongSize {
                expected,
                actual: rgba.len(),
            });
        }
        if hotspot.0 >= width || hotspot.1 >= height {
            return Err(CursorImageError::HotspotOutside);
        }
        let image = CursorImage {
            rgba: rgba.into(),
            width,
            height,
            hotspot,
        };
        let mut images = IMAGES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let index = match images.iter().position(|known| **known == image) {
            Some(index) => index,
            None => {
                images.push(Arc::new(image));
                images.len() - 1
            }
        };
        Ok(Self(
            u32::try_from(index).expect("fewer than 2^32 cursor images"),
        ))
    }

    /// The cursor's image.
    pub fn image(self) -> Arc<CursorImage> {
        let images = IMAGES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(&images[self.0 as usize])
    }

    /// A number that names this cursor for the life of the program.
    pub fn id(self) -> u32 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_image_is_the_same_cursor() {
        let dot = CustomCursor::from_rgba(vec![255; 4 * 4 * 4], 4, 4, (1, 2)).unwrap();
        let again = CustomCursor::from_rgba(vec![255; 4 * 4 * 4], 4, 4, (1, 2)).unwrap();
        let moved = CustomCursor::from_rgba(vec![255; 4 * 4 * 4], 4, 4, (2, 2)).unwrap();
        assert_eq!(dot, again);
        assert_ne!(dot, moved, "another hotspot is another cursor");
        let image = dot.image();
        assert_eq!((image.width, image.height, image.hotspot), (4, 4, (1, 2)));
        assert_eq!(moved.image().hotspot, (2, 2));
    }

    #[test]
    fn bad_images_are_refused() {
        assert_eq!(
            CustomCursor::from_rgba(vec![0; 10], 2, 2, (0, 0)),
            Err(CursorImageError::WrongSize {
                expected: 16,
                actual: 10
            })
        );
        assert_eq!(
            CustomCursor::from_rgba(vec![0; 16], 2, 2, (2, 0)),
            Err(CursorImageError::HotspotOutside)
        );
        assert_eq!(
            CustomCursor::from_rgba(Vec::new(), 0, 2, (0, 0)),
            Err(CursorImageError::BadDimensions)
        );
    }
}

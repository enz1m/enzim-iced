// Modified by enzUI contributors. See FORK.md.
//! Images display raster graphics in different formats (PNG, JPG, etc.).
//!
//! # Example
//! ```no_run
//! # mod iced { pub mod widget { pub use iced_widget::*; } }
//! # pub type State = ();
//! # pub type Element<'a, Message> = iced_widget::core::Element<'a, Message, iced_widget::Theme, iced_widget::Renderer>;
//! use iced::widget::image;
//!
//! enum Message {
//!     // ...
//! }
//!
//! fn view(state: &State) -> Element<'_, Message> {
//!     image("ferris.png").into()
//! }
//! ```
//! <img src="https://github.com/iced-rs/iced/blob/9712b319bb7a32848001b96bd84977430f14b623/examples/resources/ferris.png?raw=true" width="300">
pub mod viewer;
pub use viewer::Viewer;

use crate::core::border;
use crate::core::image;
use crate::core::layout;
use crate::core::mouse;
use crate::core::renderer;
use crate::core::widget::Tree;
use crate::core::{ContentFit, Element, Layout, Length, Rectangle, Rotation, Size, Widget};

pub use image::{FilterMethod, Handle};

/// Creates a new [`Viewer`] with the given image `Handle`.
pub fn viewer<Handle>(handle: Handle) -> Viewer<Handle> {
    Viewer::new(handle)
}

/// A frame that displays an image while keeping aspect ratio.
///
/// # Example
/// ```no_run
/// # mod iced { pub mod widget { pub use iced_widget::*; } }
/// # pub type State = ();
/// # pub type Element<'a, Message> = iced_widget::core::Element<'a, Message, iced_widget::Theme, iced_widget::Renderer>;
/// use iced::widget::image;
///
/// enum Message {
///     // ...
/// }
///
/// fn view(state: &State) -> Element<'_, Message> {
///     image("ferris.png").into()
/// }
/// ```
/// <img src="https://github.com/iced-rs/iced/blob/9712b319bb7a32848001b96bd84977430f14b623/examples/resources/ferris.png?raw=true" width="300">
pub struct Image<Handle = image::Handle> {
    handle: Handle,
    width: Length,
    height: Length,
    crop: Option<Rectangle<u32>>,
    border_radius: border::Radius,
    content_fit: ContentFit,
    filter_method: FilterMethod,
    rotation: Rotation,
    opacity: f32,
    scale: f32,
    expand: bool,
}

impl<Handle> Image<Handle> {
    /// Creates a new [`Image`] with the given path.
    pub fn new(handle: impl Into<Handle>) -> Self {
        Image {
            handle: handle.into(),
            width: Length::Shrink,
            height: Length::Shrink,
            crop: None,
            border_radius: border::Radius::default(),
            content_fit: ContentFit::default(),
            filter_method: FilterMethod::default(),
            rotation: Rotation::default(),
            opacity: 1.0,
            scale: 1.0,
            expand: false,
        }
    }

    /// Sets the width of the [`Image`] boundaries.
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    /// Sets the height of the [`Image`] boundaries.
    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.height = height.into();
        self
    }

    /// Sets whether the [`Image`] should try to fill as much space
    /// available as possible while keeping aspect ratio and without
    /// allocating extra space in any axis with a [`Length::Shrink`]
    /// sizing strategy.
    ///
    /// This is similar to using [`Length::Fill`] for both the
    /// [`width`](Self::width) and the [`height`](Self::height),
    /// but without the downside of blank space.
    pub fn expand(mut self, expand: bool) -> Self {
        self.expand = expand;
        self
    }

    /// Sets the [`ContentFit`] of the [`Image`].
    ///
    /// Defaults to [`ContentFit::Contain`]
    pub fn content_fit(mut self, content_fit: ContentFit) -> Self {
        self.content_fit = content_fit;
        self
    }

    /// Sets the [`FilterMethod`] of the [`Image`].
    pub fn filter_method(mut self, filter_method: FilterMethod) -> Self {
        self.filter_method = filter_method;
        self
    }

    /// Applies the given [`Rotation`] to the [`Image`].
    pub fn rotation(mut self, rotation: impl Into<Rotation>) -> Self {
        self.rotation = rotation.into();
        self
    }

    /// Sets the opacity of the [`Image`].
    ///
    /// It should be in the [0.0, 1.0] range—`0.0` meaning completely transparent,
    /// and `1.0` meaning completely opaque.
    pub fn opacity(mut self, opacity: impl Into<f32>) -> Self {
        self.opacity = opacity.into();
        self
    }

    /// Sets the scale of the [`Image`].
    ///
    /// The region of the [`Image`] drawn will be scaled from the center by the given scale factor.
    /// This can be useful to create certain effects and animations, like smooth zoom in / out.
    pub fn scale(mut self, scale: impl Into<f32>) -> Self {
        self.scale = scale.into();
        self
    }

    /// Crops the [`Image`] to the given region described by the [`Rectangle`] in absolute
    /// coordinates.
    ///
    /// Cropping is done before applying any transformation or [`ContentFit`]. In practice,
    /// this means that cropping an [`Image`] with this method should produce the same result
    /// as cropping it externally (e.g. with an image editor) and creating a new [`Handle`]
    /// for the cropped version.
    ///
    /// However, this method is much more efficient; since it just leverages scissoring during
    /// rendering and no image cropping actually takes place. Instead, it reuses the existing
    /// image allocations and should be as efficient as not cropping at all!
    ///
    /// The `region` coordinates will be clamped to the image dimensions, if necessary.
    pub fn crop(mut self, region: Rectangle<u32>) -> Self {
        self.crop = Some(region);
        self
    }

    /// Sets the [`border::Radius`] of the [`Image`].
    ///
    /// Currently, it will only be applied around the rectangular bounding box
    /// of the [`Image`].
    pub fn border_radius(mut self, border_radius: impl Into<border::Radius>) -> Self {
        self.border_radius = border_radius.into();
        self
    }
}

/// Computes the layout of an [`Image`].
pub fn layout<Renderer, Handle>(
    renderer: &Renderer,
    limits: &layout::Limits,
    handle: &Handle,
    width: Length,
    height: Length,
    region: Option<Rectangle<u32>>,
    content_fit: ContentFit,
    rotation: Rotation,
    expand: bool,
) -> Size
where
    Renderer: image::Renderer<Handle = Handle>,
{
    layout_node(
        renderer,
        limits,
        handle,
        width,
        height,
        region,
        content_fit,
        rotation,
        expand,
    )
    .size()
}

/// Computes the [`layout::Node`] of an [`Image`], declaring its owned media
/// presentation (enzICED).
#[allow(clippy::too_many_arguments)]
pub fn layout_node<Renderer, Handle>(
    renderer: &Renderer,
    limits: &layout::Limits,
    handle: &Handle,
    width: Length,
    height: Length,
    region: Option<Rectangle<u32>>,
    content_fit: ContentFit,
    rotation: Rotation,
    expand: bool,
) -> layout::Node
where
    Renderer: image::Renderer<Handle = Handle>,
{
    let media = layout::media::Media {
        source: renderer.measure_image(handle).unwrap_or_default(),
        sizing: Size::new(width, height),
        crop: region,
        fit: content_fit,
        rotation,
        expand,
    };
    layout::Node::new(media.allocation(limits)).with_media_presentation(
        *limits,
        media,
        renderer::presentation::MediaKind::Raster,
    )
}

fn drawing_bounds<Renderer, Handle>(
    renderer: &Renderer,
    bounds: Rectangle,
    handle: &Handle,
    region: Option<Rectangle<u32>>,
    content_fit: ContentFit,
    rotation: Rotation,
    scale: f32,
) -> Rectangle
where
    Renderer: image::Renderer<Handle = Handle>,
{
    layout::media::Media {
        source: renderer.measure_image(handle).unwrap_or_default(),
        sizing: Size::new(Length::Shrink, Length::Shrink),
        crop: region,
        fit: content_fit,
        rotation,
        expand: false,
    }
    .drawing_bounds(bounds, scale)
}

/// Draws an [`Image`]
pub fn draw<Renderer, Handle>(
    renderer: &mut Renderer,
    layout: Layout,
    tree: &Tree,
    handle: &Handle,
    crop: Option<Rectangle<u32>>,
    border_radius: border::Radius,
    content_fit: ContentFit,
    filter_method: FilterMethod,
    rotation: Rotation,
    opacity: f32,
    scale: f32,
) where
    Renderer: image::Renderer<Handle = Handle>,
    Handle: Clone,
{
    let bounds = layout.bounds();
    if (bounds.width == 0.0 || bounds.height == 0.0)
        && !tree
            .media_presentation_node()
            .is_some_and(|node| renderer.retains_layout_leaf(node))
    {
        return;
    }
    let drawing_bounds =
        drawing_bounds(renderer, bounds, handle, crop, content_fit, rotation, scale);

    renderer.draw_layout_image(
        tree.media_presentation_node(),
        image::Image {
            handle: handle.clone(),
            border_radius,
            filter_method,
            rotation: rotation.radians(),
            opacity,
        },
        drawing_bounds,
        bounds,
        scale,
    );
}

impl<Message, Theme, Renderer, Handle> Widget<Message, Theme, Renderer> for Image<Handle>
where
    Renderer: image::Renderer<Handle = Handle>,
    Handle: Clone,
{
    fn size(&self) -> Size<Length> {
        Size {
            width: self.width,
            height: self.height,
        }
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &layout::Limits) {
        layout_node(
            renderer,
            limits,
            &self.handle,
            self.width,
            self.height,
            self.crop,
            self.content_fit,
            self.rotation,
            self.expand,
        )
        .apply(tree);
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        draw(
            renderer,
            layout,
            tree,
            &self.handle,
            self.crop,
            self.border_radius,
            self.content_fit,
            self.filter_method,
            self.rotation,
            self.opacity,
            self.scale,
        );
    }
}

impl<'a, Message, Theme, Renderer, Handle> From<Image<Handle>>
    for Element<'a, Message, Theme, Renderer>
where
    Renderer: image::Renderer<Handle = Handle>,
    Handle: Clone + 'a,
{
    fn from(image: Image<Handle>) -> Element<'a, Message, Theme, Renderer> {
        Element::new(image)
    }
}

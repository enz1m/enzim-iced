// Modified by enzUI contributors. See FORK.md.
//! First-frame readiness and allocation lifetime for recorded raster artwork.

use iced_wgpu::Renderer;
use iced_wgpu::core::image::{Handle, Image, Renderer as _};
use iced_wgpu::core::renderer::Headless;
use iced_wgpu::core::{Renderer as _, Size};

#[test]
fn large_artwork_is_visible_on_the_first_replay_frame() {
    let mut renderer = pollster::block_on(<Renderer as Headless>::new(
        iced_wgpu::core::renderer::Settings::default(),
        Some("wgpu"),
    ))
    .expect("large artwork replay requires a wgpu adapter");
    let size = Size::new(100, 100);
    let bounds = iced_wgpu::core::Rectangle::with_size(Size::new(100.0, 100.0));
    let banner = Handle::from_rgba(1200, 675, vec![255; 1200 * 675 * 4]);
    let poster = Handle::from_rgba(240, 360, vec![255; 240 * 360 * 4]);
    let allocation = renderer
        .load_image(&banner)
        .expect("native banner allocation");
    assert!(renderer.start_recording(bounds));
    renderer.draw_image(
        Image::new(banner.clone()),
        iced_wgpu::core::Rectangle::with_size(Size::new(100.0, 50.0)),
        bounds,
    );
    renderer.draw_image(
        Image::new(poster),
        iced_wgpu::core::Rectangle::new(
            iced_wgpu::core::Point::new(0.0, 50.0),
            Size::new(100.0, 50.0),
        ),
        bounds,
    );
    let recording = renderer.finish_recording().expect("artwork recording");
    let screenshot = |renderer: &mut Renderer| {
        <Renderer as Headless>::screenshot(renderer, size, 1.0, iced_wgpu::core::Color::BLACK)
    };
    let pixel = |pixels: &[u8], y: usize| pixels[(y * 100 + 50) * 4];
    let native = screenshot(&mut renderer);
    assert_eq!(pixel(&native, 25), 255);
    assert_eq!(pixel(&native, 75), 255);

    let mut warmed_uploads = None;
    for _ in 0..3 {
        renderer.reset(bounds);
        assert!(renderer.replay(&recording, iced_wgpu::core::Vector::ZERO));
        let replayed = screenshot(&mut renderer);
        assert_eq!(pixel(&replayed, 75), 255, "small poster positive control");
        assert_eq!(
            pixel(&replayed, 25),
            255,
            "a frozen large banner must be ready before its first replay is painted"
        );
        assert_eq!(renderer.image_cache_statistics().pending_jobs, 0);
        let uploads = renderer.image_cache_statistics().raster_uploads;
        if let Some(expected) = warmed_uploads {
            assert_eq!(uploads, expected, "warm replay must reuse its allocations");
        }
        warmed_uploads = Some(uploads);
    }
    for _ in 0..4 {
        renderer.reset(bounds);
        let _ = screenshot(&mut renderer);
    }
    renderer.reset(bounds);
    assert!(renderer.replay(&recording, iced_wgpu::core::Vector::ZERO));
    assert_eq!(pixel(&screenshot(&mut renderer), 25), 255);

    let mut cold_renderer = pollster::block_on(<Renderer as Headless>::new(
        iced_wgpu::core::renderer::Settings::default(),
        Some("wgpu"),
    ))
    .expect("independent replay renderer");
    // A one-frame grace, so the pins' release shows as eviction.
    cold_renderer.set_image_cache_policy(iced_wgpu::ImageCachePolicy::new(1, 64 * 1024 * 1024));
    assert!(cold_renderer.replay(&recording, iced_wgpu::core::Vector::ZERO));
    let cold = screenshot(&mut cold_renderer);
    assert_eq!(
        pixel(&cold, 25),
        255,
        "a separate renderer must prepare the banner"
    );
    assert_eq!(pixel(&cold, 75), 255);
    assert_eq!(cold_renderer.image_cache_statistics().pending_jobs, 0);
    assert_eq!(cold_renderer.image_cache_statistics().raster_entries, 2);
    for _ in 0..4 {
        cold_renderer.reset(bounds);
        let _ = screenshot(&mut cold_renderer);
    }
    assert_eq!(
        cold_renderer.image_cache_statistics().raster_entries,
        0,
        "native reset must release replay allocation pins"
    );
    assert_eq!(allocation.handle().id(), banner.id());
}

#[test]
fn large_artwork_replay_bounds_combined_frozen_allocations() {
    let mut renderer = pollster::block_on(<Renderer as Headless>::new(
        iced_wgpu::core::renderer::Settings::default(),
        Some("wgpu"),
    ))
    .expect("raster allocation budget requires a wgpu adapter");
    let bounds = iced_wgpu::core::Rectangle::with_size(Size::new(16.0, 16.0));
    let record = |width, height| {
        let mut producer = pollster::block_on(<Renderer as Headless>::new(
            iced_wgpu::core::renderer::Settings::default(),
            Some("wgpu"),
        ))
        .expect("independent recording producer");
        assert!(producer.start_recording(bounds));
        producer.draw_image(
            Image::new(Handle::from_rgba(
                width,
                height,
                vec![255; width as usize * height as usize * 4],
            )),
            bounds,
            bounds,
        );
        producer
            .finish_recording()
            .expect("bounded raster recording")
    };
    let first = record(4096, 4096);
    let second = record(4096, 4096);
    let excess = record(1, 1);
    renderer.reset(bounds);
    assert!(renderer.replay(&first, iced_wgpu::core::Vector::ZERO));
    assert!(renderer.replay(&second, iced_wgpu::core::Vector::ZERO));
    assert!(renderer.replay(&first, iced_wgpu::core::Vector::ZERO));
    assert!(!renderer.replay(&excess, iced_wgpu::core::Vector::ZERO));
    let pixels = <Renderer as Headless>::screenshot(
        &mut renderer,
        Size::new(16, 16),
        1.0,
        iced_wgpu::core::Color::BLACK,
    );
    assert_eq!(&pixels[(8 * 16 + 8) * 4..][..4], &[255; 4]);
    renderer.reset(bounds);
    assert!(renderer.replay(&excess, iced_wgpu::core::Vector::ZERO));
}

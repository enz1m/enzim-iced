// Modified by enzUI contributors. See FORK.md.
//! Opt-in dump of every presented frame, for checking motion frame by frame.
//!
//! With `ICED_FRAME_DUMP=<dir>`, each frame handed to the surface is copied
//! back and written to `<dir>` as raw pixels named
//! `<sequence>_<ui|lane>_<microseconds>_<width>x<height>_<format>.raw`. The
//! microseconds count from the first dumped frame. Reading a frame back
//! stalls the GPU, so timings taken while dumping are not representative.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

fn directory() -> Option<&'static PathBuf> {
    static DIRECTORY: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIRECTORY
        .get_or_init(|| {
            let directory = PathBuf::from(std::env::var_os("ICED_FRAME_DUMP")?);
            std::fs::create_dir_all(&directory).ok()?;
            Some(directory)
        })
        .as_ref()
}

/// Whether frames are dumped; the surface then needs `COPY_SRC`.
pub(crate) fn enabled() -> bool {
    directory().is_some()
}

/// Writes `texture`, about to be presented, to the dump directory.
pub(crate) fn capture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    lane: bool,
) {
    static START: OnceLock<Instant> = OnceLock::new();
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let Some(directory) = directory() else {
        return;
    };
    if !texture.usage().contains(wgpu::TextureUsages::COPY_SRC) {
        return;
    }
    let micros = START.get_or_init(Instant::now).elapsed().as_micros();
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let size = texture.size();
    let row = size.width as usize * 4;
    let alignment = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
    let padded = row.div_ceil(alignment) * alignment;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("iced_wgpu.frame_dump"),
        size: (padded * size.height as usize) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("iced_wgpu.frame_dump"),
    });
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded as u32),
                rows_per_image: None,
            },
        },
        wgpu::Extent3d {
            depth_or_array_layers: 1,
            ..size
        },
    );
    let submission = queue.submit([encoder.finish()]);
    let slice = buffer.slice(..);
    let (send, receive) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = send.send(result);
    });
    let _ = device.poll(wgpu::PollType::Wait {
        submission_index: Some(submission),
        timeout: None,
    });
    if !matches!(receive.recv(), Ok(Ok(()))) {
        return;
    }
    let mapped = slice.get_mapped_range();
    let mut pixels = Vec::with_capacity(row * size.height as usize);
    for line in mapped.chunks(padded) {
        pixels.extend_from_slice(&line[..row]);
    }
    drop(mapped);
    buffer.unmap();
    let name = format!(
        "{sequence:06}_{}_{micros}_{}x{}_{:?}.raw",
        if lane { "lane" } else { "ui" },
        size.width,
        size.height,
        texture.format(),
    );
    let _ = std::fs::write(directory.join(name), pixels);
}

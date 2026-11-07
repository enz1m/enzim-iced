# enzUI softbuffer 0.4.8 ledger

## Upstream snapshot

The published crates.io archive, unpacked as retrieved on 2026-10-31:

- Package / version: `softbuffer` `0.4.8`
- Archive: <https://static.crates.io/crates/softbuffer/softbuffer-0.4.8.crate>
- Archive SHA-256:
  `aac18da81ebbf05109ab275b157c22a653bb3c12cf884450179942f81bcbf6c3`
- Upstream commit `d871852faa1137d4615b99b9ad31c3ba80f345b5`
  (`rust-windowing/softbuffer`), from the archive's `.cargo_vcs_info.json`
- Licence: MIT OR Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE`, unmodified)

## Change

`src/backends/wayland/buffer.rs` creates its `wl_shm` buffers as `Argb8888`
instead of `Xrgb8888` (both places a buffer is created). Upstream 0.4.8 has no
way to ask for alpha, so the compositor discarded the alpha channel and every
transparent pixel of a software-rendered window showed as black: a shell
window that clears transparent, or the rounded corners of a window that draws
its own frame.

`iced_tiny_skia` already writes premultiplied pixels in that byte order (it
swaps red and blue into the `0xAARRGGBB` word), and the GPU renderer presents
every window with premultiplied alpha, so both renderers now composite the
same. An opaque window writes alpha 255 and looks as before.

## How it reaches applications

The iced workspace depends directly on `vendor/softbuffer`. Keeping the source
inside this checkout makes the alpha fix available to every consumer without
an external sibling checkout or a root Cargo patch. The crate's original
manifest, README, changelog and licenses are retained.

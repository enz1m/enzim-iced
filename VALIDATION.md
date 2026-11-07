# Validation

Linux x86_64, Rust 1.95.0. The reviewed source is published as a GitHub fork;
this is not a claim of release testing on every platform or every retained feature.

## Build and source checks

- `cargo check --locked --workspace --all-targets --all-features` passes.
  This includes every workspace example, library, test target and benchmark.
- `cargo fmt --all -- --check` passes.
- An independent application builds with the default desktop features plus
  `a11y`, `tokio`, `advanced`, `fira-sans`, `image` and `svg`, using the root
  patch table documented in `docs/DEPENDENCIES.md`.
- All-feature Cargo metadata contains one wgpu family, cosmic-text, cryoglyph,
  winit, AccessKit family, zbus and zvariant at the intended public revisions.
  Every local package path remains inside this repository.
- 525 Rust files match the installed source after formatting and modification
  notices. Two test-only adaptations use the current software renderer in the
  rich-text gate and remove an unused layout-trait import.
- The upstream base already present in the installed fork is retained unchanged.
  Original licenses, font notices and upstream README are preserved.

## Focused regression tests

22 existing tests passed with explicit, nonempty filters:

| Area | Passed |
| --- | ---: |
| Bounded undo history | 2 |
| Input method surrounding text | 1 |
| Accessibility publication | 4 |
| Rich-text highlight pixels | 1 |
| GPU glyph atlas growth, preservation and cap | 5 |
| Glass composition pass count | 1 |
| Rounded clipping with text | 2 |
| Smooth scroll timing | 5 |
| Editor line-cache retention | 1 |

The atlas tests exercised monochrome and color glyphs, pixel preservation,
newly revealed text and exhaustion through the real headless renderer. The
GPU runs acquired an adapter; no skip messages were reported. The adapted
rich-text gate was calibrated by disabling highlight resolution in the actual
widget, observing the expected pixel assertion fail, restoring it, and passing.
The other imported tests are existing coverage, not newly calibrated here.

Representative commands (shared build directory omitted):

```sh
cargo +1.95.0 test --locked --workspace --lib --all-features history_
cargo +1.95.0 test --locked --workspace --lib --all-features surrounding_tests
cargo +1.95.0 test --locked --workspace --lib --all-features accessibility_publication
cargo +1.95.0 test --locked --workspace --all-features --test rich_text_enzui a_keyed_highlight_background_follows_the_resolver
cargo +1.95.0 test --locked --workspace --all-features --test text_atlas_growth growth_
cargo +1.95.0 test --locked --workspace --all-features --test text_atlas_growth newly_revealed
cargo +1.95.0 test --locked --workspace --all-features --test text_atlas_growth a_capped_atlas
cargo +1.95.0 test --locked --workspace --all-features --test material_composite_pass glass_composites
cargo +1.95.0 test --locked --workspace --all-features --test rounded_clip_elision a_label_
cargo +1.95.0 test --locked --workspace --lib --all-features scroll_timing_tests
cargo +1.95.0 test --locked --workspace --lib --all-features distant_navigation_keeps_a_bounded_set_of_line_caches
```

## Lint limitations

Clippy does not pass on the retained implementation with Rust 1.95.0.
Strict `-D warnings` stops on `nonminimal_bool` in
`core/src/text/editor/paint.rs`. Normal Clippy also reports that style in
`graphics/src/presentation/layout_lease.rs`, collapsible matches in
`wgpu/src/quad.rs`, and stops on the workspace-denied
`match_wildcard_for_single_variants` in `wgpu/src/owned_media.rs`.
These are reported rather than suppressed or folded into runtime changes.

Cross-platform builds, the declared minimum Rust version, display-dependent
native event tests, and a full desktop visual review were not run here.
Existing applications and the installed enzUI checkout retain their original pins.

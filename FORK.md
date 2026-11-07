# Fork provenance and scope

Upstream: https://github.com/iced-rs/iced

Upstream base: `10e9b995eb5af74dbac08ed62b01971fe7e495b2`.
This is the latest upstream tip already merged into the installed fork,
not an upstream update made during this preparation. The installed ledger
still named its older `23604ff2` branch point; using the integrated tip
preserves the later upstream commits and their original authorship.

Source snapshot: `6d6aa0e56` in the existing enzICED repository. The new
history groups the final source by subsystem, with separate signed commits
for dependency routing, core contracts, text, layout and presentation,
rendering, controls, native integration, tests and diagnostics. Each modified
file is introduced in its final form; the old development ledger and its
successive edits are not imported. Existing upstream history is unchanged.

The runtime Rust source is retained apart from formatting and modification
notices. The standalone rich-text regression test is adapted to use the
current software renderer; an unused test import is removed. Public Git
pins replace embedded cosmic-text and AccessKit copies and sibling paths
for cryoglyph and wgpu. The software buffer fix is bundled with its original
provenance and licenses. No application or installed framework pins change.

## Review areas

- Core: presentation-aware layout, motion, clipping, materials, input,
  window controls, editor and text contracts.
- Graphics: scene recording, replay, editor caches, font selection and styles.
- Wgpu: glass, alpha/rounded composition, image and glyph caches, retained
  frame damage, surface presentation and diagnostics.
- Widgets: editors and fields, scrolling, rich text, layout containers,
  controls, overlays and cached components.
- Runtime and winit: bounded dispatch, invalidation, native event routing,
  accessibility, independent presentation and guest surfaces.
- Software renderer and renderer facade: matching extended rendering contracts.

The crate names, upstream authors, MIT license and upstream documentation
remain. `README.upstream.md` is the base's README without edits. Bundled
Fira Sans faces retain their OFL notice; softbuffer retains both licenses.

Verification covers Linux. This source preview does not claim a cross-platform
release test or exhaustive validation of every retained feature. See
[VALIDATION.md](VALIDATION.md).

## Signed patch series

| # | Commit subject |
| --- | --- |
| 1 | build: Pin the public foundation forks |
| 2 | core: Add materials, motion and runtime preferences |
| 3 | core: Extend native window and clipboard controls |
| 4 | core: Extend text editing and font contracts |
| 5 | core: Extend pointer, keyboard and input method events |
| 6 | core: Add retained layout and presentation primitives |
| 7 | debug: Trace frame requests and presentation costs |
| 8 | examples: Adopt the extended widget and rendering APIs |
| 9 | iced: Expose the extended framework APIs |
| 10 | graphics: Refine shaping, font matching and bundled styles |
| 11 | graphics: Record presentation scenes and independent motion |
| 12 | graphics: Improve editor layout and bound retained caches |
| 13 | renderer: Delegate extended rendering contracts |
| 14 | runtime: Bound dispatch and track layout invalidation |
| 15 | test: Cover application rendering and event behavior |
| 16 | tiny-skia: Support rounded clips, materials and presentation |
| 17 | wgpu: Compose rounded clips and gradients correctly |
| 18 | wgpu: Integrate retained rendering and surface presentation |
| 19 | wgpu: Replay retained scenes and repaint changed regions |
| 20 | wgpu: Preserve text and image caches across partial frames |
| 21 | wgpu: Render glass materials and their backdrops |
| 22 | widget: Refine controls, overlays and cached components |
| 23 | widget: Extend layout, clipping and motion containers |
| 24 | widget: Refine smooth scrolling and scrollbar layout |
| 25 | widget: Resolve rich text themes and inline layout |
| 26 | widget: Improve text editor interaction and presentation |
| 27 | widget: Improve field editing, focus and IME behavior |
| 28 | winit: Publish accessibility trees and route actions |
| 29 | winit: Integrate native input, windows and frame scheduling |
| 30 | winit: Drive independent presentation and guest surfaces |

The final documentation commit introduces the preview README, this provenance
record, dependency guide, validation results and signing instructions once.

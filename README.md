# enzim-iced

The enzUI fork of [iced](https://github.com/iced-rs/iced), an Elm-inspired,
cross-platform GUI library for Rust. This repository carries the rendering,
input, layout and accessibility extensions used by enzUI.

The crates retain their upstream names (`iced`, `iced_core`, `iced_widget`,
and the other workspace crates). The repository name identifies this fork;
its upstream history and MIT license are preserved.

## What the fork adds

- Glass materials, rounded clipping and consistent alpha composition.
- Retained presentation scenes, independent motion and damage-based rendering.
- Text editing, font matching, rich text, smooth scrolling and input method improvements.
- Native window controls, accessibility integration and frame diagnostics.

See [FORK.md](FORK.md) for provenance and the commit organization,
[DEPENDENCIES.md](docs/DEPENDENCIES.md) for the public dependency pins,
[VALIDATION.md](VALIDATION.md) for checks and limitations, and
[upstream's README](README.upstream.md) for the original project overview.

## Build

The preparation is verified on Linux with Rust 1.95.0.

```sh
cargo +1.95.0 check --locked --workspace --all-targets --all-features
cargo +1.95.0 run --locked --example counter
```

Normal development also supports the upstream feature flags. Applications using
accessibility should enable `a11y` and `tokio` and copy the overrides from
[the dependency guide](docs/DEPENDENCIES.md) into their own workspace root.
All external fork sources are public and pinned to full Git revisions. The
software-rendering alpha fix is bundled under `vendor/softbuffer` with its licenses.

This is a Gitea review preview for a planned GitHub fork. It has not been
published on GitHub. Package repository metadata names the planned destination.

## License

The iced code is MIT licensed; see [LICENSE](LICENSE). Font license notices
are under [graphics/fonts](graphics/fonts/README.md). The bundled softbuffer
source retains its MIT OR Apache-2.0 license files.

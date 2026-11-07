# Embedded fonts

`iced_graphics` embeds these faces with `include_bytes!` (`src/text.rs`), so
they travel inside every binary that links it.

| file | embedded | licence | notice to ship |
|---|---|---|---|
| `Iced-Icons.ttf` | always | the iced repository's MIT licence (`LICENSE` at the repository root) | `LICENSE` |
| `FiraSans-Regular.ttf`, `FiraSans-Bold.ttf`, `FiraSans-Italic.ttf`, `FiraSans-BoldItalic.ttf` | with the `fira-sans` feature | SIL Open Font License 1.1 | `FiraSans-Regular.OFL.txt` |

## Iced-Icons.ttf

A seven-glyph icon font the iced authors generated with Fontello for iced's
own widgets: `down-dir` (pick list), `ok` (checkbox), `iced` (the logo,
added upstream in 1305d3c00) and private-use glyphs U+E802 to U+E805 (the
scrollable's auto-scroll icon among them, 99748b89d). Its name table reads
"Copyright (C) 2020 by original authors @ fontello.com" and carries no licence
record. iced commits it under the repository's MIT licence; upstream never
committed the Fontello configuration, so which Fontello icon sets the
individual glyphs were drawn from is not recorded anywhere in the repository.

## Fira Sans

The regular, bold, italic and bold italic faces are Fira Sans 4.203, digitized data copyright 2012-2016 The Mozilla Foundation and
Telefonica S.A., licensed under the SIL Open Font License 1.1 (name records 13
and 14). The OFL forbids releasing the face under any other licence: an
application built with `fira-sans` must ship `FiraSans-Regular.OFL.txt` (or
Fira's own `LICENSE`) with the binary, beside iced's MIT `LICENSE`.

The matching bold, italic and bold italic faces are distributed by [Google Fonts](https://github.com/google/fonts/tree/main/ofl/firasans). All four faces are covered by `FiraSans-Regular.OFL.txt`.

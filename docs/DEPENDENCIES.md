# Dependencies and consumer setup

The existing crate names and versions are retained. These are source forks,
not new crates.io releases. Use Git dependencies pinned to a full commit and
commit your application's Cargo.lock.

| Dependency | Public source | Revision |
| --- | --- | --- |
| wgpu family | https://github.com/enz1m/enzim-wgpu | `c62972c9be1326882c4b4f1939d8a2713e42d210` |
| cosmic-text | https://github.com/enz1m/enzim-cosmic-text | `ad474131e52c0fde5979f8def2889489b50a0480` |
| cryoglyph | https://github.com/enz1m/enzim-cryoglyph | `0617a49e7b8a5c5c4cc572a3e98cda70ff1be89f` |
| winit | https://github.com/enz1m/enzim-winit | `40eeb41f8637ef1620afa377ba006874224b6d65` |
| AccessKit family | https://github.com/enz1m/enzim-accesskit | `7da7ac7b800fabf0bd2ba62eef23fa9117a684f4` |
| zbus family | https://github.com/enz1m/enzim-zbus | `e182645b56d15e637bb6c8144f99ebfa7f900716` |

The source dependency on softbuffer 0.4.8 is internal to this checkout, so its
Wayland alpha fix reaches applications without a sibling checkout.

## GitHub consumer

The example pins the reviewed source revision. The default branch is `enzim`;
avoid depending on that moving branch in an application.

```toml
[dependencies]
iced = { git = "https://github.com/enz1m/enzim-iced.git", rev = "6fb8ed64462d5a855c16eb6c17e1dd0e8f678d38", features = ["a11y", "tokio"] }
```

## Workspace overrides

Cargo reads `[patch]` only from the root being built. Copy this table into your
application's root Cargo.toml (merge it with an existing table). The winit override
is necessary with `a11y`: AccessKit otherwise selects registry winit and its
`Window` type differs from the fork's. The zbus family overrides keep the
transport fix and generated types coherent across AccessKit and portal users.
This is required consumer setup, not something an iced dependency can propagate.

```toml
[patch.crates-io]
winit = { git = "https://github.com/enz1m/enzim-winit.git", rev = "40eeb41f8637ef1620afa377ba006874224b6d65" }
zbus = { git = "https://github.com/enz1m/enzim-zbus.git", rev = "e182645b56d15e637bb6c8144f99ebfa7f900716" }
zbus_macros = { git = "https://github.com/enz1m/enzim-zbus.git", rev = "e182645b56d15e637bb6c8144f99ebfa7f900716" }
zbus_names = { git = "https://github.com/enz1m/enzim-zbus.git", rev = "e182645b56d15e637bb6c8144f99ebfa7f900716" }
zvariant = { git = "https://github.com/enz1m/enzim-zbus.git", rev = "e182645b56d15e637bb6c8144f99ebfa7f900716" }
zvariant_derive = { git = "https://github.com/enz1m/enzim-zbus.git", rev = "e182645b56d15e637bb6c8144f99ebfa7f900716" }
zvariant_utils = { git = "https://github.com/enz1m/enzim-zbus.git", rev = "e182645b56d15e637bb6c8144f99ebfa7f900716" }
```

When depending directly on wgpu, cosmic-text or AccessKit types, use the same
Git URL and full revision as this workspace. A registry source or another Git
URL creates a distinct Rust crate even when the version numbers match.

Accessibility uses AccessKit's Tokio worker. The default `async-io` runtime
must not be enabled for `accesskit_unix` alongside `tokio`.

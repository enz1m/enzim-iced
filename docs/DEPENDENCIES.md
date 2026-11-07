# Dependencies and consumer setup

The existing crate names and versions are retained. These are source forks,
not new crates.io releases. Use Git dependencies pinned to a full commit and
commit your application's Cargo.lock.

| Dependency | Public source | Revision |
| --- | --- | --- |
| wgpu family | https://github.com/enz1m/enzim-wgpu | `7f040c8501f6838262c90ede4edfa40940fe2de8` |
| cosmic-text | https://github.com/enz1m/enzim-cosmic-text | `ad474131e52c0fde5979f8def2889489b50a0480` |
| cryoglyph | https://github.com/enz1m/enzim-cryoglyph | `eeaaa5585fbb1bac004ca398cfad957fec6be5f3` |
| winit | https://github.com/enz1m/enzim-winit | `f6ce16941bf815a438db7e628ab1126ce48e31dd` |
| AccessKit family | https://github.com/enz1m/enzim-accesskit | `e9da4a8da1825e65356f35c94325fb9438cf90d0` |
| zbus family | https://github.com/enz1m/enzim-zbus | `815a9ca7b64aa24e14ccb6ea3f7eda97ef7bbc8a` |

The source dependency on softbuffer 0.4.8 is internal to this checkout, so its
Wayland alpha fix reaches applications without a sibling checkout.

## Gitea preview consumer

Replace `FULL_ICED_COMMIT` with the full revision you reviewed. The preview
branch is `enzim`; avoid depending on that moving branch in an application.

```toml
[dependencies]
iced = { git = "http://192.168.2.86:3048/agent_c/enzim-iced.git", rev = "FULL_ICED_COMMIT", features = ["a11y", "tokio"] }
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
winit = { git = "https://github.com/enz1m/enzim-winit.git", rev = "f6ce16941bf815a438db7e628ab1126ce48e31dd" }
zbus = { git = "https://github.com/enz1m/enzim-zbus.git", rev = "815a9ca7b64aa24e14ccb6ea3f7eda97ef7bbc8a" }
zbus_macros = { git = "https://github.com/enz1m/enzim-zbus.git", rev = "815a9ca7b64aa24e14ccb6ea3f7eda97ef7bbc8a" }
zbus_names = { git = "https://github.com/enz1m/enzim-zbus.git", rev = "815a9ca7b64aa24e14ccb6ea3f7eda97ef7bbc8a" }
zvariant = { git = "https://github.com/enz1m/enzim-zbus.git", rev = "815a9ca7b64aa24e14ccb6ea3f7eda97ef7bbc8a" }
zvariant_derive = { git = "https://github.com/enz1m/enzim-zbus.git", rev = "815a9ca7b64aa24e14ccb6ea3f7eda97ef7bbc8a" }
zvariant_utils = { git = "https://github.com/enz1m/enzim-zbus.git", rev = "815a9ca7b64aa24e14ccb6ea3f7eda97ef7bbc8a" }
```

When depending directly on wgpu, cosmic-text or AccessKit types, use the same
Git URL and full revision as this workspace. A registry source or another Git
URL creates a distinct Rust crate even when the version numbers match.

Accessibility uses AccessKit's Tokio worker. The default `async-io` runtime
must not be enabled for `accesskit_unix` alongside `tokio`.

For the future GitHub publication only the iced Git URL changes to
`https://github.com/enz1m/enzim-iced.git`; use its reviewed published revision.
No Github iced repository has been created for this preview.

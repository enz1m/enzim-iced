// Modified by enzUI contributors. See FORK.md.
//! How the desktop decorates and arranges a window (enzICED).

use crate::window::Id;

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::{PoisonError, RwLock};

/// Which edges of a window are tiled against a neighbour or the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TiledEdges {
    /// The top edge.
    pub top: bool,
    /// The bottom edge.
    pub bottom: bool,
    /// The left edge.
    pub left: bool,
    /// The right edge.
    pub right: bool,
}

impl TiledEdges {
    /// Whether any edge is tiled.
    #[must_use]
    pub fn any(self) -> bool {
        self.top || self.bottom || self.left || self.right
    }
}

/// How the desktop decorates and arranges a window.
///
/// The default, an undecorated free-floating window, is what a window has
/// until the desktop says otherwise, and on platforms that never do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Decorations {
    /// The compositor draws the title bar and frame; the application must
    /// not draw its own window controls or resize edges.
    pub server_side: bool,
    /// Edges tiled against a neighbour or the screen: square corners there
    /// and no resize edge.
    pub tiled: TiledEdges,
    /// The window is maximized.
    pub maximized: bool,
    /// The window is fullscreen.
    pub fullscreen: bool,
}

static KNOWN: RwLock<Option<HashMap<Id, Decorations>>> = RwLock::new(None);

thread_local! {
    static VIEWING: Cell<Option<Id>> = const { Cell::new(None) };
    /// The window whose view was built last: the runtime lays a window out
    /// right after building its view, and content built during layout (a
    /// responsive closure) belongs to it.
    static LAST_VIEWED: Cell<Option<Id>> = const { Cell::new(None) };
}

/// Records what the desktop reported for window `id`; the runtime calls this.
pub fn set_decorations(id: Id, decorations: Decorations) {
    let _ = KNOWN
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .get_or_insert_with(HashMap::new)
        .insert(id, decorations);
}

/// What the desktop reported for window `id`.
#[must_use]
pub fn decorations_of(id: Id) -> Decorations {
    KNOWN
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .and_then(|known| known.get(&id).copied())
        .unwrap_or_default()
}

/// The decorations of the window whose view is being built, so widgets can
/// adapt their chrome while the application builds its view. Outside a view
/// (in content a responsive closure builds during layout), those of the
/// window whose view was built last; before any view, the default.
#[must_use]
pub fn decorations() -> Decorations {
    VIEWING
        .with(Cell::get)
        .or_else(|| LAST_VIEWED.with(Cell::get))
        .map(decorations_of)
        .unwrap_or_default()
}

/// Runs `view` as building the view of window `id`; the runtime calls this.
pub fn viewing<R>(id: Id, view: impl FnOnce() -> R) -> R {
    struct Restore(Option<Id>);
    impl Drop for Restore {
        fn drop(&mut self) {
            VIEWING.with(|viewing| viewing.set(self.0));
        }
    }
    let _restore = Restore(VIEWING.with(|viewing| viewing.replace(Some(id))));
    LAST_VIEWED.with(|last| last.set(Some(id)));
    view()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_view_reads_the_decorations_of_its_own_window() {
        let tiled = Id::unique();
        let floating = Id::unique();
        let reported = Decorations {
            server_side: true,
            tiled: TiledEdges {
                left: true,
                ..TiledEdges::default()
            },
            ..Decorations::default()
        };
        set_decorations(tiled, reported);

        assert_eq!(decorations(), Decorations::default(), "before any view");
        assert_eq!(viewing(tiled, decorations), reported);
        assert_eq!(viewing(floating, decorations), Decorations::default());
        assert_eq!(
            viewing(floating, || viewing(tiled, decorations)),
            reported,
            "the innermost view wins"
        );
        viewing(tiled, || ());
        assert_eq!(
            decorations(),
            reported,
            "content built during layout reads the window viewed last"
        );
    }
}

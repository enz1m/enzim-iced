// Modified by enzUI contributors. See FORK.md.
//! enzICED deviations of the `container` widget from upstream, each pinned
//! against the upstream behaviour it replaces.
use iced_runtime::core::widget::Tree;
use iced_runtime::core::widget::tree::Tag;
use iced_runtime::core::{Element, Theme};
use iced_widget::{container, text_input};

type Renderer = iced_tiny_skia::Renderer;

/// The content owns a child tree, so a search for a widget's state by tag
/// finds the content's own tree rather than a copy on the container.
/// Upstream still reports the content's tag and state on the container.
///
/// Sabotage: return `self.content.as_widget().tag()` from `Container::tag`
/// and the container's tree carries the input's tag.
#[test]
fn a_container_does_not_wear_its_contents_tag() {
    let mut element: Element<'_, (), Theme, Renderer> = container(text_input("", "value")).into();
    let mut tree = Tree::new(element.as_widget());
    tree.diff(element.as_widget_mut());

    assert_eq!(tree.tag, Tag::stateless());
    assert_ne!(tree.children[0].tag, Tag::stateless());
}

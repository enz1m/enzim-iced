// Modified by enzUI contributors. See FORK.md.
pub use crate::graphics::Image;

#[derive(Debug, Default, Clone)]
pub struct Batch;

impl Batch {
    pub fn push(&mut self, _image: Image) {}

    pub fn clear(&mut self) {}

    pub fn is_empty(&self) -> bool {
        true
    }

    pub fn len(&self) -> usize {
        0
    }

    pub fn append(&mut self, _batch: &mut Self) {}
}

impl<'a> IntoIterator for &'a mut Batch {
    type Item = &'a mut Image;
    type IntoIter = std::iter::Empty<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        std::iter::empty()
    }
}

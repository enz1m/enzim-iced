// Modified by enzUI contributors. See FORK.md.
use crate::core::renderer::presentation::{Error, Program, State};
use crate::{Scene, Segment};
#[derive(Debug, Clone, Default)]
pub(crate) struct Assets;
impl Assets {
    pub(crate) fn usage(&self) -> (usize, usize) {
        (0, 0)
    }
}
#[derive(Debug, Clone)]
pub(crate) enum Captured {}
pub(crate) fn validate(_scene: &Scene, program: &Program) -> Result<(), Error> {
    if program.media_nodes().next().is_some() {
        Err(Error::UnsupportedBackend)
    } else {
        Ok(())
    }
}
pub(crate) fn apply(
    scene: &Scene,
    program: &Program,
    _state: &State,
    _segments: &mut [Segment],
) -> Result<(), Error> {
    validate(scene, program)
}

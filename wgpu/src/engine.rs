// Modified by enzUI contributors. See FORK.md.
use crate::clear;
use crate::graphics::{Antialiasing, Shell};
use crate::primitive;
use crate::quad;
use crate::text;
use crate::triangle;

use std::sync::{Arc, RwLock};

#[derive(Clone)]
pub struct Engine {
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
    pub(crate) format: wgpu::TextureFormat,

    pub(crate) clear_pipeline: clear::Pipeline,
    pub(crate) quad_pipeline: quad::Pipeline,
    pub(crate) text_pipeline: text::Pipeline,
    pub(crate) triangle_pipeline: triangle::Pipeline,
    #[cfg(any(feature = "image", feature = "svg"))]
    pub(crate) image_pipeline: crate::image::Pipeline,
    pub(crate) primitive_storage: Arc<RwLock<primitive::Storage>>,
    pub(crate) health: Arc<crate::device::Health>,
    _shell: Shell,
}

impl Engine {
    pub fn new(
        _adapter: &wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
        format: wgpu::TextureFormat,
        antialiasing: Option<Antialiasing>, // TODO: Initialize AA pipelines lazily
        shell: Shell,
    ) -> Self {
        Self {
            format,

            clear_pipeline: clear::Pipeline::new(&device, format),
            quad_pipeline: quad::Pipeline::new(&device, format),
            text_pipeline: text::Pipeline::new(&device, &queue, format),
            triangle_pipeline: triangle::Pipeline::new(&device, format, antialiasing),

            #[cfg(any(feature = "image", feature = "svg"))]
            image_pipeline: {
                let backend = _adapter.get_info().backend;

                crate::image::Pipeline::new(&device, format, backend)
            },

            primitive_storage: Arc::new(RwLock::new(primitive::Storage::default())),
            health: crate::device::Health::watch(&device),

            device,
            queue,
            _shell: shell,
        }
    }

    /// Whether the device under this engine has been lost; see
    /// [`Health`](crate::device::Health).
    pub fn device_lost(&self) -> bool {
        self.health.is_lost()
    }

    /// [`Self::device_lost`] after a poll, which runs a loss callback wgpu
    /// has not delivered yet (a destroyed device reports its loss there).
    pub(crate) fn confirm_device_lost(&self) -> bool {
        let _ = self.device.poll(wgpu::PollType::Poll);
        self.device_lost()
    }

    pub(crate) fn independent_presentation(&self) -> Self {
        let mut engine = self.clone();
        engine.text_pipeline = self.text_pipeline.independent(&self.device, &self.queue);
        engine.primitive_storage = Arc::new(RwLock::new(primitive::Storage::default()));
        engine
    }

    #[cfg(any(feature = "image", feature = "svg"))]
    pub fn create_image_cache(&self) -> crate::image::Cache {
        self.image_pipeline
            .create_cache(&self.device, &self.queue, &self._shell)
    }

    pub fn trim(&mut self) {
        self.text_pipeline.trim();
        self.trim_primitives();
    }

    /// Trims only the custom primitives' storage, after a frame that prepared
    /// nothing else (enzUI-ICED #3442).
    pub fn trim_primitives(&mut self) {
        self.primitive_storage
            .write()
            .expect("primitive storage should be writable")
            .trim();
    }
}

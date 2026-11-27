// Modified by enzUI contributors. See FORK.md.
use crate::clear;
use crate::graphics::{Antialiasing, Shell};
use crate::primitive;
use crate::quad;
use crate::text;
use crate::triangle;

use std::sync::{Arc, Mutex, OnceLock, RwLock};

#[derive(Clone)]
pub struct Engine {
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
    pub(crate) format: wgpu::TextureFormat,
    submission_gate: SubmissionGate,

    pub(crate) clear_pipeline: clear::Pipeline,
    pub(crate) quad_pipeline: quad::Pipeline,
    pub(crate) text_pipeline: text::Pipeline,
    triangle_pipeline: Arc<OnceLock<triangle::Pipeline>>,
    pub(crate) antialiasing: Option<Antialiasing>,
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
        antialiasing: Option<Antialiasing>,
        shell: Shell,
    ) -> Self {
        Self {
            format,
            submission_gate: SubmissionGate::default(),

            clear_pipeline: clear::Pipeline::new(&device, format),
            quad_pipeline: quad::Pipeline::new(&device, format),
            text_pipeline: text::Pipeline::new(&device, &queue, format),
            triangle_pipeline: Arc::new(OnceLock::new()),
            antialiasing,

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

    pub(crate) fn triangle_pipeline(&self) -> &triangle::Pipeline {
        self.triangle_pipeline
            .get_or_init(|| triangle::Pipeline::new(&self.device, self.format, self.antialiasing))
    }

    #[cfg(test)]
    pub(crate) fn has_triangle_pipeline(&self) -> bool {
        self.triangle_pipeline.get().is_some()
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
        self.image_pipeline.create_cache(
            &self.device,
            &self.queue,
            &self._shell,
            #[cfg(feature = "image")]
            self.submission_gate.clone(),
        )
    }

    pub(crate) fn submit(&self, command: wgpu::CommandBuffer) -> wgpu::SubmissionIndex {
        self.submission_gate.submit(&self.queue, command)
    }

    pub(crate) fn configure_surface(
        &self,
        surface: &wgpu::Surface<'_>,
        configuration: &wgpu::SurfaceConfiguration,
    ) {
        // wgpu requires the shared queue to stay idle while it recreates a swapchain.
        let _submissions = self.submission_gate.lock();
        if !self.device_lost() {
            surface.configure(&self.device, configuration);
        }
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

#[derive(Clone, Debug, Default)]
pub(crate) struct SubmissionGate(Arc<Mutex<()>>);

impl SubmissionGate {
    fn lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn submit(
        &self,
        queue: &wgpu::Queue,
        command: wgpu::CommandBuffer,
    ) -> wgpu::SubmissionIndex {
        let _submission = self.lock();
        queue.submit([command])
    }
}

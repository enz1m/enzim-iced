// Modified by enzUI contributors. See FORK.md.
//! Device creation shared by the window compositor and the headless renderer.
//!
//! On Vulkan the device is opened with the extensions that import DMA-BUF
//! images (`VK_EXT_image_drm_format_modifier`, `VK_EXT_external_memory_dma_buf`,
//! `VK_KHR_external_memory_fd`, and `VK_EXT_queue_family_foreign` when present)
//! whenever the adapter has them, so content another process renders (a web
//! engine, a video decoder, a camera) can be sampled on the renderer's own
//! device without a copy through the CPU. [`imports_dma_buf`] says whether a
//! device can. Everywhere else the device is requested as before.

/// Requests a device from `adapter` as `desc` describes, with DMA-BUF import
/// enabled where the adapter supports it.
pub(crate) async fn request(
    adapter: &wgpu::Adapter,
    desc: &wgpu::DeviceDescriptor<'_>,
) -> Result<(wgpu::Device, wgpu::Queue), wgpu::RequestDeviceError> {
    #[cfg(all(
        feature = "vulkan",
        not(target_arch = "wasm32"),
        not(target_vendor = "apple")
    ))]
    if let Some(result) = vulkan::request(adapter, desc) {
        return result;
    }
    adapter.request_device(desc).await
}

/// Whether a device is still usable, shared by every renderer built over it.
///
/// wgpu reports a lost device only through the device-lost callback, and it
/// treats a driver allocation failure the same way: under memory pressure one
/// failed allocation loses the whole device. Nothing else says so. Textures
/// created afterwards are invalid placeholders and surface configuration fails
/// silently, so the next view or frame acquisition panics. Renderers read
/// [`Health::is_lost`] to stop using the device, and the runtime rebuilds the
/// compositor on a new one.
#[derive(Debug, Default)]
pub struct Health {
    lost: std::sync::atomic::AtomicBool,
    reason: std::sync::Mutex<Option<String>>,
}

impl Health {
    /// Watches `device`: its loss callback marks the returned health lost.
    ///
    /// wgpu holds one callback per device, so this is called once, where the
    /// device is wrapped in an [`Engine`](crate::Engine).
    pub(crate) fn watch(device: &wgpu::Device) -> std::sync::Arc<Self> {
        let health = std::sync::Arc::new(Self::default());
        let watcher = std::sync::Arc::downgrade(&health);
        device.set_device_lost_callback(move |reason, message| {
            log::error!("Graphics device lost ({reason:?}): {message}");
            if let Some(health) = watcher.upgrade() {
                health.mark_lost(format!("{reason:?}: {message}"));
            }
        });
        health
    }

    /// Whether the device has been lost.
    pub fn is_lost(&self) -> bool {
        self.lost.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Why the device was lost, as wgpu reported it.
    pub fn reason(&self) -> Option<String> {
        self.reason.lock().ok().and_then(|reason| reason.clone())
    }

    fn mark_lost(&self, reason: String) {
        if let Ok(mut slot) = self.reason.lock() {
            let _ = slot.get_or_insert(reason);
        }
        self.lost.store(true, std::sync::atomic::Ordering::Release);
    }
}

/// Whether `device` was opened able to import DMA-BUF images: a Vulkan device
/// with the image-modifier, DMA-BUF and fd-memory extensions enabled.
pub fn imports_dma_buf(device: &wgpu::Device) -> bool {
    #[cfg(all(
        feature = "vulkan",
        not(target_arch = "wasm32"),
        not(target_vendor = "apple")
    ))]
    {
        vulkan::imports_dma_buf(device)
    }
    #[cfg(not(all(
        feature = "vulkan",
        not(target_arch = "wasm32"),
        not(target_vendor = "apple")
    )))]
    {
        let _ = device;
        false
    }
}

#[cfg(all(
    feature = "vulkan",
    not(target_arch = "wasm32"),
    not(target_vendor = "apple")
))]
mod vulkan {
    use ash::{ext, khr};
    use std::ffi::CStr;
    use wgpu::hal::api::Vulkan;

    /// Needed to import a DMA-BUF image.
    const REQUIRED: [&CStr; 3] = [
        ext::image_drm_format_modifier::NAME,
        ext::external_memory_dma_buf::NAME,
        khr::external_memory_fd::NAME,
    ];

    /// Used when present: lets an imported image be acquired from the
    /// producer's queue family.
    const OPTIONAL: [&CStr; 1] = [ext::queue_family_foreign::NAME];

    /// `None` when the adapter is not Vulkan or lacks a required extension;
    /// the caller then requests the device as usual.
    pub(super) fn request(
        adapter: &wgpu::Adapter,
        desc: &wgpu::DeviceDescriptor<'_>,
    ) -> Option<Result<(wgpu::Device, wgpu::Queue), wgpu::RequestDeviceError>> {
        #[allow(unsafe_code, reason = "opening a Vulkan device through wgpu-hal")]
        // SAFETY: the hal adapter is only read here and the device is opened
        // from it with every extension wgpu chose, plus ones the physical
        // device reports; `create_device_from_hal` takes the opened device
        // over with the same descriptor it was opened for.
        unsafe {
            let hal = adapter.as_hal::<Vulkan>()?;
            let instance = hal.shared_instance().raw_instance();
            let available = instance
                .enumerate_device_extension_properties(hal.raw_physical_device())
                .ok()?;
            let supported = |name: &CStr| {
                available
                    .iter()
                    .any(|property| property.extension_name_as_c_str() == Ok(name))
            };
            if !REQUIRED.iter().all(|name| supported(name)) {
                return None;
            }
            let wanted: Vec<&'static CStr> = REQUIRED
                .iter()
                .chain(OPTIONAL.iter())
                .copied()
                .filter(|name| supported(name))
                .collect();
            let opened = hal.open_with_callback(
                desc.required_features,
                &desc.required_limits,
                &desc.memory_hints,
                Some(Box::new(move |args| {
                    for name in wanted {
                        if !args.extensions.contains(&name) {
                            args.extensions.push(name);
                        }
                    }
                })),
            );
            drop(hal);
            let opened = match opened {
                Ok(opened) => opened,
                Err(error) => {
                    log::warn!("DMA-BUF import unavailable: opening the device failed: {error}");
                    return None;
                }
            };
            Some(adapter.create_device_from_hal(opened, desc))
        }
    }

    pub(super) fn imports_dma_buf(device: &wgpu::Device) -> bool {
        #[allow(unsafe_code, reason = "reading a Vulkan device's enabled extensions")]
        // SAFETY: the hal device is only read; the guard is dropped before
        // returning.
        unsafe {
            device.as_hal::<Vulkan>().is_some_and(|hal| {
                REQUIRED
                    .iter()
                    .all(|name| hal.enabled_device_extensions().contains(name))
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::core::renderer;
    use crate::graphics::{Antialiasing, Shell};
    use crate::{Engine, Renderer};

    fn engine() -> Engine {
        pollster::block_on(async {
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends: wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY),
                flags: wgpu::InstanceFlags::empty(),
                ..wgpu::InstanceDescriptor::new_without_display_handle()
            });
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions::default())
                .await
                .expect("an adapter is required to lose a device");
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor::default())
                .await
                .expect("device");
            Engine::new(
                &adapter,
                device,
                queue,
                wgpu::TextureFormat::Rgba8Unorm,
                Some(Antialiasing::MSAAx4),
                Shell::headless(),
            )
        })
    }

    #[test]
    fn a_lost_device_retires_a_lane_renderer_without_touching_the_device() {
        let engine = engine();
        let mut renderer =
            Renderer::new_presentation(engine.clone(), renderer::Settings::default());
        assert!(!renderer.confirm_device_lost(), "a fresh device is healthy");

        engine.device.destroy();
        assert!(
            renderer.confirm_device_lost(),
            "the loss callback marks the device lost"
        );
        assert!(
            engine.device_lost(),
            "every engine over the device shares its health"
        );

        // A lane retiring after the loss used to rebuild its text pipeline,
        // whose glyph atlas view panicked on the dead device.
        renderer.presentation_resources_active = true;
        renderer.retire_presentation_resources();
    }
}

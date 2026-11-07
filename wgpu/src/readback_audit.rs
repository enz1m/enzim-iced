// Modified by enzUI contributors. See FORK.md.
use std::sync::mpsc;
use wgpu::util::DeviceExt as _;

pub(crate) struct Audit(wgpu::Buffer);
pub(crate) struct Pending {
    buffer: wgpu::Buffer,
    ready: mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
}

impl Audit {
    pub(crate) fn record(
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        texture: &wgpu::Texture,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("readback alpha audit"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(
                r#"
@group(0) @binding(0) var source: texture_2d<f32>;
struct Result { count: atomic<u32>, first: atomic<u32> }
@group(0) @binding(1) var<storage, read_write> result: Result;
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(source);
    if id.x >= size.x || id.y >= size.y { return; }
    if textureLoad(source, vec2<i32>(id.xy), 0).a != 1.0 {
        atomicAdd(&result.count, 1u);
        atomicMin(&result.first, id.y * size.x + id.x);
    }
}
"#,
            )),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("readback alpha audit"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: std::num::NonZeroU64::new(8),
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("readback alpha audit"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("readback alpha audit"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        let result = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("readback alpha audit result"),
            contents: &[0, 0, 0, 0, 255, 255, 255, 255],
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("readback alpha audit"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: result.as_entire_binding(),
                },
            ],
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("readback alpha audit"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bindings, &[]);
            pass.dispatch_workgroups(texture.width().div_ceil(8), texture.height().div_ceil(8), 1);
        }
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback alpha audit copy"),
            size: 8,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_buffer_to_buffer(&result, 0, &readback, 0, 8);
        Self(readback)
    }

    pub(crate) fn map(self) -> Pending {
        let (send, ready) = mpsc::sync_channel(1);
        self.0
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = send.try_send(result);
            });
        Pending {
            buffer: self.0,
            ready,
        }
    }
}

impl Pending {
    pub(crate) fn finish(self) -> [u32; 2] {
        self.ready
            .try_recv()
            .expect("GPU audit map callback")
            .expect("GPU audit map");
        let bytes = self.buffer.slice(..).get_mapped_range();
        let result = [
            u32::from_le_bytes(bytes[0..4].try_into().unwrap()),
            u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
        ];
        drop(bytes);
        self.buffer.unmap();
        result
    }
}

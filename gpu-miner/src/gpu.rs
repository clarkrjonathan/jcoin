//! The GPU search, through wgpu: one compute dispatch tries a batch of nonces
//! and reports the smallest one whose hash meets the target.

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use crate::kernel::Config;
use crate::pow::GpuWork;

/// Threads per workgroup in the default configuration.
pub const WORKGROUP: u32 = 256;

/// Matches `struct Params` in the shaders (no implicit padding). The
/// hand-written sha256.wgsl declares only the first 24 words; the generated
/// kernels (`kernel.rs`) declare all of it.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    midstate: [u32; 8],
    tail: [u32; 5],
    nonce_hi: u32,
    nonce_lo_base: u32,
    _pad: u32,
    target: [u32; 8],
    /// `pow::prefix`: the state after round 5 and schedule words 16 to 20.
    pre: [u32; 8],
    wpre: [u32; 5],
}

pub struct Gpu {
    /// The shader variant and workgroup size this was opened with.
    pub config: Config,
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    bind_group: wgpu::BindGroup,
    params: wgpu::Buffer,
    result: wgpu::Buffer,
    readback: wgpu::Buffer,
    /// Adapter name and backend, for display.
    pub description: String,
}

fn instance() -> wgpu::Instance {
    wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle())
}

/// The GPUs (adapters) wgpu can use on this machine, in index order.
pub fn adapters() -> Vec<wgpu::Adapter> {
    pollster::block_on(instance().enumerate_adapters(wgpu::Backends::all()))
}

pub fn describe(adapter: &wgpu::Adapter) -> String {
    let i = adapter.get_info();
    format!("{} ({:?}, {:?})", i.name, i.backend, i.device_type)
}

impl Gpu {
    /// Open adapter `index` from `adapters()`, or the best one wgpu picks,
    /// with the default kernel.
    pub fn open(index: Option<usize>) -> Result<Gpu, String> {
        Gpu::open_with(index, Config::default())
    }

    /// `open` with a chosen kernel and workgroup size.
    pub fn open_with(index: Option<usize>, config: Config) -> Result<Gpu, String> {
        let adapter = match index {
            Some(i) => adapters()
                .into_iter()
                .nth(i)
                .ok_or(format!("no adapter with index {i} (see --list)"))?,
            None => pollster::block_on(instance().request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                ..Default::default()
            }))
            .map_err(|e| format!("no GPU adapter: {e}"))?,
        };
        let description = describe(&adapter);
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("jcoin-gpu-miner"),
            ..Default::default()
        }))
        .map_err(|e| format!("cannot open {description}: {e}"))?;
        let most = device.limits().max_compute_workgroup_size_x;
        if config.workgroup == 0 || config.workgroup > most {
            return Err(format!("workgroup size {} is not in 1..={most}", config.workgroup));
        }

        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sha256"),
            source: wgpu::ShaderSource::Wgsl(crate::kernel::source(config).into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("sha256"),
            layout: None,
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("params"),
            size: std::mem::size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let result = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("result"),
            contents: bytemuck::cast_slice(&[0u32, u32::MAX]),
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
        });
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: 8,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sha256"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: result.as_entire_binding(),
                },
            ],
        });
        Ok(Gpu {
            config,
            device,
            queue,
            pipeline,
            bind_group,
            params,
            result,
            readback,
            description,
        })
    }

    /// Largest batch one dispatch can cover (the per-dimension workgroup limit).
    pub fn max_batch(&self) -> u32 {
        self.device.limits().max_compute_workgroups_per_dimension * self.config.workgroup
    }

    /// Try nonces `(nonce_hi << 32) | lo` for `lo` in `lo_base .. lo_base +
    /// count`. Returns the smallest such `lo` whose hash meets the target.
    /// `count` must be a multiple of the workgroup size, at most
    /// `max_batch()`, and the range must not pass 2^32.
    pub fn search(&self, work: &GpuWork, nonce_hi: u32, lo_base: u32, count: u32) -> Option<u32> {
        assert!(count % self.config.workgroup == 0 && count <= self.max_batch());
        assert!(lo_base.checked_add(count - 1).is_some(), "batch would wrap the low word");
        let (pre, wpre) = crate::pow::prefix(work, nonce_hi);
        let p = Params {
            midstate: work.midstate,
            tail: work.tail,
            nonce_hi,
            nonce_lo_base: lo_base,
            _pad: 0,
            target: work.target,
            pre,
            wpre,
        };
        self.queue.write_buffer(&self.params, 0, bytemuck::bytes_of(&p));
        self.queue
            .write_buffer(&self.result, 0, bytemuck::cast_slice(&[0u32, u32::MAX]));

        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.dispatch_workgroups(count / self.config.workgroup, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&self.result, 0, &self.readback, 0, 8);
        self.queue.submit([encoder.finish()]);

        let slice = self.readback.slice(..);
        slice.map_async(wgpu::MapMode::Read, |r| r.expect("map readback"));
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("GPU poll failed");
        let (found, lo) = {
            let data = slice.get_mapped_range().expect("map range");
            let words: &[u32] = bytemuck::cast_slice(&data);
            (words[0], words[1])
        };
        self.readback.unmap();
        (found != 0).then_some(lo)
    }
}

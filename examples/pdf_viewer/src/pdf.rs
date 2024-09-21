use std::cell::RefCell;
use std::num::NonZeroUsize;
use iced::advanced:: Shell;
use iced::event;
use iced::mouse;
use iced::{ Rectangle, Size};
use iced::widget::shader::{self, Viewport};

use pdf::file::CachedFile;
use pdf_render::vello_backend::{OutlineBuilder, VelloBackend};
use pdf_render::{render_page, Cache};
use pathfinder_geometry::transform2d::Transform2F;
use vello::{DebugLayers, RendererOptions, Scene};

pub struct PDF {
    page_nr: u32,
    file: CachedFile<Vec<u8>>,
}

impl PDF {
    pub fn new(file: CachedFile<Vec<u8>>, page_nr: u32) -> Self {
        Self {
            page_nr,
            file,
        }
    }
}

pub struct State {
    cache: RefCell<Cache<OutlineBuilder>>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            cache: RefCell::new(Cache::new(OutlineBuilder::default())),
        }
    }
}

impl<Message> shader::Program<Message> for PDF {
    type State = State;
    type Primitive = PdfPrimitive;

    fn update(
        &self, _state: &mut Self::State, _event: shader::Event, _bounds: Rectangle, _cursor: mouse::Cursor,
        _shell: &mut Shell<'_, Message>,
    ) -> (event::Status, Option<Message>) {
        (event::Status::Ignored, None)
    }

    fn draw(&self, state: &Self::State, _cursor: mouse::Cursor, bounds: Rectangle) -> Self::Primitive {
        let mut cache = state.cache.borrow_mut();
        let mut backend: VelloBackend<'_> = VelloBackend::new(&mut *cache);

        let page = self.file.get_page(self.page_nr).unwrap();

        let resolver = self.file.resolver();

        let _ = render_page(&mut backend, &resolver, &page, Transform2F::default(),
        Some(pdf_render::Size::new(bounds.width, bounds.height)))
            .unwrap();

        let scene = backend.finish();

        PdfPrimitive::new(scene)
    }
}

pub struct PdfPrimitive {
    scene: Scene,
}

impl PdfPrimitive {
    pub fn new(scene: Scene) -> Self {
        Self { scene}
    }
}
use std::fmt;
impl fmt::Debug for PdfPrimitive {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PDF Primitive")
    }
}

struct Pipeline {
    renderer: vello::Renderer,
    render_params: vello::RenderParams,
    bind_group: Option<wgpu::BindGroup>,
    bind_group_layout: wgpu::BindGroupLayout,
    render_pipeline: wgpu::RenderPipeline,
}

impl Pipeline {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat, target_size: Size<u32>) -> Self {
        tracing::info!("Creating PDF pipeline {:?}", target_size);
        let renderer = vello::Renderer::new(
            device,
            RendererOptions {
                // 从 iced传递过来的format 是Bgra8UnormSrgb,
                // 但 vello并未支持，所以在此处硬编码为Bgra8Unorm
                surface_format: Some(wgpu::TextureFormat::Bgra8Unorm),
                use_cpu: false,
                num_init_threads: NonZeroUsize::new(1),
                antialiasing_support: vello::AaSupport::area_only(),
            },
        )
        .expect("Unable to create Vello Renderer");

        let render_params = vello::RenderParams {
            base_color: vello::peniko::Color::BLACK,
            width: target_size.width as u32,
            height: target_size.height as u32,
            antialiasing_method: vello::AaConfig::Area,
            debug: DebugLayers::none(),
        };

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("PDF bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("PDF Render Pipeline Layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("PDF Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader/blit.wgsl").into()),
        });

        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("PDF Render Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[],
                compilation_options:  wgpu::PipelineCompilationOptions::default()
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Bgra8UnormSrgb,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options:  wgpu::PipelineCompilationOptions::default()
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        Self {
            renderer,
            render_params,
            bind_group: None,
            bind_group_layout,
            render_pipeline,
        }
    }

    pub fn update(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, scene: &Scene, target_size: Size<u32>) {
        tracing::info!("Update PDF pipeline {:?}", target_size);

        let target: TargetTexture = TargetTexture::new(device, target_size.width, target_size.height);

        self.render_params.width = target_size.width as u32;
        self.render_params.height = target_size.height as u32;

        self.renderer
            .render_to_texture(device, queue, scene, &target.view, &self.render_params)
            .expect("Got non-Send/Sync error from rendering");

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("PDF bind group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&target.view),
                },
            ],
        });

        self.bind_group = Some(bind_group);
    }

    pub fn render(&self, iced_encoder: &mut wgpu::CommandEncoder, iced_target: &wgpu::TextureView, clip_bounds: &Rectangle<u32>) {
        if let Some(bind_group) = self.bind_group.as_ref() {
            {
                let mut render_pass = iced_encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("PDF Render Pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &iced_target,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });

                render_pass.set_pipeline(&self.render_pipeline);

                render_pass.set_scissor_rect(clip_bounds.x, clip_bounds.y, clip_bounds.width, clip_bounds.height);

                render_pass.set_bind_group(0, bind_group, &[]);
                render_pass.draw(0..6, 0..1);
            }
        }
    }
}

#[derive(Debug)]
struct TargetTexture {
    view: wgpu::TextureView,
    width: u32,
    height: u32,
}

impl TargetTexture {
    pub fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            format: wgpu::TextureFormat::Rgba8Unorm,
            view_formats: &[wgpu::TextureFormat::Rgba8UnormSrgb],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        Self { view, width, height }
    }
}

impl shader::Primitive for PdfPrimitive {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        storage: &mut iced_wgpu::primitive::Storage,
        bounds: &Rectangle,
        viewport: &Viewport,
    ) {
        if !storage.has::<Pipeline>() {
            storage.store(Pipeline::new(device, queue, format, viewport.physical_size()));
        }

        let pipeline = storage.get_mut::<Pipeline>().unwrap();

        // Upload data to GPU
        pipeline.update(device, queue, &self.scene, viewport.physical_size());
    }

    fn render(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        storage: &iced_wgpu::primitive::Storage,
        target: &wgpu::TextureView,
        clip_bounds: &Rectangle<u32>,
    ) {
        // At this point our pipeline should always be initialized
        let pipeline = storage.get::<Pipeline>().unwrap();

        // Render primitive
        pipeline.render(encoder, target, clip_bounds);
    }
}

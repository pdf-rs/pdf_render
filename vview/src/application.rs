use pathfinder_geometry::transform2d::Transform2F;
use pathfinder_geometry::vector::Vector2F;
use vello::kurbo::{Affine, Line, Stroke};
use vello::util::RenderContext;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};
use winit::dpi::LogicalSize;
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::Icon;

use std::fs::File;
use std::num::NonZeroUsize;
use std::sync::Arc;
use font::Encoder;
use pdf::file::{File as PdfFile, Cache as PdfCache, Log};
use pdf::any::AnySync;
use pdf::object::PageRc;
use pdf::PdfError;
use pdf::backend::Backend;
use pdf_render::vello_backend::{VelloBackend, OutlineBuilder};
use pdf_render::{Cache, page_bounds, render_page};

use vello::peniko::Color;
use vello::util::RenderSurface;
use vello::{Renderer, RendererOptions, Scene};
use vello::wgpu;

pub struct ActiveRenderState<'s> {
    // The fields MUST be in this order, so that the surface is dropped before the window
    surface: RenderSurface<'s>,
    window: Arc<Window>,
}

enum RenderState<'s> {
    Active(ActiveRenderState<'s>),
    // Cache a window so that it can be reused when the app is resumed after being suspended
    Suspended(Option<Arc<Window>>),
}

pub struct App<'a> {
    renderers: Vec<Option<Renderer>>,
    render_ctx : RenderContext,
    render_state : RenderState<'a>,
    modifiers: ModifiersState,
    mouse_down: bool,
    view_ctx: ViewContext
}

impl<'a>  App<'a> {
    fn new(view_ctx: ViewContext) -> Self {
        Self {
            renderers: vec![],
            render_ctx: RenderContext::new(),
            render_state : RenderState::Suspended(None),
            modifiers: ModifiersState::default(),
            mouse_down : false,
            view_ctx,
        }
    }

    pub fn run(view_ctx: ViewContext)
    {
        let event_loop = EventLoop::new().unwrap();

        event_loop.set_control_flow(ControlFlow::Wait);
        
        let mut app = App::new(view_ctx);

        let _ = event_loop.run_app(&mut app);
    }
}

impl<'a> ApplicationHandler for App<'a> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let RenderState::Suspended(cached_window) = &mut self.render_state else {
            return;
        };

        let window = cached_window
            .take()
            .unwrap_or_else(|| create_window(&event_loop));

        let size: winit::dpi::PhysicalSize<u32> = window.inner_size();
        let render_ctx = &mut self.render_ctx;
        let surface_future = render_ctx.create_surface(window.clone(), size.width, size.height, wgpu::PresentMode::AutoVsync);
        
        // We need to block here, in case a Suspended event appeared
        let surface: RenderSurface = pollster::block_on(surface_future).expect("Error creating surface");

        self.render_state = {
            self.renderers.resize_with(render_ctx.devices.len(), || None);
            self.renderers[surface.dev_id].get_or_insert_with(||create_vello_renderer(&render_ctx, &surface));
            RenderState::Active(ActiveRenderState { window, surface })
        };
        event_loop.set_control_flow(ControlFlow::Poll);
    }

    fn suspended(&mut self, event_loop: &ActiveEventLoop) {
        if let RenderState::Active(state) = &self.render_state {
            self.render_state = RenderState::Suspended(Some(state.window.clone()));
        }
        event_loop.set_control_flow(ControlFlow::Wait);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, window_id: WindowId, event: WindowEvent) {
        // Ignore the event (return from the function) if
        //   - we have no render_state
        //   - OR the window id of the event doesn't match the window id of our render_state
        //
        // Else extract a mutable reference to the render state from its containing option for use below
        let render_state = match &mut self.render_state {
            RenderState::Active(state) if state.window.id() == window_id => state,
            _ => return,
        };
        let render_ctx = &self.render_ctx;

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed {
                    if self.modifiers.shift_key() {
                        match event.logical_key {
                            Key::Named(NamedKey::ArrowRight) => self.view_ctx.seek_forward(10),
                            Key::Named(NamedKey::ArrowLeft) =>  self.view_ctx.seek_backwards(10),
                            _ => {}
                        }
                    } else {
                        match event.logical_key {
                            Key::Named(NamedKey::ArrowRight) => self.view_ctx.seek_forward(1),
                            Key::Named(NamedKey::ArrowLeft) =>  self.view_ctx.seek_backwards(1),
                            _ => {}
                        }
                    }
                }
            }
            WindowEvent::Resized(size) => {
                render_ctx.resize_surface(&mut render_state.surface, size.width, size.height);
                render_state.window.request_redraw();
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if button == MouseButton::Left {
                    self.mouse_down = state == ElementState::Pressed;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
            }
            WindowEvent::CursorLeft { .. } => {
            }
            WindowEvent::CursorMoved { position, .. } => {
            }
            WindowEvent::RedrawRequested => {
                let width = render_state.surface.config.width;
                let height = render_state.surface.config.height;
                let device_handle = &render_ctx.devices[render_state.surface.dev_id];
                
                let mut scene = Scene::new();
                if let Some(current) = self.view_ctx.get_current_mut() {
                    if let Some(s) = current.render(render_state.window.clone()) {
                        scene = s;
                    }
                }
    
                let antialiasing_method = vello::AaConfig::Area;
                let render_params = vello::RenderParams {
                    base_color: Color::WHITE,
                    width,
                    height,
                    antialiasing_method,
                };
                let surface_texture = render_state
                    .surface
                    .surface
                    .get_current_texture()
                    .expect("failed to get surface texture");

                vello::block_on_wgpu(
                    &device_handle.device,
                    self.renderers[render_state.surface.dev_id]
                        .as_mut()
                        .unwrap()
                        .render_to_surface_async(
                            &device_handle.device,
                            &device_handle.queue,
                            &scene,
                            &surface_texture,
                            &render_params,
                        ),
                )
                .expect("failed to render to surface");

                surface_texture.present();
                device_handle.device.poll(wgpu::Maintain::Poll);
            }
            _ => {}
        }
    }
}


pub struct FileContext {
    page_nr: u32,
    file: pdf::file::CachedFile<Vec<u8>>,
    cache: Cache<OutlineBuilder>,
}
impl FileContext {
    pub fn new(file: pdf::file::CachedFile<Vec<u8>>) -> Self {
        Self {
            page_nr: 0,
            file,
            cache: Cache::new(OutlineBuilder::default()),
        }
    }

    fn render(&mut self, window: Arc<Window>) -> Option<Scene> {
        let page = self.file.get_page(self.page_nr).ok()?;
        let mut backend = VelloBackend::new(&mut self.cache);
        let resolver = self.file.resolver();

        // Calculate the scale factor to fit the page into the window
        let page_size = page_bounds(&page);
        let window_size = window.inner_size();
        let scale_x = window_size.height as f32 / page_size.height();
        let scale_y = window_size.width as f32 / page_size.width();
        let transform = Transform2F::from_scale(scale_x.min(scale_y));

        render_page(&mut backend, &resolver, &page, transform).ok()?;

        Some(backend.finish())
    }
}
pub struct ViewContext {
    files: Vec<FileContext>,
    current_file: Option<usize>
}
impl ViewContext {
    pub fn new(files: Vec<FileContext>) -> Self {
        let current_file = if files.is_empty() {  None } else { Some(0) };

        Self {
            files,
            current_file
        }
    }

    fn get_current(&self) -> Option<&FileContext> {
        if let Some(n) = self.current_file {
            self.files.get(n)
        } else {
            None
        }
    }
    fn get_current_mut(&mut self) -> Option<&mut FileContext> {
        if let Some(n) = self.current_file {
            self.files.get_mut(n)
        } else {
            None
        }
    }
    fn seek_forward(&mut self, n: u32) {
        if let Some(current) = self.get_current_mut() {
            current.page_nr = (current.page_nr + n).min(current.file.num_pages());
        }
    }
    fn seek_backwards(&mut self, n: u32) {
        if let Some(current) = self.get_current_mut() {
            current.page_nr = current.page_nr.saturating_sub(n);
        }
    }
}


fn create_window(event_loop: &ActiveEventLoop) -> Arc<Window> {
    let icon = {
        let icon: image::ImageBuffer<image::Rgba<u8>, Vec<u8>> = image::load_from_memory_with_format(include_bytes!("../../logo.png"), image::ImageFormat::Png).unwrap().to_rgba8();

        let image = icon;
        let (width, height) = image.dimensions();
        let rgba = image.into_raw();
        Icon::from_rgba(rgba, width, height).unwrap()
    };

    let attr = Window::default_attributes()
        .with_inner_size(LogicalSize::new(1044, 800))
        .with_resizable(true)
        .with_title("PDF render demo")
        .with_window_icon(Some(icon));

    Arc::new(event_loop.create_window(attr).unwrap())
}

fn create_vello_renderer(render_ctx: &RenderContext, surface: &RenderSurface) -> Renderer {
    let id = surface.dev_id;

    eprintln!("Creating renderer {id}");
    Renderer::new(
        &render_ctx.devices[id].device,
        RendererOptions {
            surface_format: Some(surface.format),
            use_cpu: false,
            antialiasing_support: vello::AaSupport::all(),
            num_init_threads: NonZeroUsize::new(1),
        },
    )
    .expect("Could create renderer")
}
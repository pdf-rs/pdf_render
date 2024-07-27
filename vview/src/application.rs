use pathfinder_geometry::rect::RectF;
use pathfinder_geometry::transform2d::Transform2F;
use pathfinder_geometry::vector::Vector2F;
use vello::kurbo::{Affine, Line, Stroke, Vec2};
use vello::util::RenderContext;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::Icon;
use winit::window::{Window, WindowId};

use font::Encoder;
use pdf::any::AnySync;
use pdf::backend::Backend;
use pdf::file::{Cache as PdfCache, File as PdfFile, Log};
use pdf::object::{Page, PageRc};
use pdf::PdfError;
use pdf_render::vello_backend::{OutlineBuilder, VelloBackend};
use pdf_render::{page_bounds, render_page, Cache};
use std::collections::VecDeque;
use std::fs::File;
use std::num::NonZeroUsize;
use std::process;
use std::sync::Arc;

use vello::peniko::Color;
use vello::util::RenderSurface;
use vello::wgpu;
use vello::{Renderer, RendererOptions, Scene};

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
    render_ctx: RenderContext,
    render_state: RenderState<'a>,
    modifiers: ModifiersState,
    mouse_down: bool,
    view_ctx: ViewContext,
    prior_position: Option<Vector2F>,
    transform: Transform2F,
}

impl<'a> App<'a> {
    fn new(view_ctx: ViewContext) -> Self {
        Self {
            renderers: vec![],
            render_ctx: RenderContext::new(),
            render_state: RenderState::Suspended(None),
            modifiers: ModifiersState::default(),
            mouse_down: false,
            view_ctx,
            prior_position: None,
            transform: Transform2F::default(),
        }
    }

    pub fn run(view_ctx: ViewContext) {
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
        let surface_future = render_ctx.create_surface(
            window.clone(),
            size.width,
            size.height,
            wgpu::PresentMode::AutoVsync,
        );

        // We need to block here, in case a Suspended event appeared
        let surface: RenderSurface =
            pollster::block_on(surface_future).expect("Error creating surface");

        self.render_state = {
            self.renderers
                .resize_with(render_ctx.devices.len(), || None);
            self.renderers[surface.dev_id]
                .get_or_insert_with(|| create_vello_renderer(&render_ctx, &surface));
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

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
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
                            Key::Named(NamedKey::ArrowLeft) => self.view_ctx.seek_backwards(10),
                            _ => {}
                        }
                    } else {
                        match event.logical_key {
                            Key::Named(NamedKey::ArrowRight) => self.view_ctx.seek_forward(1),
                            Key::Named(NamedKey::ArrowLeft) => self.view_ctx.seek_backwards(1),
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
                const PIXELS_PER_LINE: f64 = 20.0;

                let delta = if let MouseScrollDelta::PixelDelta(delta) = delta {
                    delta.y
                } else if let MouseScrollDelta::LineDelta(_, y) = delta {
                    y as f64 * PIXELS_PER_LINE
                } else {
                    0.0
                };

                let mut scroll_offset = 0.;

                scroll_offset += delta as f32;
                dbg!(scroll_offset);
                // When to trigger rendering new page?
                // 1. scroll up,  current page bottom reached the top of the window
                // 2. scroll down, current page top reached the bottom of the window
                // How to find the position of bottom and top of current page relative to the window?

                self.transform = Transform2F::from_translation(Vector2F::new(0.0, delta as f32))
                    * self.transform;

                render_state.window.request_redraw();
            }
            WindowEvent::CursorLeft { .. } => {
                self.prior_position = None;
            }
            WindowEvent::CursorMoved { position, .. } => {
                let position = Vector2F::new(position.x as f32, position.y as f32);
                if self.mouse_down {
                    if let Some(prior) = self.prior_position {
                        self.transform =
                            Transform2F::from_translation(position - prior) * self.transform;
                    }
                }
                self.prior_position = Some(position);
            }
            WindowEvent::RedrawRequested => {
                let width = render_state.surface.config.width;
                let height = render_state.surface.config.height;
                let device_handle = &render_ctx.devices[render_state.surface.dev_id];

                let mut scene = Scene::new();
                if let Some(current) = self.view_ctx.get_current_mut() {
                    if let Some(s) = current.render(render_state.window.clone(), self.transform) {
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
                device_handle.device.poll(wgpu::Maintain::Wait);
            }
            _ => {}
        }
    }
}

pub struct FileContext {
    page_nr: u32,
    file: pdf::file::CachedFile<Vec<u8>>,
    cache: Cache<OutlineBuilder>,
    sliding_window: SlidingWindow<(u32, PageRc, Transform2F)>,
}
impl FileContext {
    pub fn new(file: pdf::file::CachedFile<Vec<u8>>) -> Self {
        Self {
            page_nr: 5,
            file,
            cache: Cache::new(OutlineBuilder::default()),
            sliding_window: SlidingWindow::new(5),
        }
    }

    fn render(&mut self, window: Arc<Window>, transform: Transform2F) -> Option<Scene> {
        let mut backend = VelloBackend::new(&mut self.cache);
        let resolver = self.file.resolver();

        let page: PageRc = self.file.get_page(self.page_nr).ok()?;

        // Calculate the scale factor to fit the page into the window
        let bounds: RectF = page_bounds(&page);

        // let window_size: winit::dpi::PhysicalSize<u32> = window.inner_size();
        // let scale_x = window_size.height as f32 / bounds.height();
        // let scale_y = window_size.width as f32 / bounds.width();
        // let transform = transform * Transform2F::from_scale(scale_x.min(scale_y));

        let current_page_nr = self.page_nr;
        let current_page_view_box = get_page_view_box(bounds, page.rotate as f32, transform);

        let threshhold = 100.0;
        let gap = 30.0;

        // cut the sliding window in half
        let sliding_window_size = self.sliding_window.get_window_size() as u32;
        let start_page = current_page_nr.saturating_sub(sliding_window_size / 2);
        let end_page = (current_page_nr + sliding_window_size / 2).min(self.file.num_pages());

        // the pages before current page
        let mut translate: Transform2F = transform;
        for page_nr in (start_page..current_page_nr).rev() {
            let page: PageRc = self.file.get_page(page_nr).ok()?;
            let bounds: RectF = page_bounds(&page);

            let page_view_box = get_page_view_box(bounds, page.rotate as f32, transform);

            translate =
                Transform2F::from_translation(Vector2F::new(0.0, -(page_view_box.height() + gap)))
                    * translate;

            self.sliding_window.push_front((page_nr, page, translate));
        }

        self.sliding_window.push_back((current_page_nr, page, transform));

        // the pages after current page
        let mut translate: Transform2F = transform;
        let mut previous_page_view_box = current_page_view_box;

        for page_nr in (current_page_nr + 1)..=end_page {
            let page: PageRc = self.file.get_page(page_nr).ok()?;
            let bounds: RectF = page_bounds(&page);

            translate = Transform2F::from_translation(Vector2F::new(
                0.0,
                previous_page_view_box.height() + gap,
            )) * translate;
            previous_page_view_box = get_page_view_box(bounds, page.rotate as f32, transform);

            self.sliding_window.push_back((page_nr, page, translate));
        }
        for (page_nr, page, transform) in self.sliding_window.iter() {
            render_page(&mut backend, &resolver, page, transform.clone()).ok()?;
        }

        let window_size: winit::dpi::PhysicalSize<u32> = window.inner_size();
        let window_br = Vector2F::new(window_size.width as f32, window_size.height as f32);

        if let Some((last_page_nr, last_page, last_page_transform)) = self.sliding_window.back() {
            dbg!(last_page_nr, self.sliding_window.len());

            let last_page_br = last_page_transform.translation();
            if window_br.y() <= last_page_br.y() && (last_page_br.y() <= window_br.y() + threshhold) {
                let next_page_nr = (last_page_nr + 1).min(self.file.num_pages());

                if next_page_nr > *last_page_nr {
                    let bounds: RectF = page_bounds(last_page);
                    let page_view_box = get_page_view_box(bounds, last_page.rotate as f32, transform);

                    let next_page: PageRc = self.file.get_page(next_page_nr).ok()?;

                    self.sliding_window.push_back((
                        next_page_nr,
                        next_page,
                        Transform2F::from_translation(Vector2F::new(
                            0.0,
                            page_view_box.height() + gap,
                        ))*last_page_transform.clone(),
                    ));

                    dbg!(next_page_nr, self.sliding_window.len());
                }
            }
        }
        if let Some((first_page_nr, first_page, first_page_transform)) = self.sliding_window.front() {
            // dbg!(first.2.translation());
            let first_page_br = first_page_transform.translation();
            let bounds: RectF = page_bounds(&first_page);

            let page_view_box = get_page_view_box(bounds, first_page.rotate as f32, transform);

            let top_y = first_page_br.y() - page_view_box.height() - gap;

            if  -threshhold <=  top_y && top_y <= 0.0 {
                let prev_page_nr = (*first_page_nr).saturating_sub(1);
                if prev_page_nr < *first_page_nr {
                    let page: PageRc = self.file.get_page(prev_page_nr).ok()?;
                    self.sliding_window.push_front((prev_page_nr, page));
                }
            }
        }

        Some(backend.finish())
    }
}
pub struct ViewContext {
    files: Vec<FileContext>,
    current_file: Option<usize>,
}
impl ViewContext {
    pub fn new(files: Vec<FileContext>, current_file: Option<usize>) -> Self {
        let current_file = if files.is_empty() {
            None
        } else {
            current_file.or(Some(0))
        };

        Self {
            files,
            current_file,
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
        let icon: image::ImageBuffer<image::Rgba<u8>, Vec<u8>> =
            image::load_from_memory_with_format(
                include_bytes!("../../logo.png"),
                image::ImageFormat::Png,
            )
            .unwrap()
            .to_rgba8();

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

fn get_page_view_box(page_bounds: RectF, page_rotate: f32, transform: Transform2F) -> RectF {
    // Calculate the view box
    let rotate: Transform2F = Transform2F::from_rotation(page_rotate * std::f32::consts::PI / 180.);
    let br: RectF = rotate * RectF::new(Vector2F::zero(), page_bounds.size());

    transform * br
}

struct SlidingWindow<T> {
    queue: VecDeque<T>,
    capacity: usize,
}

use std::collections::vec_deque::Iter;

impl<T> SlidingWindow<T> {
    fn new(capacity: usize) -> Self {
        SlidingWindow {
            queue: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    fn get_window_size(&self) -> usize {
        self.capacity
    }

    fn push_front(&mut self, item: T) {
        if self.queue.len() == self.capacity {
            self.queue.pop_back();
        }
        self.queue.push_front(item);
    }

    fn front(&self) -> Option<&T> {
        self.queue.front()
    }

    fn back(&self) -> Option<&T> {
        self.queue.back()
    }

    fn push_back(&mut self, item: T) {
        if self.queue.len() == self.capacity {
            self.queue.pop_front();
        }
        self.queue.push_back(item);
    }

    fn len(&self) -> usize {
        self.queue.len()
    }

    fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    pub fn iter(&self) -> Iter<'_, T> {
        self.queue.iter()
    }
}

use std::vec::IntoIter;

impl<T> IntoIterator for SlidingWindow<T> {
    type Item = T;
    type IntoIter = IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.queue.into_iter().collect::<Vec<T>>().into_iter()
    }
}

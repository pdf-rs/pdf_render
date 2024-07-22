#[macro_use] extern crate log;

use std::fs::File;
use std::sync::Arc;
use pdf::file::{File as PdfFile, Cache as PdfCache, Log};
use pdf::any::AnySync;
use pdf::object::PageRc;
use pdf::PdfError;
use pdf::backend::Backend;
use pdf_render::vello::VelloBackend;
use pdf_render::{Cache, SceneBackend, page_bounds, render_page};

use vello::kurbo::Affine;
use vello::peniko::Color;
use vello::util::{RenderContext, RenderSurface};
use vello::{Renderer, Scene};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::{
    event_loop::{EventLoop, EventLoopBuilder},
    window::Window,
};

struct RenderState {
    // SAFETY: We MUST drop the surface before the `window`, so the fields
    // must be in this order
    surface: RenderSurface,
    window: Window,
}

type UserEvent = ();

struct Args {

}

struct FileContext {
    page_nr: u32,
    file: pdf::file::CachedFile<Vec<u8>>,
    cache: Cache,
}
impl FileContext {
    fn render(&mut self) -> Option<Scene> {
        let page = self.file.get_page(self.page_nr).ok()?;
        let mut backend = VelloBackend::new(&mut self.cache);
        let resolver = self.file.resolver();
        render_page(&mut backend, &resolver, &page, Default::default()).ok()?;
        Some(backend.finish())
    }
}
struct ViewContext {
    files: Vec<FileContext>,
    current_file: Option<usize>
}
impl ViewContext {
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

fn run(
    event_loop: EventLoop<UserEvent>,
    args: Args,
    render_cx: RenderContext,
    view: &mut ViewContext
) {

    use winit::{event::*, event_loop::ControlFlow};
    let mut renderers: Vec<Option<Renderer>> = vec![];
    let mut render_cx = render_cx;
    let mut render_state = None::<RenderState>;
    let mut cached_window = None;
    let mut modifiers = ModifiersState::default();
    let mut mouse_down = false;

    event_loop.run(move |event, target| match event {
        Event::WindowEvent {
            ref event,
            window_id,
        } => {
            let Some(render_state) = &mut render_state else {
                return;
            };
            if render_state.window.id() != window_id {
                return;
            }
            match event {
                WindowEvent::CloseRequested => target.exit(),
                WindowEvent::ModifiersChanged(m) => modifiers = m.state(),
                WindowEvent::KeyboardInput { event, .. } => {
                    if event.state == ElementState::Pressed {
                        if modifiers.shift_key() {
                            match event.logical_key {
                                Key::Named(NamedKey::ArrowRight) => view.seek_forward(10),
                                Key::Named(NamedKey::ArrowLeft) =>  view.seek_backwards(10),
                                _ => {}
                            }
                        } else {
                            match event.logical_key {
                                Key::Named(NamedKey::ArrowRight) => view.seek_forward(1),
                                Key::Named(NamedKey::ArrowLeft) =>  view.seek_backwards(1),
                                _ => {}
                            }
                        }
                    }
                }
                WindowEvent::Resized(size) => {
                    render_cx.resize_surface(&mut render_state.surface, size.width, size.height);
                    render_state.window.request_redraw();
                }
                WindowEvent::MouseInput { state, button, .. } => {
                    if button == &MouseButton::Left {
                        mouse_down = state == &ElementState::Pressed;
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
                    let device_handle = &render_cx.devices[render_state.surface.dev_id];
                    
                    let mut scene = Scene::new();
                    if let Some(current) = view.get_current_mut() {
                        if let Some(s) = current.render() {
                            scene = s;
                        }
                    }
        
                    let antialiasing_method = vello::AaConfig::Area;
                    let render_params = vello::RenderParams {
                        base_color: Color::BLACK,
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
                        renderers[render_state.surface.dev_id]
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
        Event::AboutToWait => {
            if let Some(render_state) = &mut render_state {
                render_state.window.request_redraw();
            }
        }
        Event::Suspended => {
            eprintln!("Suspending");
            // When we suspend, we need to remove the `wgpu` Surface
            if let Some(render_state) = render_state.take() {
                cached_window = Some(render_state.window);
            }
            *control_flow = ControlFlow::Wait;
        }
        Event::Resumed => {
            let Option::None = render_state else { return };
            let window = cached_window
                .take()
                .unwrap_or_else(|| create_window(_event_loop));
            let size = window.inner_size();
            let surface_future = render_cx.create_surface(&window, size.width, size.height);
            // We need to block here, in case a Suspended event appeared
            let surface = pollster::block_on(surface_future).expect("Error creating surface");
            render_state = {
                let render_state = RenderState { window, surface };
                renderers.resize_with(render_cx.devices.len(), || None);
                let id = render_state.surface.dev_id;
                renderers[id].get_or_insert_with(|| {
                    eprintln!("Creating renderer {id}");
                    Renderer::new(
                        &render_cx.devices[id].device,
                        RendererOptions {
                            surface_format: Some(render_state.surface.format),
                            use_cpu,
                            antialiasing_support: vello::AaSupport::all(),
                        },
                    )
                    .expect("Could create renderer")
                });
                Some(render_state)
            };
            *control_flow = ControlFlow::Poll;
        }
        _ => {}
    });
}

fn create_window(event_loop: &winit::event_loop::EventLoopWindowTarget<()>) -> Window {
    use winit::{dpi::LogicalSize, window::WindowBuilder};
    let icon = image::load_from_memory_with_format(include_bytes!("../../logo.png"), image::ImageFormat::Png).unwrap().to_rgba8().into();
    WindowBuilder::new()
        .with_inner_size(LogicalSize::new(1044, 800))
        .with_resizable(true)
        .with_title("Vello demo")
        .with_window_icon(Some(icon))
        .build(event_loop)
        .unwrap()
}

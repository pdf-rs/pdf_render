#[macro_use] extern crate log;

use pathfinder_view::{show, Config, Interactive, Context, Emitter, ElementState, KeyCode, KeyEvent};
use pathfinder_resources::embedded::EmbeddedResourceLoader;
use pathfinder_color::ColorF;
use pathfinder_renderer::scene::Scene;
use pathfinder_geometry::vector::Vector2F;

use pdf::file::File as PdfFile;
use pdf::backend::Backend;
use pdf_render::{Cache, ItemMap, TraceItem};


pub struct PdfView<B: Backend> {
    file: PdfFile<B>,
    num_pages: usize,
    cache: Cache,
    map: Option<ItemMap>,
}
impl<B: Backend> PdfView<B> {
    pub fn new(file: PdfFile<B>) -> Self {
        PdfView {
            num_pages: file.num_pages() as usize,
            file,
            cache: Cache::new(),
            map: None,
        }
    }
}
impl<B: Backend + 'static> Interactive for PdfView<B> {
    type Event = Vec<u8>;
    fn title(&self) -> String {
        self.file.trailer.info_dict.as_ref()
            .and_then(|info| info.get("Title"))
            .and_then(|p| p.as_str().map(|s| s.into_owned()))
            .unwrap_or_else(|| "PDF View".into())
    }
    fn init(&mut self, ctx: &mut Context, sender: Emitter<Self::Event>) {
        ctx.num_pages = self.num_pages;
        ctx.set_icon(image::load_from_memory_with_format(include_bytes!("../../../logo.png"), image::ImageFormat::Png).unwrap().to_rgba8().into());
    }
    fn scene(&mut self, ctx: &mut Context) -> Scene {
        let page = self.file.get_page(ctx.page_nr as u32).unwrap();

        ctx.set_bounds(self.cache.page_bounds(&self.file, &page));

        let (scene, map) = self.cache.render_page(&self.file, &page, ctx.view_transform()).unwrap();
        self.map = Some(map);
        scene
    }
    fn mouse_input(&mut self, ctx: &mut Context, page: usize, pos: Vector2F, state: ElementState) {
        if state != ElementState::Pressed { return; }
        info!("x={}, y={}", pos.x(), pos.y());

        if let Some(ref map) = self.map {
            for item in map.matches(pos) {
                match item {
                    TraceItem::Single(_, op) => info!("{}", op),
                    TraceItem::Multi(ref ops) => for &(_, ref op) in ops {
                        info!("{}", op);
                    }
                }
            }
        }
    }
    fn keyboard_input(&mut self, ctx: &mut Context, event: &mut KeyEvent) {
        if event.state == ElementState::Released {
            return;
        }
        match event.keycode {
            KeyCode::Right | KeyCode::PageDown => ctx.next_page(),
            KeyCode::Left | KeyCode::PageUp => ctx.prev_page(),
            KeyCode::S => self.cache.report(),
            _ => return
        }
        ctx.request_redraw();
    }
}

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

#[cfg(target_arch = "wasm32")]
use js_sys::Uint8Array;

#[cfg(target_arch = "wasm32")]
use web_sys::{HtmlCanvasElement};

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(start)]
pub fn run() {
    std::panic::set_hook(Box::new(console_error_panic_hook::hook));
    console_log::init_with_level(log::Level::Info);
    warn!("test");
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn show(canvas: HtmlCanvasElement, data: &Uint8Array) -> WasmView {
    use pathfinder_resources::embedded::EmbeddedResourceLoader;

    let data: Vec<u8> = data.to_vec();
    info!("got {} bytes of data", data.len());
    let file = PdfFile::from_data(data).expect("failed to parse PDF");
    info!("got the file");
    let view = PdfView::new(file);

    let mut config = Config::new(Box::new(EmbeddedResourceLoader));
    config.zoom = false;
    config.pan = false;
    WasmView::new(
        canvas,
        config,
        Box::new(view) as _
    )
}


fn main() {
    env_logger::init();
    let path = std::env::args().nth(1).unwrap();
    let file = PdfFile::<Vec<u8>>::open(&path).unwrap();
    let view = PdfView::new(file);
    let mut config = Config::new(Box::new(EmbeddedResourceLoader));
    config.zoom = true;
    config.pan = true;
    config.background = ColorF::new(0.9, 0.9, 0.9, 1.0);
    show(view, config);
}

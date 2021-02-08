use pathfinder_view::{show, Config};
use pathfinder_resources::embedded::EmbeddedResourceLoader;
use pathfinder_color::ColorF;

use pdf::file::File as PdfFile;
use pdf_view::PdfView;


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

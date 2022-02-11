use argh::FromArgs;
use pdf::file::File;
use pdf_render::{Cache, SceneBackend, render_page};
use pathfinder_rasterize::Rasterizer;
use pathfinder_geometry::transform2d::Transform2F;
use std::error::Error;

use std::path::PathBuf;

#[derive(FromArgs)]
///  PDF rasterizer
struct Options {
    /// DPI
    #[argh(option, default="150.")]
    dpi: f32,

    /// page to render (0 based)
    #[argh(option, default="0")]
    page: u32,

    /// input PDF file
    #[argh(positional)]
    pdf: PathBuf,

    /// output image
    #[argh(positional)]
    image: PathBuf,
}

fn main() -> Result<(), Box<dyn Error>> {
    env_logger::init();
    let opt: Options = argh::from_env();

    let file = File::open(&opt.pdf)?;
    let page = file.get_page(opt.page)?;

    let mut cache = Cache::new();
    let mut backend = SceneBackend::new(&mut cache);

    render_page(&mut backend, &file, &page, Transform2F::from_scale(opt.dpi / 25.4))?;

    let image = Rasterizer::new().rasterize(backend.finish(), None);

    image.save(opt.image)?;

    Ok(())
}
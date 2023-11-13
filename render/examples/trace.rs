use pdf::file::FileOptions;
use pdf_render::render_page;
use pdf_render::tracer::{TraceCache, Tracer};

fn main() {
    env_logger::init();
    let arg = std::env::args().nth(1).unwrap();

    let file = FileOptions::cached().open(&arg).unwrap();
    let resolver = file.resolver();

    let mut cache = TraceCache::new();

    for page in file.pages() {
        let p = page.unwrap();
        let mut clip_paths = vec![];
        let mut backend = Tracer::new(&mut cache, &mut clip_paths);
        render_page(&mut backend, &resolver, &p, Default::default()).unwrap();
        let items = backend.finish();
        for i in items {
            println!("{:?}", i);
        }
    }
}

use pdf::file::File;
use pdf_render::tracer::{TraceCache, Tracer};
use pdf_render::render_page;

fn main() {
    env_logger::init();
    let arg = std::env::args().nth(1).unwrap();

    let file = File::<Vec<u8>>::open(&arg).unwrap();
    
    let mut cache = TraceCache::new();
    for page in file.pages() {
        let p = page.unwrap();
        let mut backend = Tracer::new(&mut cache);
        render_page(&mut backend, &file, &p, Default::default()).unwrap();
        let items = backend.finish();
        for i in items {
            println!("{:?}", i);
        }
    }
}
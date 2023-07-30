use criterion::{black_box, criterion_group, criterion_main, Criterion};

use pdf::file::{FileOptions};
use pdf::object::*;
use std::path::Path;
use pdf_render::{Cache, render_page, SceneBackend};
use pathfinder_renderer::scene::Scene;

fn render_file(path: &Path) -> Vec<Scene> {
    let file = FileOptions::cached().open(path).unwrap();
    let resolver = file.resolver();
    
    let mut cache = Cache::new();
    file.pages().map(|page| {
        let p: &Page = &*page.unwrap();
        let mut backend = SceneBackend::new(&mut cache);
        render_page(&mut backend, &resolver, p, Default::default()).unwrap();
        backend.finish()
    }).collect()
}

fn bench_file(c: &mut Criterion, name: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("files").join(name);
    c.bench_function(name, |b| b.iter(|| render_file(&path)));
}

macro_rules! bench_files {
    (@a $($file:expr, $name:ident;)*) => (
        $(
            fn $name(c: &mut Criterion) {
                bench_file(c, $file)
            }
        )*

    );
    (@b $($file:expr, $name:ident;)*) => (
        criterion_group!(benches $(, $name)*);
    );
    ($($file:expr, $name:ident;)*) => (
        bench_files!(@a $($file, $name;)*);
        bench_files!(@b $($file, $name;)*);
    );
}

bench_files!(
    "example.pdf", example;
    "ep.pdf", ep;
    "ep2.pdf", ep2;
    "libreoffice.pdf", libreoffice;
    "pdf-sample.pdf", pdf_sample;
    "xelatex-drawboard.pdf", xelatex_drawboard;
    "xelatex.pdf", xelatex;
    "PDF32000_2008.pdf", pdf32000;
);

criterion_main!(benches);

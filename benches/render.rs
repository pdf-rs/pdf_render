use criterion::{black_box, criterion_group, criterion_main, Criterion};

use pdf::file::{FileOptions};
use pdf_render::{Cache, render_page, SceneBackend};
use std::time::Duration;

fn bench_render_page(c: &mut Criterion) {
    let file = FileOptions::cached().open("/home/sebk/Downloads/10.1016@j.eswa.2020.114101.pdf").unwrap();
    let resolver = file.resolver();

    let mut group = c.benchmark_group("10.1016@j.eswa.2020.114101.pdf");
    group.sample_size(50);
    group.warm_up_time(Duration::from_secs(1));

    let mut cache = Cache::new();
    let mut backend = SceneBackend::new(&mut cache);
    for (i, page) in file.pages().enumerate() {
        if let Ok(page) = page {
            group.bench_function(&format!("page {}", i), |b| b.iter(|| render_page(&mut backend, &resolver, &page, Default::default()).unwrap()));
        }
    }
    group.finish();
}

criterion_group!(benches, bench_render_page);
criterion_main!(benches);

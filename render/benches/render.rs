use criterion::{black_box, criterion_group, criterion_main, Criterion};

use pdf::file::File as PdfFile;
use pdf_render::Cache;
use std::time::Duration;

fn bench_render_page(c: &mut Criterion) {
    let file = PdfFile::<Vec<u8>>::open("/home/sebk/Downloads/10.1016@j.eswa.2020.114101.pdf").unwrap();
    let mut group = c.benchmark_group("10.1016@j.eswa.2020.114101.pdf");
    group.sample_size(50);
    group.warm_up_time(Duration::from_secs(1));

    let mut cache = Cache::new();
    for (i, page) in file.pages().enumerate() {
        if let Ok(page) = page {
            group.bench_function(&format!("page {}", i), |b| b.iter(|| cache.render_page(&file, &page, Default::default()).unwrap()));
        }
    }
    group.finish();
}

criterion_group!(benches, bench_render_page);
criterion_main!(benches);

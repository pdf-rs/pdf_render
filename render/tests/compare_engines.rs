//! Render the same PDF page with two independent engines and compare the output.
//!
//! One side is `pdf_render` itself, rasterised with the CPU backend (`vello_cpu`),
//! the other side is an external PDF renderer, by default Poppler's `pdftoppm`.
//! Both run at the same resolution, so the images should agree apart from
//! anti-aliasing and font substitution; the test reports how far apart they are
//! and fails when the page structure is visibly different.
//!
//! Run it with:
//!
//! ```text
//! cargo test -p pdf_render --features vello_cpu --test compare_engines -- --nocapture
//! ```
//!
//! Environment:
//!
//! * `PDF_COMPARE_ENGINE`     path to the external renderer (default: `pdftoppm`
//!                            from `PATH`); it must accept
//!                            `-png -r DPI -f N -l N -singlefile IN OUT`
//! * `PDF_COMPARE_PDF`        colon separated list of PDFs; replaces the default corpus
//! * `PDF_COMPARE_DIR`        directory of `*.pdf` files to add
//! * `PDF_COMPARE_DPI`        resolution for both engines (default 72)
//! * `PDF_COMPARE_MAX_PAGES`  pages per document (default 2)
//! * `PDF_COMPARE_REQUIRE`    set to 1 to fail instead of skipping when the external
//!                            engine is missing
//! * `PDF_COMPARE_OUT`        where the comparison images go
//!                            (default `<repo>/target/pdf-compare`)
//!
//! For every page three PNGs are kept (`*-ours.png`, `*-engine.png`, `*-diff.png`)
//! so the difference can be inspected by eye.
//!
//! The default corpus is `wasm/logo.pdf` from this repository plus a handful of
//! documents from the `pdf` checkout next to it (`../../pdf/files`), when that
//! checkout is present. Pages listed in [`KNOWN_GAPS`] are printed and counted
//! but do not fail the test.

#![cfg(feature = "vello_cpu")]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use image::RgbaImage;
use pathfinder_geometry::transform2d::Transform2F;
use pathfinder_geometry::vector::vec2f;
use pdf::file::FileOptions;
use pdf_render::vello_cpu_backend::{OutlineBuilder, VelloCpuBackend};
use pdf_render::{page_bounds, render_page, Cache};

/// Resolution both engines render at, in pixels per inch.
const DEFAULT_DPI: f32 = 72.0;
/// Pages per document that are compared.
const DEFAULT_MAX_PAGES: u32 = 2;
/// A channel difference above this counts as a differing pixel.
const PIXEL_TOLERANCE: u8 = 48;
/// Largest allowed fraction of differing pixels per page.
const MAX_OUTLIER_FRACTION: f64 = 0.20;
/// Largest allowed difference of the structural (16x16 block) comparison, in
/// 0..1 of full scale. Font substitution on its own stays well below this.
const MAX_STRUCTURE_DIFF: f64 = 0.06;

/// Documents whose pages are compared and reported but do not fail the test:
/// differences we know about and that are not part of what this test guards
/// (yet). Remove an entry when the cause is fixed.
const KNOWN_GAPS: &[(&str, &str)] = &[];

fn known_gap(stem: &str) -> Option<&'static str> {
    KNOWN_GAPS
        .iter()
        .find(|(names, _)| names.split(',').any(|name| name == stem))
        .map(|(_, why)| *why)
}

#[test]
fn pages_match_a_second_engine() {
    let Some(engine) = engine_binary() else {
        let msg = "no external PDF renderer found (install poppler-utils / pdftoppm, \
                   or set PDF_COMPARE_ENGINE=/path/to/pdftoppm)";
        if env_flag("PDF_COMPARE_REQUIRE") {
            panic!("{msg}");
        }
        eprintln!("skipping cross-engine comparison: {msg}");
        return;
    };
    eprintln!("external engine: {}", engine.display());
    point_standard_fonts_at_the_repository();

    let dpi = env_f32("PDF_COMPARE_DPI", DEFAULT_DPI);
    let max_pages = env_u32("PDF_COMPARE_MAX_PAGES", DEFAULT_MAX_PAGES);
    let out_dir = output_dir();

    let documents = corpus();
    if documents.is_empty() {
        eprintln!("skipping cross-engine comparison: no PDF to render");
        return;
    }

    let mut pages_compared = 0usize;
    let mut failures: Vec<String> = Vec::new();
    let mut lines: Vec<String> = Vec::new();

    for path in &documents {
        let file = match FileOptions::cached().open(path) {
            Ok(file) => file,
            Err(e) => {
                eprintln!("cannot open {}: {e:?}", path.display());
                continue;
            }
        };
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "document".into());
        let page_count = file.num_pages().min(max_pages);

        for page_index in 0..page_count {
            let page_number = page_index + 1;

            let ours = render_ours(&file, page_index, dpi);
            let ours = match ours {
                Ok(image) => image,
                Err(e) => {
                    failures.push(format!("{} page {page_number}: {e}", path.display()));
                    continue;
                }
            };

            let prefix = out_dir.join(format!("{stem}-p{page_number}"));
            let theirs = match render_engine(&engine, path, page_number, dpi, &prefix) {
                Ok(image) => image,
                Err(e) => {
                    failures.push(format!("{} page {page_number}: {e}", path.display()));
                    continue;
                }
            };

            save(&prefix.with_extension("ours.png"), &ours);
            save(&prefix.with_extension("engine.png"), &theirs);

            let report = compare(&ours, &theirs);
            save(&prefix.with_extension("diff.png"), &report.diff_image);
            let line = format!(
                "{:<28} page {:>2}  {:>4}x{:<4} vs {:>4}x{:<4}  mean {:>6.2}%  \
                 outliers {:>5.2}%  structure {:>6.2}%",
                stem,
                page_number,
                ours.width(),
                ours.height(),
                theirs.width(),
                theirs.height(),
                report.mean_error * 100.0,
                report.outlier_fraction * 100.0,
                report.structure_error * 100.0,
            );
            println!("{line}");
            lines.push(line);

            if report.outlier_fraction > MAX_OUTLIER_FRACTION
                || report.structure_error > MAX_STRUCTURE_DIFF
            {
                let detail = format!(
                    "{} page {page_number}: {:.1}% of pixels differ and the page structure \
                     differs by {:.2}% (see {})",
                    path.display(),
                    report.outlier_fraction * 100.0,
                    report.structure_error * 100.0,
                    prefix.with_extension("diff.png").display(),
                );
                match known_gap(&stem) {
                    Some(why) => {
                        let line = format!("known gap: {detail}\n           reason: {why}");
                        println!("{line}");
                        lines.push(line);
                    }
                    None => failures.push(format!(
                        "{detail}; limit {:.0}% / {:.0}%",
                        MAX_OUTLIER_FRACTION * 100.0,
                        MAX_STRUCTURE_DIFF * 100.0,
                    )),
                }
            }
            pages_compared += 1;
        }
    }

    assert!(pages_compared > 0, "no page could be compared: {failures:#?}");
    let header = format!(
        "external engine: {}\ndpi: {dpi}\n\n{}",
        engine.display(),
        lines.join("\n")
    );
    let report_path = out_dir.join("report.txt");
    if let Err(e) = std::fs::write(&report_path, format!("{header}\n")) {
        eprintln!("cannot write {}: {e}", report_path.display());
    } else {
        eprintln!("comparison report: {}", report_path.display());
    }
    if !failures.is_empty() {
        panic!(
            "{} of {pages_compared} compared pages disagree with {}:\n{}",
            failures.len(),
            engine.display(),
            failures.join("\n")
        );
    }
}

/// The concrete document type produced by `FileOptions::cached()`.
type CachedPdf = pdf::file::File<
    Vec<u8>,
    pdf::file::ObjectCache,
    pdf::file::StreamCache,
    pdf::file::NoLog,
>;

/// Render one page with this crate's CPU backend.
fn render_ours(file: &CachedPdf, page_index: u32, dpi: f32) -> Result<RgbaImage, String> {
    let page = file
        .get_page(page_index)
        .map_err(|e| format!("cannot load page: {e:?}"))?;
    let bounds = page_bounds(&page);
    // `page_bounds` and the root transform are in millimetres, so pixels per
    // millimetre is what `dpi` has to be converted to.
    let scale = dpi / 25.4;
    let width = (bounds.width() * scale).ceil().max(1.0) as u16;
    let height = (bounds.height() * scale).ceil().max(1.0) as u16;

    let mut cache = Cache::<OutlineBuilder>::new(OutlineBuilder::default());
    let mut backend = VelloCpuBackend::new(&mut cache, width, height);
    backend.fill_background(1.0, 1.0, 1.0);
    render_page(
        &mut backend,
        &file.resolver(),
        &page,
        Transform2F::from_scale(vec2f(scale, scale)),
    )
    .map_err(|e| format!("rendering failed: {e:?}"))?;

    let png = backend
        .finish()
        .into_png()
        .map_err(|e| format!("png encode failed: {e:?}"))?;
    image::load_from_memory(&png)
        .map(|i| i.to_rgba8())
        .map_err(|e| format!("cannot decode our png: {e}"))
}

/// Render one page with the external engine.
fn render_engine(
    engine: &Path,
    pdf: &Path,
    page_number: u32,
    dpi: f32,
    out_prefix: &Path,
) -> Result<RgbaImage, String> {
    if let Some(dir) = out_prefix.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let output = Command::new(engine)
        .arg("-png")
        .arg("-r")
        .arg(format!("{dpi}"))
        .arg("-f")
        .arg(page_number.to_string())
        .arg("-l")
        .arg(page_number.to_string())
        .arg("-singlefile")
        .arg(pdf)
        .arg(out_prefix)
        .output()
        .map_err(|e| format!("cannot run {}: {e}", engine.display()))?;
    if !output.status.success() {
        return Err(format!(
            "{} failed with {}: {}",
            engine.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let png = out_prefix.with_extension("png");
    image::open(&png)
        .map(|i| i.to_rgba8())
        .map_err(|e| format!("cannot decode {}: {e}", png.display()))
}

struct Diff {
    mean_error: f64,
    outlier_fraction: f64,
    structure_error: f64,
    diff_image: RgbaImage,
}

fn compare(a: &RgbaImage, b: &RgbaImage) -> Diff {
    let width = a.width().min(b.width());
    let height = a.height().min(b.height());

    let mut sum = 0u64;
    let mut outliers = 0u64;
    let mut diff_image = RgbaImage::new(width, height);

    for y in 0..height {
        for x in 0..width {
            let pa = a.get_pixel(x, y);
            let pb = b.get_pixel(x, y);
            let mut worst = 0u8;
            for c in 0..3 {
                let delta = pa[c].abs_diff(pb[c]);
                sum += u64::from(delta);
                worst = worst.max(delta);
            }
            if worst > PIXEL_TOLERANCE {
                outliers += 1;
            }
            diff_image.put_pixel(x, y, image::Rgba([worst.saturating_mul(4), 0, 0, 255]));
        }
    }

    let pixels = u64::from(width) * u64::from(height);
    let (mean_error, outlier_fraction) = if pixels == 0 {
        (1.0, 1.0)
    } else {
        (
            sum as f64 / (pixels as f64 * 3.0 * 255.0),
            outliers as f64 / pixels as f64,
        )
    };

    Diff {
        mean_error,
        outlier_fraction,
        structure_error: structure_error(a, b, width, height),
        diff_image,
    }
}

/// Mean absolute difference of the two pages after reducing them to a grid of
/// block averages: insensitive to anti-aliasing and glyph shapes, sensitive to
/// moved, missing or miscoloured content.
fn structure_error(a: &RgbaImage, b: &RgbaImage, width: u32, height: u32) -> f64 {
    const BLOCKS: u32 = 16;
    if width == 0 || height == 0 {
        return 1.0;
    }
    let mut sum = 0f64;
    for by in 0..BLOCKS {
        for bx in 0..BLOCKS {
            let x0 = bx * width / BLOCKS;
            let x1 = ((bx + 1) * width / BLOCKS).max(x0 + 1).min(width);
            let y0 = by * height / BLOCKS;
            let y1 = ((by + 1) * height / BLOCKS).max(y0 + 1).min(height);
            let mut sa = [0f64; 3];
            let mut sb = [0f64; 3];
            for y in y0..y1 {
                for x in x0..x1 {
                    let pa = a.get_pixel(x, y);
                    let pb = b.get_pixel(x, y);
                    for c in 0..3 {
                        sa[c] += f64::from(pa[c]);
                        sb[c] += f64::from(pb[c]);
                    }
                }
            }
            let n = f64::from((x1 - x0) * (y1 - y0));
            for c in 0..3 {
                sum += (sa[c] - sb[c]).abs() / (n * 255.0);
            }
        }
    }
    sum / f64::from(BLOCKS * BLOCKS * 3)
}

fn save(path: &Path, image: &RgbaImage) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = image.save(path) {
        eprintln!("cannot write {}: {e}", path.display());
    }
}

/// The external renderer: `PDF_COMPARE_ENGINE` or `pdftoppm` from `PATH`.
fn engine_binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("PDF_COMPARE_ENGINE") {
        let path = PathBuf::from(path);
        return path.is_file().then_some(path);
    }
    let path_var: OsString = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|dir| dir.join("pdftoppm"))
        .find(|candidate| candidate.is_file())
}

/// PDFs to compare: `PDF_COMPARE_PDF`, `PDF_COMPARE_DIR`, and documents from the
/// repository as a default.
fn corpus() -> Vec<PathBuf> {
    let mut pdfs: Vec<PathBuf> = Vec::new();

    if let Some(list) = std::env::var_os("PDF_COMPARE_PDF") {
        pdfs.extend(
            std::env::split_paths(&list)
                .map(PathBuf::from)
                .filter(|p| p.is_file()),
        );
    }
    if let Some(dir) = std::env::var_os("PDF_COMPARE_DIR") {
        if let Ok(entries) = std::fs::read_dir(PathBuf::from(dir)) {
            let mut found: Vec<PathBuf> = entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    p.is_file()
                        && p.extension()
                            .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
                })
                .collect();
            found.sort();
            pdfs.extend(found);
        }
    }

    if pdfs.is_empty() {
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut candidates = vec![repo.join("../wasm/logo.pdf")];
        // Documents from the `pdf` checkout next to this repository, when it is
        // there: small files that between them use text, vector art, images and
        // form XObjects. Encrypted and deliberately broken files are left out.
        let pdf_files = repo.join("../../pdf/files");
        for name in [
            "example.pdf",
            "pdf-sample.pdf",
            "xelatex.pdf",
            "libreoffice.pdf",
            "jpeg.pdf",
            "lossless.pdf",
            "offset.pdf",
            "formxobject.pdf",
        ] {
            candidates.push(pdf_files.join(name));
        }
        for candidate in candidates {
            if candidate.is_file() {
                pdfs.push(candidate);
            }
        }
    }

    pdfs.dedup();
    pdfs
}

/// The renderer resolves the standard 14 fonts from `$STANDARD_FONTS`; point that
/// at the repository's font directory unless the caller set it.
fn point_standard_fonts_at_the_repository() {
    if std::env::var_os("STANDARD_FONTS").is_some() {
        return;
    }
    let fonts = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fonts");
    if fonts.is_dir() {
        std::env::set_var("STANDARD_FONTS", fonts);
    }
}

fn output_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("PDF_COMPARE_OUT") {
        let dir = PathBuf::from(dir);
        let _ = std::fs::create_dir_all(&dir);
        return dir;
    }
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/pdf-compare");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn env_flag(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.is_empty() && v != "0")
}

fn env_f32(name: &str, default: f32) -> f32 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v: &f32| *v > 0.0)
        .unwrap_or(default)
}

fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

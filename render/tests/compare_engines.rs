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
//! * `PDF_COMPARE_DPI`        resolution for both engines (default 600)
//! * `PDF_COMPARE_MAX_PAGES`  pages per document (default 2)
//! * `PDF_COMPARE_ONLY`       only documents whose name contains this; handy for
//!                            looking at one page at a time
//! * `PDF_COMPARE_REQUIRE`    set to 1 to fail instead of skipping when the external
//!                            engine is missing
//! * `PDF_COMPARE_OUT`        where the comparison images go
//!                            (default `<repo>/target/pdf-compare`)
//!
//! For every page three PNGs are kept (`*-ours.png`, `*-engine.png`, `*-diff.png`)
//! so the difference can be inspected by eye, plus an overlay that puts the
//! reference in the red channel, our page in the blue channel and the darker of
//! the two in green: white ink is where both engines agree.
//!
//! Pages that differ by more than one percent of their pixels get a diagnosis in
//! the report: the shift that would line the two pages up best, the same per
//! text line and per vertical strip, and the three worst areas cropped out
//! (`*spotK.png`). A shift that is the same all over the page is a placement
//! problem, one that grows along a line is an advance or scaling problem, and a
//! page that matches nowhere is a glyph or colour problem.
//!
//! Note that documents whose fonts are *not* embedded (`example.pdf`,
//! `pdf-sample.pdf`) cannot match: both engines substitute their own clone of
//! Times, Arial or Symbol, and clones differ in advance widths and glyph shapes.
//! Theirs are the largest differences in the default corpus and they are the
//! floor, not a bug.
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
const DEFAULT_DPI: f32 = 600.0;
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
    let only = std::env::var("PDF_COMPARE_ONLY")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase());
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
        if let Some(filter) = &only {
            if !stem.to_lowercase().contains(filter.as_str()) {
                continue;
            }
        }
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
            let overlay = overlay(&ours, &theirs);
            save(&prefix.with_extension("overlay.png"), &overlay);
            // Coarse whole-page shift: tells a page-wide offset from differences
            // that are local to one line or object.
            let (small_ours, small_theirs, small) = downscale(&ours, &theirs, 4);
            let (pdx, pdy, pshifted, punshifted) =
                best_shift(&small_ours, &small_theirs, (0, 0, small.0, small.1), 8);
            let line = format!(
                "  page-wide shift: ({:+}, {:+}) px, {:.2}% left vs {:.2}% unshifted",
                pdx * 4,
                pdy * 4,
                pshifted * 100.0,
                punshifted * 100.0,
            );
            println!("{line}");
            lines.push(line);
            // Detail for pages that are visibly off; clean pages stay quiet.
            if report.outlier_fraction > 0.01 {
                for line in worst_lines(&ours, &theirs, 3) {
                    println!("{line}");
                    lines.push(line);
                }
                for line in strip_shifts(&small_ours, &small_theirs, 8, 4, small) {
                    println!("{line}");
                    lines.push(line);
                }
            }
            for (n, spot) in report.hotspots.iter().enumerate() {
                if spot.count < 200 {
                    continue;
                }
                let region = spot.crop(overlay.width(), overlay.height());
                let suffix = format!("spot{}", n + 1);
                let (dx, dy, shifted, unshifted) = best_shift(&ours, &theirs, region, 32);
                save(&prefix.with_extension(format!("{suffix}.png")), &crop(&overlay, region));
                save(
                    &prefix.with_extension(format!("{suffix}.ours.png")),
                    &crop(&ours, region),
                );
                save(
                    &prefix.with_extension(format!("{suffix}.engine.png")),
                    &crop(&theirs, region),
                );
                let line = format!(
                    "  hotspot {suffix}: {}x{} at ({}, {}), {} px, ours shifted by \
                     ({dx:+}, {dy:+}) px, {:.2}% left vs {:.2}% unshifted  -> {}",
                    spot.width(),
                    spot.height(),
                    spot.x0,
                    spot.y0,
                    spot.count,
                    shifted * 100.0,
                    unshifted * 100.0,
                    prefix.with_extension(format!("{suffix}.png")).display(),
                );
                println!("{line}");
                lines.push(line);
            }
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
            let _ = write_report(&out_dir, &engine, dpi, &lines);
        }
    }

    assert!(pages_compared > 0, "no page could be compared: {failures:#?}");
    match write_report(&out_dir, &engine, dpi, &lines) {
        Ok(path) => eprintln!("comparison report: {}", path.display()),
        Err(e) => eprintln!("cannot write the comparison report: {e}"),
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

/// Writes the report, rewritten after every page so a run that is interrupted
/// still leaves the pages it got through.
fn write_report(
    out_dir: &Path,
    engine: &Path,
    dpi: f32,
    lines: &[String],
) -> std::io::Result<PathBuf> {
    let header = format!("external engine: {}\ndpi: {dpi}\n\n", engine.display());
    let path = out_dir.join("report.txt");
    std::fs::create_dir_all(out_dir)?;
    std::fs::write(&path, format!("{header}{}\n", lines.join("\n")))?;
    Ok(path)
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
    /// Where the differing pixels are, worst first: bounding box and count.
    hotspots: Vec<Hotspot>,
}

/// A cluster of differing pixels, as a bounding box in page pixels.
struct Hotspot {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
    count: u64,
}

impl Hotspot {
    fn width(&self) -> u32 {
        self.x1 - self.x0
    }

    fn height(&self) -> u32 {
        self.y1 - self.y0
    }

    /// The hotspot grown to a square of at most `MAX_CROP` pixels, centred on
    /// the differing pixels and clipped to the page: small enough to look at.
    fn crop(&self, page_width: u32, page_height: u32) -> (u32, u32, u32, u32) {
        const MAX_CROP: u32 = 480;
        const MIN_CROP: u32 = 160;
        let side = self
            .width()
            .max(self.height())
            .saturating_add(32)
            .clamp(MIN_CROP, MAX_CROP)
            .min(page_width.max(1))
            .min(page_height.max(1));
        let cx = (self.x0 + self.x1) / 2;
        let cy = (self.y0 + self.y1) / 2;
        let x0 = cx.saturating_sub(side / 2).min(page_width.saturating_sub(side));
        let y0 = cy.saturating_sub(side / 2).min(page_height.saturating_sub(side));
        (x0, y0, side.min(page_width - x0), side.min(page_height - y0))
    }
}

/// Groups differing pixels into clusters on a coarse grid, worst first.
fn hotspots(mask: &[bool], width: u32, height: u32, count: usize) -> Vec<Hotspot> {
    const CELL: u32 = 48;
    const NEIGHBOURS: i64 = 1;
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let cols = width.div_ceil(CELL);
    let rows = height.div_ceil(CELL);
    let mut per_cell = vec![0u64; (cols * rows) as usize];
    for y in 0..height {
        for x in 0..width {
            if mask[(y * width + x) as usize] {
                per_cell[((y / CELL) * cols + x / CELL) as usize] += 1;
            }
        }
    }
    let mut cells: Vec<(u64, u32, u32)> = per_cell
        .iter()
        .enumerate()
        .filter(|(_, n)| **n > 0)
        .map(|(i, n)| (*n, (i as u32) % cols, (i as u32) / cols))
        .collect();
    cells.sort_unstable_by(|a, b| b.0.cmp(&a.0));

    let mut out: Vec<Hotspot> = Vec::new();
    for (_, cx, cy) in cells {
        if out.len() >= count {
            break;
        }
        // Skip a cell that is part of a cluster already taken.
        if out.iter().any(|h| {
            let ncx = (h.x0 / CELL) as i64;
            let ncy = (h.y0 / CELL) as i64;
            (cx as i64) >= ncx - NEIGHBOURS
                && (cx as i64) <= (h.x1 / CELL) as i64 + NEIGHBOURS
                && (cy as i64) >= ncy - NEIGHBOURS
                && (cy as i64) <= (h.y1 / CELL) as i64 + NEIGHBOURS
        }) {
            continue;
        }
        let x0 = (cx.saturating_sub(1) * CELL).min(width - 1);
        let y0 = (cy.saturating_sub(1) * CELL).min(height - 1);
        let x1 = ((cx + 2) * CELL).min(width);
        let y1 = ((cy + 2) * CELL).min(height);
        let mut n = 0u64;
        for y in y0..y1 {
            for x in x0..x1 {
                if mask[(y * width + x) as usize] {
                    n += 1;
                }
            }
        }
        if n > 0 {
            out.push(Hotspot {
                x0,
                y0,
                x1,
                y1,
                count: n,
            });
        }
        let _ = count;
    }
    out
}

/// Sebastian's comparison overlay: the reference page in the red channel, our
/// page in the blue channel, and the smaller of the two in green, so ink that
/// both engines agree on comes out white, ink only the reference has red, ink
/// only we have blue.
fn overlay(ours: &RgbaImage, theirs: &RgbaImage) -> RgbaImage {
    let width = ours.width().min(theirs.width());
    let height = ours.height().min(theirs.height());
    let mut image = RgbaImage::new(width, height);
    for y in 0..height {
        for x in 0..width {
            let po = ours.get_pixel(x, y);
            let pt = theirs.get_pixel(x, y);
            let r = 255 - luminance(pt);
            let b = 255 - luminance(po);
            image.put_pixel(x, y, image::Rgba([r, r.min(b), b, 255]));
        }
    }
    image
}

fn luminance(p: &image::Rgba<u8>) -> u8 {
    ((u32::from(p[0]) * 299 + u32::from(p[1]) * 587 + u32::from(p[2]) * 114) / 1000) as u8
}

/// Copy a region out of an image, clamped to what exists.
fn crop(image: &RgbaImage, region: (u32, u32, u32, u32)) -> RgbaImage {
    let (x0, y0, w, h) = region;
    let w = w.min(image.width().saturating_sub(x0));
    let h = h.min(image.height().saturating_sub(y0));
    image::imageops::crop_imm(image, x0, y0, w, h).to_image()
}

/// Ink rows of one image, as bands of consecutive non-empty rows.
fn ink_bands(image: &RgbaImage, min_height: u32, max_height: u32) -> Vec<(u32, u32)> {
    let mut bands = Vec::new();
    let mut start: Option<u32> = None;
    let mut last_ink = 0u32;
    for y in 0..image.height() {
        let inked = (0..image.width()).any(|x| luminance(image.get_pixel(x, y)) < 200);
        if inked {
            if start.is_none() {
                start = Some(y);
            }
            last_ink = y;
        } else if let Some(y0) = start {
            if y >= last_ink + 3 {
                let height = last_ink + 1 - y0;
                if (min_height..=max_height).contains(&height) {
                    bands.push((y0, last_ink + 1));
                }
                start = None;
            }
        }
    }
    if let Some(y0) = start {
        let height = last_ink + 1 - y0;
        if (min_height..=max_height).contains(&height) {
            bands.push((y0, last_ink + 1));
        }
    }
    bands
}

/// Horizontal shift that best matches `ours` to `theirs` inside `region`,
/// searching x only (text lines are the interesting case, and a y shift would
/// just find the neighbouring line).
fn line_dx(
    ours: &RgbaImage,
    theirs: &RgbaImage,
    region: (u32, u32, u32, u32),
    max_shift: i32,
) -> (i32, f64, f64) {
    let (x0, y0, w, h) = region;
    let (x0, y0) = (x0 as i32, y0 as i32);
    let (w, h) = (w as i32, h as i32);
    let score = |dx: i32| -> f64 {
        let mut sum = 0f64;
        let mut n = 0f64;
        for y in y0..(y0 + h) {
            for x in (x0 + max_shift)..(x0 + w - max_shift) {
                let sx = x + dx;
                if sx < 0 || y < 0 || x < 0 {
                    continue;
                }
                let (sx, y_u) = (sx as u32, y as u32);
                let (px, py) = (x as u32, y_u);
                if sx >= ours.width()
                    || y_u >= ours.height()
                    || px >= theirs.width()
                    || py >= theirs.height()
                {
                    continue;
                }
                let d = i32::from(luminance(ours.get_pixel(sx, y_u)))
                    - i32::from(luminance(theirs.get_pixel(px, py)));
                sum += f64::from(d.abs());
                n += 1.0;
            }
        }
        if n == 0.0 { f64::INFINITY } else { sum / (n * 255.0) }
    };
    let unshifted = score(0);
    let (mut best_dx, mut best) = (0, unshifted);
    for dx in (-max_shift..=max_shift).step_by(2) {
        let s = score(dx);
        if s < best {
            (best_dx, best) = (dx, s);
        }
    }
    for dx in (best_dx - 1)..=(best_dx + 1) {
        let s = score(dx);
        if s < best {
            (best_dx, best) = (dx, s);
        }
    }
    (best_dx, best, unshifted)
}

/// The three text lines of a page whose horizontal placement differs most.
fn worst_lines(ours: &RgbaImage, theirs: &RgbaImage, count: usize) -> Vec<String> {
    let mut bands = ink_bands(ours, 8, 600);
    bands.extend(ink_bands(theirs, 8, 600));
    bands.sort_unstable();
    bands.dedup();
    if bands.is_empty() {
        // Fall back to equal slices: the page did not separate into lines.
        let step = (ours.height() / 20).max(16);
        let mut y = 0;
        while y + step <= ours.height() {
            bands.push((y, y + step));
            y += step;
        }
    }
    let mut found: Vec<(f64, String)> = Vec::new();
    for (y0, y1) in bands {
        let height = y1 - y0;
        let max_shift = ((height / 2) as i32 - 4).clamp(2, 40);
        let (dx, shifted, unshifted) =
            line_dx(ours, theirs, (0, y0, ours.width(), height), max_shift);
        if unshifted < 0.005 {
            continue;
        }
        found.push((
            unshifted,
            format!(
                "  line y {:>4}-{:<4}: ours shifted by {dx:+} px, {:.2}% left vs {:.2}% unshifted",
                y0,
                y1,
                shifted * 100.0,
                unshifted * 100.0
            ),
        ));
    }
    found.sort_by(|a, b| b.0.total_cmp(&a.0));
    found.into_iter().take(count).map(|(_, line)| line).collect()
}

/// Shifts estimated separately for vertical strips of the page: a shift that
/// grows from left to right is a horizontal scale error, one that is the same
/// everywhere is a translation, and one that comes and goes is local content.
fn strip_shifts(
    ours: &RgbaImage,
    theirs: &RgbaImage,
    strips: u32,
    factor: u32,
    small: (u32, u32),
) -> Vec<String> {
    let (width, height) = small;
    let mut out = Vec::new();
    for i in 0..strips {
        let x0 = i * width / strips;
        let x1 = (i + 1) * width / strips;
        let (dx, dy, shifted, unshifted) = best_shift(ours, theirs, (x0, 0, x1 - x0, height), 8);
        out.push(format!(
            "  strip {:>2}: x {:>4}-{:<4}  shift ({:+}, {:+}) px  {:.2}% left vs {:.2}% unshifted",
            i + 1,
            x0 * factor,
            x1 * factor,
            dx * factor as i32,
            dy * factor as i32,
            shifted * 100.0,
            unshifted * 100.0,
        ));
    }
    out
}

/// Downscale both images by `factor` to the same smaller size, for the coarse
/// whole-page shift estimate (the shift is searched in small pixels).
fn downscale(a: &RgbaImage, b: &RgbaImage, factor: u32) -> (RgbaImage, RgbaImage, (u32, u32)) {
    let width = (a.width().min(b.width()) / factor).max(1);
    let height = (a.height().min(b.height()) / factor).max(1);
    let resize = |image: &RgbaImage| {
        image::imageops::resize(image, width, height, image::imageops::FilterType::Triangle)
    };
    (resize(a), resize(b), (width, height))
}

/// The shift of `ours` against `theirs` (page pixels, `+x` right, `+y` down)
/// that makes `region` agree best, with the remaining mean difference there
/// and the same figure for no shift at all.  A shift that is the same across
/// the page is a positioning error; one that grows along a line is a scaling
/// error.
fn best_shift(
    ours: &RgbaImage,
    theirs: &RgbaImage,
    region: (u32, u32, u32, u32),
    max_shift: i32,
) -> (i32, i32, f64, f64) {
    let (x0, y0, w, h) = region;
    let (x0, y0) = (x0 as i32, y0 as i32);
    let (w, h) = (w as i32, h as i32);
    let score = |dx: i32, dy: i32| -> f64 {
        let mut sum = 0f64;
        let mut n = 0f64;
        for y in (y0 + max_shift)..(y0 + h - max_shift) {
            for x in (x0 + max_shift)..(x0 + w - max_shift) {
                let (sx, sy) = (x + dx, y + dy);
                if sx < 0 || sy < 0 || x < 0 || y < 0 {
                    continue;
                }
                let (sx, sy) = (sx as u32, sy as u32);
                let (px, py) = (x as u32, y as u32);
                if sx >= ours.width()
                    || sy >= ours.height()
                    || px >= theirs.width()
                    || py >= theirs.height()
                {
                    continue;
                }
                let d = i32::from(luminance(ours.get_pixel(sx, sy)))
                    - i32::from(luminance(theirs.get_pixel(px, py)));
                sum += f64::from(d.abs());
                n += 1.0;
            }
        }
        if n == 0.0 { f64::INFINITY } else { sum / (n * 255.0) }
    };
    let unshifted = score(0, 0);
    // Coarse search first, then refine around the winner.
    let (mut best_dx, mut best_dy, mut best) = (0, 0, unshifted);
    for dy in (-max_shift..=max_shift).step_by(4) {
        for dx in (-max_shift..=max_shift).step_by(4) {
            let s = score(dx, dy);
            if s < best {
                (best_dx, best_dy, best) = (dx, dy, s);
            }
        }
    }
    for dy in (best_dy - 3)..=(best_dy + 3) {
        for dx in (best_dx - 3)..=(best_dx + 3) {
            let s = score(dx, dy);
            if s < best {
                (best_dx, best_dy, best) = (dx, dy, s);
            }
        }
    }
    (best_dx, best_dy, best, unshifted)
}

fn compare(a: &RgbaImage, b: &RgbaImage) -> Diff {
    let width = a.width().min(b.width());
    let height = a.height().min(b.height());

    let mut sum = 0u64;
    let mut outliers = 0u64;
    let mut diff_image = RgbaImage::new(width, height);
    let mut mask = vec![false; (width * height) as usize];

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
                mask[(y * width + x) as usize] = true;
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
        hotspots: hotspots(&mask, width, height, 3),
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

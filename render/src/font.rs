use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::Deref;
use std::path::PathBuf;

#[cfg(feature="glyphmatcher")]
use glyphmatcher::FontDb;

use log::{debug, warn};
use pdf::object::*;
use pdf::font::{Font as PdfFont};
use pdf::error::{Result, PdfError};

use font::{self, Encoder, FontVariant};
use vello_encoding::{Encoding, PathEncoder};
use std::sync::Arc;
use super::FontEntry;
use globalcache::{sync::SyncCache, ValueSize};
use std::hash::{Hash, Hasher};

pub struct FontRc<E: Encoder>(Arc<font::FontVariant<E>>);
impl<E: Encoder> FontRc<E> {
    /// Identity of the font, for caches that outlive one page: the address of
    /// the shared variant. Two `FontRc`s of the same font are the same address,
    /// and it stays valid as long as any of them is alive.
    #[inline]
    pub fn address(&self) -> usize {
        Arc::as_ptr(&self.0) as usize
    }
}
impl<E: Encoder> Clone for FontRc<E> {
    #[inline]
    fn clone(&self) -> Self {
        FontRc(self.0.clone())
    }
}
impl<E: Encoder> ValueSize for FontRc<E> {
    #[inline]
    fn size(&self) -> usize {
        1 // TODO
    }
}
impl<E: Encoder> From<font::FontVariant<E>> for FontRc<E> {
    #[inline]
    fn from(f: font::FontVariant<E>) -> Self {
        FontRc(f.into())
    }
}
impl<E: Encoder> Deref for FontRc<E> {
    type Target = FontVariant<E>;
    #[inline]
    fn deref(&self) -> &Self::Target {
        &*self.0
    }
}
impl<E: Encoder> PartialEq for FontRc<E> {
    #[inline]
    fn eq(&self, rhs: &Self) -> bool {
        Arc::as_ptr(&self.0) == Arc::as_ptr(&rhs.0)
    }
}
impl<E: Encoder> Eq for FontRc<E> {}
impl<E: Encoder> Hash for FontRc<E> {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.0).hash(state)
    }
}

pub struct StandardCache<E: Encoder> {
    inner: Arc<SyncCache<String, Option<FontRc<E>>>>,

    /// Where the standard fonts come from: a directory on disk, the embedded
    /// copies, or a bundle the host fetched for us.
    dir: Box<dyn DirRead + Send + Sync>,

    fonts: HashMap<String, String>,
    dump: Dump,

    #[cfg(feature="glyphmatcher")]
    font_db: Option<FontDb>,

    require_unique_unicode: bool,
}

impl<E: Encoder + 'static> StandardCache<E> where E::GlyphRef: Sync + Send {
    /// The standard fonts as a file tree: a directory on disk, the embedded
    /// copies, or a bundle the host fetched.
    ///
    /// `dir` must contain `fonts.json` and the files it names. Files are read
    /// on demand, but *all* of them are expected to be present: the browser
    /// fetches the whole set as one resource up front, so which fonts a
    /// document actually uses cannot be observed from the outside.
    pub fn with_dir(dir: impl DirRead + Send + Sync + 'static) -> Self {
        let dir: Box<dyn DirRead + Send + Sync> = Box::new(dir);
        let fonts = Self::index(&*dir);

        // The font database needs a real directory; `new` fills it in.
        #[cfg(feature="glyphmatcher")]
        let font_db: Option<FontDb> = None;

        StandardCache {
            inner: SyncCache::new(),
            dir,
            fonts,
            dump: Dump::Never,
            #[cfg(feature="glyphmatcher")]
            font_db,
            require_unique_unicode: false,
        }
    }

    /// `$STANDARD_FONTS` from the environment, plus the `DUMP_FONT` and system
    /// font database hooks, which only make sense where there is a filesystem.
    ///
    /// Not available on wasm: there the bundle arrives over the network, see
    /// [`with_dir`](Self::with_dir).
    #[cfg(all(feature="native", not(feature="embed")))]
    pub fn new() -> Self {
        let standard_fonts = PathBuf::from(std::env::var_os("STANDARD_FONTS").expect("STANDARD_FONTS is not set. Please check https://github.com/pdf-rs/pdf_render/#fonts for instructions."));
        let mut cache = Self::with_dir(standard_fonts.clone());

        cache.dump = match std::env::var("DUMP_FONT").as_deref() {
            Err(_) => Dump::Never,
            Ok("always") => Dump::Always,
            Ok("error") => Dump::OnError,
            Ok(_) => Dump::Never
        };
        #[cfg(feature="glyphmatcher")]
        {
            let db_path = standard_fonts.join("db");
            cache.font_db = db_path.is_dir().then(|| FontDb::new(db_path));
        }
        cache
    }

    #[cfg(feature="embed")]
    pub fn new() -> Self {
        Self::with_dir(EmbeddedStandardFonts)
    }

    /// Nothing yet: no fonts until the host hands over the bundle, see
    /// [`set_dir`](Self::set_dir).
    ///
    /// Parsing does not need fonts, only text does, so a viewer can open the
    /// document and render the first page while the bundle is still in flight.
    #[cfg(not(any(feature="native", feature="embed")))]
    pub fn new() -> Self {
        StandardCache {
            inner: SyncCache::new(),
            dir: Box::new(MemDir::new([])),
            fonts: HashMap::new(),
            dump: Dump::Never,
            #[cfg(feature="glyphmatcher")]
            font_db: None,
            require_unique_unicode: false,
        }
    }

    /// Whether a font set has been handed over yet.
    pub fn has_fonts(&self) -> bool {
        !self.fonts.is_empty()
    }

    /// Hand over the font bundle, for a host that starts without fonts.
    ///
    /// Everything derived from the old set is dropped: a font that was looked
    /// up while the set was empty has to be looked up again.
    pub fn set_dir(&mut self, dir: impl DirRead + Send + Sync + 'static) {
        let dir: Box<dyn DirRead + Send + Sync> = Box::new(dir);
        self.fonts = Self::index(&*dir);
        self.dir = dir;
        self.inner = SyncCache::new();
        #[cfg(feature="glyphmatcher")]
        {
            self.font_db = None;
        }
    }

    fn index(dir: &(dyn DirRead + Send + Sync)) -> HashMap<String, String> {
        let data = dir.read_file("fonts.json").expect("can't read fonts.json");
        serde_json::from_slice(&data).expect("fonts.json is invalid")
    }

    pub fn require_unique_unicode(&mut self, r: bool) {
        self.require_unique_unicode = r;
    }
}

/// A set of files the standard fonts can be read from, by name.
///
/// Object safe on purpose: [`StandardCache`] holds a boxed one, so a directory
/// on disk, the embedded copies and a bundle fetched over the network are the
/// same thing to the renderer.
pub trait DirRead {
    fn read_file(&self, name: &str) -> Result<Arc<[u8]>>;
    fn sub_dir(&self, name: &str) -> Option<Box<dyn DirRead + Send + Sync>>;
}

impl DirRead for PathBuf {
    fn read_file(&self, name: &str) -> Result<Arc<[u8]>> {
        std::fs::read(self.join(name)).map_err(|e| e.into()).map(Arc::from)
    }
    fn sub_dir(&self, name: &str) -> Option<Box<dyn DirRead + Send + Sync>> {
        let sub = self.join(name);
        if sub.is_dir() {
            Some(Box::new(sub))
        } else {
            None
        }
    }
}

/// A directory that lives in memory: what the browser gets when the standard
/// fonts arrive as a separate resource.
///
/// The whole set is fetched before the first page is parsed — one request, not
/// one per font — so the network cannot be watched to learn which fonts a
/// document uses. Names here are `/`-separated and relative, exactly as a
/// `PathBuf` would resolve them.
#[derive(Default, Clone)]
pub struct MemDir {
    files: HashMap<String, Arc<[u8]>>,
}

impl MemDir {
    pub fn new(files: impl IntoIterator<Item = (String, Arc<[u8]>)>) -> Self {
        MemDir { files: files.into_iter().collect() }
    }

    pub fn insert(&mut self, name: impl Into<String>, data: impl Into<Arc<[u8]>>) {
        self.files.insert(name.into(), data.into());
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Bytes held, i.e. how much the font resource cost.
    pub fn bytes(&self) -> usize {
        self.files.values().map(|d| d.len()).sum()
    }

    /// Read a bundle: a tiny table of contents, then the files.
    ///
    /// Version 1 is plain:
    ///
    /// ```text
    /// "PDFF" 1 (u8)  count (u32 LE)
    /// count × { name_len (u16 LE), name (utf8), offset (u32 LE), len (u32 LE) }
    /// data, at `offset`, from the start of the bundle
    /// ```
    ///
    /// Offsets are absolute so the host can build the file by concatenating
    /// pieces in one pass; the header is small enough that the browser may as
    /// well receive it first, but it does not have to.
    ///
    /// Version 2 is that whole file compressed as one stream, for a host that
    /// cannot say `Content-Encoding` on the response:
    ///
    /// ```text
    /// "PDFF" 2 (u8)  codec (u8)  raw_len (u32 LE)  <raw_len bytes of version 1>
    /// ```
    ///
    /// A browser is normally sent the compressed stream on its own, as the body
    /// of a response whose header says `Content-Encoding: br`, and hands this
    /// function version 1 — the framing would only repeat what HTTP already
    /// said, and the module carries no decoder to read it with.
    ///
    /// One stream for all the fonts, not one per file. They share an alphabet,
    /// a set of outline operators and, between the weights of one family, a lot
    /// of outlines. Fonts are compressed one file at a time only because a font
    /// has to be readable on its own; a bundle does not, since every font in it
    /// is fetched ([`with_dir`](StandardCache::with_dir)).
    ///
    /// What that is worth depends on the window: with gzip's 32 KiB — smaller
    /// than one font — a bundle saves nothing (838,943 bytes against 834,200
    /// for the files gzipped one at a time). With brotli's 4 MiB, which covers
    /// the whole set, it is 547,313 against 700,389: a quarter off, for one
    /// request instead of fifteen.
    ///
    /// A `raw_len` that disagrees with the stream stops the read: a font bundle
    /// that silently loses bytes would render wrong glyphs.
    pub fn parse_bundle(data: &[u8]) -> Result<MemDir> {
        let raw: Vec<u8>;
        let mut data = data;
        if data.get(..4) == Some(&b"PDFF"[..]) && data.get(4) == Some(&2) {
            let bad = |what: &str| PdfError::Other { msg: format!("font bundle: {what}") };
            let codec = data.get(5).and_then(|b| Codec::from_u8(*b)).ok_or_else(|| bad("unknown codec"))?;
            let len = u32::from_le_bytes(data.get(6..10).ok_or_else(|| bad("truncated"))?.try_into().unwrap()) as usize;
            let body = data.get(10..).ok_or_else(|| bad("truncated"))?;
            raw = codec.decompress(body, len)?;
            data = &raw;
        }

        let bad = |what: &str| PdfError::Other { msg: format!("font bundle: {what}") };
        let mut pos = 0usize;
        let take = |pos: &mut usize, n: usize| -> Result<&[u8]> {
            let end = pos.checked_add(n).ok_or_else(|| PdfError::Other { msg: "font bundle: overflow".into() })?;
            let s = data.get(*pos..end).ok_or_else(|| PdfError::Other { msg: "font bundle: truncated".into() })?;
            *pos = end;
            Ok(s)
        };
        if take(&mut pos, 4)? != b"PDFF" {
            return Err(bad("not a font bundle"));
        }
        let version = take(&mut pos, 1)?[0];
        if version != 1 {
            return Err(bad(&format!("unsupported version {version}")));
        }
        let count = u32::from_le_bytes(take(&mut pos, 4)?.try_into().unwrap()) as usize;
        let mut files = HashMap::with_capacity(count);
        for _ in 0..count {
            let name_len = u16::from_le_bytes(take(&mut pos, 2)?.try_into().unwrap()) as usize;
            let name = std::str::from_utf8(take(&mut pos, name_len)?).map_err(|_| bad("file name is not utf8"))?.to_string();
            let offset = u32::from_le_bytes(take(&mut pos, 4)?.try_into().unwrap()) as usize;
            let len = u32::from_le_bytes(take(&mut pos, 4)?.try_into().unwrap()) as usize;
            let end = offset.checked_add(len).ok_or_else(|| bad("overflow"))?;
            let bytes = data.get(offset..end).ok_or_else(|| bad("file outside the bundle"))?;
            files.insert(name, Arc::from(bytes));
        }
        Ok(MemDir { files })
    }

    /// This set as a plain bundle (version 1): a table of contents, then the
    /// files, all readable in place.
    pub fn write_plain(&self) -> Vec<u8> {
        let mut names: Vec<&String> = self.files.keys().collect();
        names.sort();

        let toc_len: usize = names.iter().map(|n| 2 + n.len() + 4 + 4).sum();
        let mut offset = 4 + 1 + 4 + toc_len;

        let mut out = Vec::with_capacity(offset + self.bytes());
        out.extend_from_slice(b"PDFF");
        out.push(1);
        out.extend_from_slice(&(names.len() as u32).to_le_bytes());
        for name in &names {
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            let data = &self.files[*name];
            out.extend_from_slice(&(offset as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            offset += data.len();
        }
        for name in &names {
            out.extend_from_slice(&self.files[*name]);
        }
        out
    }

    /// This set as a bundle in `codec` (version 2), for the network.
    ///
    /// The plain file is the input: compress what is read back, so there is
    /// only one writer of the layout.
    pub fn write_bundle(&self, codec: Codec) -> Vec<u8> {
        let plain = self.write_plain();
        let body = codec.compress(&plain);
        let mut out = Vec::with_capacity(10 + body.len());
        out.extend_from_slice(b"PDFF");
        out.push(2);
        out.push(codec as u8);
        out.extend_from_slice(&(plain.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out
    }
}

/// How the payload of a version 2 bundle is compressed.
///
/// What a browser is sent is not a version 2 bundle at all: the payload is the
/// response body and the header says `Content-Encoding: br`, so the network
/// layer inflates it and [`parse_bundle`](MemDir::parse_bundle) sees the plain
/// bundle — no decoder in the module, and the bytes are served as they were
/// built instead of compressed per request. `fonts_blob` writes that payload
/// (`standard_fonts.br`) next to the framed bundles.
///
/// The framing still earns its place where HTTP cannot say it: a bundle inside
/// an archive, in a test that reads the file directly, or a host that can only
/// set `Content-Encoding: gzip` — `Codec::Gzip` and `Codec::Deflate` exist for
/// those, and decodable by any pdf build. Brotli inside the framing needs the
/// `bundle-write` feature, since nothing in the browser path would.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    Store = 0,
    Brotli = 1,
    Deflate = 2,
    Gzip = 3,
}

impl Codec {
    pub fn from_u8(v: u8) -> Option<Codec> {
        Some(match v {
            0 => Codec::Store,
            1 => Codec::Brotli,
            2 => Codec::Deflate,
            3 => Codec::Gzip,
            _ => return None,
        })
    }

    fn decompress(self, src: &[u8], raw_len: usize) -> Result<Vec<u8>> {
        use std::io::Read;
        let mut out = Vec::with_capacity(raw_len);
        let r = match self {
            Codec::Store => {
                out.extend_from_slice(src);
                return Ok(out);
            }
            Codec::Brotli => Self::brotli_decompress(src, &mut out),
            Codec::Deflate => flate2::read::DeflateDecoder::new(src).read_to_end(&mut out).map(|_| ()),
            Codec::Gzip => flate2::read::GzDecoder::new(src).read_to_end(&mut out).map(|_| ()),
        };
        r.map_err(|e| PdfError::Other { msg: format!("font bundle: {self:?}: {e}") })?;
        if out.len() != raw_len {
            return Err(PdfError::Other {
                msg: format!("font bundle: {self:?}: {} bytes, expected {raw_len}", out.len()),
            });
        }
        Ok(out)
    }

    /// Compress `src` with this codec.
    ///
    /// Only brotli needs a dependency a wasm module would not otherwise carry,
    /// so only brotli is behind a feature; the rest is flate2, which a pdf
    /// build links anyway.
    fn compress(self, src: &[u8]) -> Vec<u8> {
        use std::io::Write;
        match self {
            Codec::Store => src.to_vec(),
            Codec::Brotli => Self::brotli(src),
            Codec::Deflate => {
                let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
                e.write_all(src).expect("deflate");
                e.finish().expect("deflate")
            }
            Codec::Gzip => {
                let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
                e.write_all(src).expect("gzip");
                e.finish().expect("gzip")
            }
        }
    }

    /// Decode a brotli bundle payload.
    ///
    /// A browser never needs this: the payload is what the server sends as the
    /// response body, and `Content-Encoding: br` has the network layer inflate
    /// it before wasm is handed a byte. The decoder is therefore build tooling
    /// (it reads back what `write_bundle` wrote), and leaving it out keeps 177 kB
    /// of module — 135 kB of that brotli's static dictionary — out of the wasm.
    #[cfg(feature = "bundle-write")]
    fn brotli_decompress(src: &[u8], out: &mut Vec<u8>) -> std::io::Result<()> {
        brotli_decompressor::BrotliDecompress(&mut &src[..], out)
    }

    #[cfg(not(feature = "bundle-write"))]
    fn brotli_decompress(_src: &[u8], _out: &mut Vec<u8>) -> std::io::Result<()> {
        // Not a failure of the data: this build has no brotli. Saying which
        // bundle to send instead beats a decoder that is not there.
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "no brotli decoder in this build: send the plain bundle with `Content-Encoding: br`, or a gzip one",
        ))
    }

    #[cfg(feature = "bundle-write")]
    fn brotli(src: &[u8]) -> Vec<u8> {
        // Quality 11, window 22 (4 MiB, more than a font set needs): the blob
        // is built once and fetched by every viewer.
        let mut params = brotli::enc::BrotliEncoderParams::default();
        params.quality = 11;
        params.lgwin = 22;
        let mut out = Vec::new();
        brotli::BrotliCompress(&mut &src[..], &mut out, &params).expect("brotli");
        out
    }

    #[cfg(not(feature = "bundle-write"))]
    fn brotli(_src: &[u8]) -> Vec<u8> {
        panic!("a brotli bundle needs the `bundle-write` feature")
    }
}

impl DirRead for MemDir {
    fn read_file(&self, name: &str) -> Result<Arc<[u8]>> {
        self.files
            .get(name)
            .cloned()
            .ok_or_else(|| PdfError::Other { msg: format!("font bundle has no {name:?}") })
    }
    fn sub_dir(&self, name: &str) -> Option<Box<dyn DirRead + Send + Sync>> {
        let prefix = format!("{name}/");
        let files: HashMap<String, Arc<[u8]>> = self
            .files
            .iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .map(|(k, v)| (k[prefix.len()..].to_string(), v.clone()))
            .collect();
        if files.is_empty() {
            None
        } else {
            Some(Box::new(MemDir { files }))
        }
    }
}

#[cfg(feature="embed")]
#[derive(rust_embed::Embed)]
#[folder = "$STANDARD_FONTS"]
pub struct EmbeddedStandardFonts;

#[cfg(feature="embed")]
impl DirRead for EmbeddedStandardFonts {
    fn read_file(&self, name: &str) -> Result<Arc<[u8]>> {
        EmbeddedStandardFonts::get(name)
            .map(|f| Arc::from(f.data.into_owned()))
            .ok_or_else(|| PdfError::Other { msg: format!("file {name:?} not embedded") })
    }
    fn sub_dir(&self, name: &str) -> Option<Box<dyn DirRead + Send + Sync>> {
        None
    }
}

#[derive(Debug)]
enum Dump {
    Never,
    OnError,
    Always
}

pub struct GlyphData {
    encoding: vello_encoding::Encoding,
    offsets: Vec<Offset>
}
struct Offset {
    path_tag: usize,
    path_data: usize,
    n_path_segments: u32,
}
impl GlyphData {
    pub fn new() -> Self {
        GlyphData { encoding: Encoding::new(), offsets: vec![] }
    }
}
impl font::Encoder for GlyphData {
    type Pen<'a> = PathEncoder<'a>;
    type GlyphRef = u32;
    fn encode_shape<'f, O, E>(&mut self, mut f: impl for<'b> FnMut(&mut Self::Pen<'b>) -> Result<O, E> + 'f) -> Result<(O, Self::GlyphRef), E> {
        let mut p: PathEncoder = self.encoding.encode_path(true);
        let o = f(&mut p)?;
        p.finish(true);
        self.offsets.push(Offset {
            path_tag: self.encoding.path_tags.len(),
            path_data: self.encoding.path_data.len(),
            n_path_segments: self.encoding.n_path_segments,
        });

        Ok((o, self.encoding.n_paths))
    }
}

pub fn load_font<E: Encoder + 'static>(encoder: &mut E, font_ref: &MaybeRef<PdfFont>, resolve: &impl Resolve, cache: &StandardCache<E>) -> Result<Option<FontEntry<E>>>
    where FontRc<E>: Send
{
    let pdf_font = font_ref.clone();
    debug!("loading {:?}", pdf_font);

    let font: FontRc<E> = match pdf_font.embedded_data(resolve) {
        Some(Ok(data)) => {
            debug!("loading embedded font");
            let font = font::parse(&data, encoder).map_err(|e| {
                PdfError::Other { msg: format!("Font Error: {:?}", e) }
            });
            #[cfg(feature = "native")]
            if matches!(cache.dump, Dump::Always) || (matches!(cache.dump, Dump::OnError) && font.is_err()) {
                let name = format!("font_{}", pdf_font.name.as_ref().map(|s| s.as_str()).unwrap_or("unnamed"));
                std::fs::write(&name, &data[..]).unwrap();
                println!("font dumped in {}", name);
            }
            font?.into()
        }
        Some(Err(e)) => return Err(e),
        None => {
            debug!("no embedded font.");
            let name = match pdf_font.name {
                Some(ref name) => name.as_str(),
                None => return Ok(None)
            };
            debug!("loading {name} instead");
            match cache.fonts.get(name).or_else(|| cache.fonts.get("Arial")) {
                Some(file_name) => {
                    let val = cache.inner.get(file_name.clone(), |_| {
                        let data = match cache.dir.read_file(file_name) {
                            Ok(data) => data,
                            Err(e) => {
                                warn!("can't open {} for {:?} {:?}", file_name, pdf_font.name, e);
                                return None;
                            }
                        };
                        match font::parse(&data, encoder) {
                            Ok(f) => Some(f.into()),
                            Err(e) => {
                                warn!("Font Error: {:?}", e);
                                return None;
                            }
                        }
                    });
                    match val {
                        Some(f) => f,
                        None => {
                            return Ok(None);
                        }
                    }
                }
                None => {
                    warn!("no font for {:?}", pdf_font.name);
                    return Ok(None);
                }
            }
        }
    };

    Ok(Some(FontEntry::build(font, pdf_font,
        #[cfg(feature="glyphmatcher")] cache.font_db.as_ref(),
        resolve, cache.require_unique_unicode)?))
}

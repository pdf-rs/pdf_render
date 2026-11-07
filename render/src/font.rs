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
    /// ```text
    /// "PDFF" 1 (u8)  count (u32 LE)
    /// count × { name_len (u16 LE), name (utf8), offset (u32 LE), len (u32 LE) }
    /// data, at `offset`, from the start of the bundle
    /// ```
    ///
    /// Offsets are absolute so the host can build the file by concatenating
    /// pieces in one pass; the header is small enough that the browser may as
    /// well receive it first, but it does not have to.
    pub fn parse_bundle(data: &Arc<[u8]>) -> Result<MemDir> {
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
        if take(&mut pos, 1)?[0] != 1 {
            return Err(bad("unsupported version"));
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

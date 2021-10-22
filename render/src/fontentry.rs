use std::collections::HashMap;
use font::{self, Font, GlyphId};
use pdf::encoding::BaseEncoding;
use pdf::font::{Font as PdfFont, Widths, ToUnicodeMap};
use pdf::object::Resolve;
use pdf::error::PdfError;
use pdf_encoding::{Encoding};


#[derive(Debug)]
pub enum TextEncoding {
    CID,
    Cmap(HashMap<u16, GlyphId>)
}

pub struct FontEntry {
    pub font: Box<dyn Font>,
    pub encoding: TextEncoding,
    pub widths: Option<Widths>,
    pub is_cid: bool,
    pub name: String,
    pub to_unicode: Option<ToUnicodeMap>,
}
impl FontEntry {
    pub fn build(font: Box<dyn Font>, pdf_font: &PdfFont, resolve: &impl Resolve) -> Result<FontEntry, PdfError> {
        let mut is_cid = pdf_font.is_cid();
        let encoding = pdf_font.encoding().clone();
        let base_encoding = encoding.as_ref().map(|e| &e.base);

        let mut to_unicode = pdf_font.to_unicode().transpose()?;

        let encoding = if let Some(map) = pdf_font.cid_to_gid_map() {
            is_cid = true;
            let cmap = map.iter().enumerate().map(|(cid, &gid)| (cid as u16, GlyphId(gid as u32))).collect();
            TextEncoding::Cmap(cmap)
        } else if base_encoding == Some(&BaseEncoding::IdentityH) {
            is_cid = true;
            TextEncoding::CID
        } else {
            let mut cmap = HashMap::new();
            let source_encoding = match base_encoding {
                Some(BaseEncoding::StandardEncoding) => Some(Encoding::AdobeStandard),
                Some(BaseEncoding::SymbolEncoding) => Some(Encoding::AdobeSymbol),
                Some(BaseEncoding::WinAnsiEncoding) => Some(Encoding::WinAnsiEncoding),
                ref e => {
                    warn!("unsupported pdf encoding {:?}", e);
                    None
                }
            };
            if let (Some(e), false) = (source_encoding, to_unicode.is_some()) {
                let decoder = e.forward_map().ok_or(PdfError::Other { msg: format!("no forward map on encoding {:?}", e)})?;
                to_unicode = Some(ToUnicodeMap::create((0..=255).filter_map(|b| decoder.get(b).map(|c| (b as u16, c.to_string())))));
            }

            let font_encoding = font.encoding();
            debug!("{:?} -> {:?}", source_encoding, font_encoding);
            match (source_encoding, font_encoding) {
                (Some(source), Some(dest)) => {
                    let transcoder = source.to(dest).expect("can't transcode");
                    
                    for b in 0 .. 256 {
                        if let Some(gid) = transcoder.translate(b).and_then(|cp| font.gid_for_codepoint(cp)) {
                            cmap.insert(b as u16, gid);
                            debug!("{} -> {:?}", b, gid);
                        }
                    }
                },
                _ => {
                    warn!("can't translate from text encoding {:?} to font encoding {:?}", base_encoding, font_encoding);
                    
                    // assuming same encoding
                    for cp in 0 .. 256 {
                        if let Some(gid) = font.gid_for_codepoint(cp) {
                            cmap.insert(cp as u16, gid);
                        }
                    }
                }
            }
            if let Some(encoding) = encoding {
                for (&cp, name) in encoding.differences.iter() {
                    debug!("{} -> {}", cp, name);
                    match font.gid_for_name(&name) {
                        Some(gid) => {
                            cmap.insert(cp as u16, gid);
                        }
                        None => info!("no glyph for name {}", name)
                    }
                }
            }
            debug!("cmap: {:?}", cmap);
            if cmap.is_empty() {
                TextEncoding::CID
            } else {
                TextEncoding::Cmap(cmap)
            }
        };
        
        let widths = pdf_font.widths(resolve)?;
        let name = pdf_font.name.as_ref().ok_or_else(|| PdfError::Other { msg: "font has no name".into() })?.clone();
        Ok(FontEntry {
            font: font,
            encoding,
            is_cid,
            widths,
            name,
            to_unicode,
        })
    }
}

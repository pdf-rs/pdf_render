use std::path::{Path, PathBuf};
use std::fs;
use std::borrow::Cow;
use pdf::object::*;
use pdf::font::{Font as PdfFont};
use pdf::error::{Result, PdfError};

use font::{self};
use std::rc::Rc;
use super::FontEntry;


pub static STANDARD_FONTS: &[(&'static str, &'static str)] = &[
    ("Courier", "CourierStd.otf"),
    ("Courier-Bold", "CourierStd-Bold.otf"),
    ("Courier-Oblique", "CourierStd-Oblique.otf"),
    ("Courier-BoldOblique", "CourierStd-BoldOblique.otf"),
    
    ("Times-Roman", "MinionPro-Regular.otf"),
    ("Times-Bold", "MinionPro-Bold.otf"),
    ("Times-Italic", "MinionPro-It.otf"),
    ("Times-BoldItalic", "MinionPro-BoldIt.otf"),
    
    ("Helvetica", "MyriadPro-Regular.otf"),
    ("Helvetica-Bold", "MyriadPro-Bold.otf"),
    ("Helvetica-Oblique", "MyriadPro-It.otf"),
    ("Helvetica-BoldOblique", "MyriadPro-BoldIt.otf"),
    
    ("Symbol", "SY______.PFB"),
    ("ZapfDingbats", "AdobePiStd.otf"),
    
    ("Arial-BoldMT", "Arial-BoldMT.otf"),
    ("ArialMT", "ArialMT.ttf"),
    ("Arial-ItalicMT", "Arial-ItalicMT.otf"),
];

pub fn load_font(font_ref: Ref<PdfFont>, resolve: &impl Resolve, standard_fonts: &Path) -> Result<Option<Rc<FontEntry>>> {
    let pdf_font = resolve.get(font_ref)?;
    debug!("loading {:?}", pdf_font);
    
    let data: Cow<[u8]> = match pdf_font.embedded_data() {
        Some(Ok(data)) => data.into(),
        Some(Err(e)) => return Err(e),
        None => {
            match STANDARD_FONTS.iter().find(|&&(name, _)| pdf_font.name.as_ref().map(|s| s == name).unwrap_or(false)) {
                Some(&(_, file_name)) => {
                    if let Ok(data) = std::fs::read(standard_fonts.join(file_name)) {
                        data.into()
                    } else {
                        warn!("can't open {} for {:?}", file_name, pdf_font.name);
                        return Ok(None);
                    }
                }
                None => {
                    warn!("no font for {:?}", pdf_font.name);
                    return Ok(None);
                }
            }
        }
    };

    let font = font::parse(&data).map_err(|e| {
        let name = format!("font_{}", pdf_font.name.as_ref().map(|s| s.as_str()).unwrap_or("unnamed"));
        std::fs::write(&name, &data).unwrap();
        println!("font dumped in {}", name);
        PdfError::Other { msg: format!("Font Error: {:?}", e) }
    })?;
    let entry = match FontEntry::build(font, pdf_font, resolve) {
        Ok(e) => Rc::new(e),
        Err(e) => {
            info!("Failed to build FontEntry: {:?}", e);
            return Ok(None);
        }
    };
    debug!("is_cid={}", entry.is_cid);
    
    Ok(Some(entry))
}

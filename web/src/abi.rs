//! The exported surface.
//!
//! Plain `extern "C"`: numbers in, pointers into the module's own linear
//! memory out, and no imports. Nothing here allocates on behalf of the host
//! without a matching `*_free`, because a leak in a page that stays open is a
//! leak in the tab.

use crate::{Doc, Ready};

/// Bumped when an export changes meaning. The page checks it before it uses
/// anything else, so a stale `.wasm` and a new `viewer.js` fail loudly.
pub const ABI: u32 = 1;

#[no_mangle]
pub extern "C" fn pdf_abi() -> u32 {
    ABI
}

/// A document of `file_len` bytes, none of them fetched yet.
#[no_mangle]
pub extern "C" fn pdf_doc_new(file_len: u32) -> *mut Doc {
    Box::into_raw(Box::new(Doc::new(file_len)))
}

#[no_mangle]
pub unsafe extern "C" fn pdf_doc_free(doc: *mut Doc) {
    if !doc.is_null() {
        drop(Box::from_raw(doc));
    }
}

/// A buffer of `len` bytes for the host to fill, e.g. with a response body.
#[no_mangle]
pub extern "C" fn pdf_buf_alloc(len: u32) -> *mut u8 {
    let bytes = vec![0u8; len as usize].into_boxed_slice();
    Box::into_raw(bytes) as *mut u8
}

/// Give back what `pdf_buf_alloc` (or `pdf_page_svg`, `pdf_error`) returned.
///
/// `len` is the length it was handed out with — 0 is a valid buffer.
#[no_mangle]
pub unsafe extern "C" fn pdf_buf_free(ptr: *mut u8, len: u32) {
    if ptr.is_null() {
        return;
    }
    drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
        ptr,
        len as usize,
    )));
}

/// Bytes that arrived for `offset`, `len` bytes long, at `ptr`.
#[no_mangle]
pub unsafe extern "C" fn pdf_supply(doc: *mut Doc, offset: u32, ptr: *const u8, len: u32) {
    let (Some(doc), Some(data)) = (doc.as_mut(), slice(ptr, len)) else {
        return;
    };
    doc.supply(offset, data);
}

/// The standard fonts, as the bundle `fonts_blob` writes. 0 on success,
/// -1 if the bundle is damaged (which [`pdf_error`] then explains).
#[no_mangle]
pub unsafe extern "C" fn pdf_fonts(doc: *mut Doc, ptr: *const u8, len: u32) -> i32 {
    let (Some(doc), Some(data)) = (doc.as_mut(), slice(ptr, len)) else {
        return -1;
    };
    match doc.set_fonts(data) {
        Ok(()) => 0,
        Err(e) => {
            doc.fail(e);
            -1
        }
    }
}

/// One pass over `page`: either the ranges to fetch, or nothing, meaning the
/// page is ready (see `pdf_page_svg`). Use `0xFFFF_FFFF` as `page` to open the
/// document and learn its page count instead.
///
/// Returns a `[start, len, start, len, …]` table in `u32`, `out_count` pairs
/// long, to be freed with [`pdf_ranges_free`]. On failure the return is null
/// and `out_count` is `0xFFFF_FFFF`; [`pdf_error`] then says why.
#[no_mangle]
pub unsafe extern "C" fn pdf_page_needs(
    doc: *mut Doc,
    page: u32,
    out_count: *mut u32,
) -> *mut u32 {
    if !out_count.is_null() {
        *out_count = 0;
    }
    let Some(doc) = doc.as_mut() else {
        return std::ptr::null_mut();
    };
    match doc.prepare(page) {
        Ready::Need(ranges) => {
            let mut flat = Vec::with_capacity(ranges.len() * 2);
            for r in ranges {
                flat.push(r.start as u32);
                flat.push(r.len() as u32);
            }
            let count = (flat.len() / 2) as u32;
            if !out_count.is_null() {
                *out_count = count;
            }
            let flat = flat.into_boxed_slice();
            Box::into_raw(flat) as *mut u32
        }
        Ready::Page | Ready::Opened(_) | Ready::Outline => std::ptr::null_mut(),
        Ready::Failed => {
            if !out_count.is_null() {
                *out_count = u32::MAX;
            }
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn pdf_ranges_free(ptr: *mut u32, count: u32) {
    if ptr.is_null() || count == u32::MAX {
        return;
    }
    drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
        ptr,
        count as usize * 2,
    )));
}

/// The page `pdf_page_needs` just prepared, as one SVG document (UTF-8), or
/// null if that page is not ready. Free with [`pdf_buf_free`] and the length.
#[no_mangle]
pub unsafe extern "C" fn pdf_page_svg(doc: *mut Doc, page: u32, out_len: *mut u32) -> *mut u8 {
    if !out_len.is_null() {
        *out_len = 0;
    }
    let Some(doc) = doc.as_mut() else {
        return std::ptr::null_mut();
    };
    let Some(svg) = doc.take_svg(page) else {
        return std::ptr::null_mut();
    };
    let bytes = svg.into_bytes().into_boxed_slice();
    if !out_len.is_null() {
        *out_len = bytes.len() as u32;
    }
    Box::into_raw(bytes) as *mut u8
}

/// The outline `pdf_page_needs(…, OUTLINE_ONLY)` just built: JSON,
/// `[[depth, page or null, "title"], …]`, page 0-based. Null if there is none
/// (an outline-less document gives `[]`, which is 2 bytes, not null).
/// Free with [`pdf_buf_free`] and the length.
#[no_mangle]
pub unsafe extern "C" fn pdf_outline(doc: *mut Doc, out_len: *mut u32) -> *mut u8 {
    if !out_len.is_null() {
        *out_len = 0;
    }
    let Some(doc) = doc.as_mut() else {
        return std::ptr::null_mut();
    };
    let Some(json) = doc.take_outline() else {
        return std::ptr::null_mut();
    };
    let bytes = json.into_bytes().into_boxed_slice();
    if !out_len.is_null() {
        *out_len = bytes.len() as u32;
    }
    Box::into_raw(bytes) as *mut u8
}

/// Pages in the document, 0 until `pdf_page_needs(…, OPEN_ONLY)` succeeded.
#[no_mangle]
pub unsafe extern "C" fn pdf_num_pages(doc: *mut Doc) -> u32 {
    doc.as_ref().map(Doc::num_pages).unwrap_or(0)
}

/// Bytes of the document held in memory, for the page's own readout.
#[no_mangle]
pub unsafe extern "C" fn pdf_held(doc: *mut Doc) -> u32 {
    doc.as_ref().map(|d| d.held() as u32).unwrap_or(0)
}

/// The last error, UTF-8, free with [`pdf_buf_free`]; length 0 if there was
/// none since the last successful call.
#[no_mangle]
pub unsafe extern "C" fn pdf_error(doc: *mut Doc, out_len: *mut u32) -> *mut u8 {
    if !out_len.is_null() {
        *out_len = 0;
    }
    let Some(doc) = doc.as_ref() else {
        return std::ptr::null_mut();
    };
    let Some(err) = doc.error() else {
        return std::ptr::null_mut();
    };
    let bytes = err.to_string().into_bytes().into_boxed_slice();
    if !out_len.is_null() {
        *out_len = bytes.len() as u32;
    }
    Box::into_raw(bytes) as *mut u8
}

unsafe fn slice<'a>(ptr: *const u8, len: u32) -> Option<&'a [u8]> {
    if ptr.is_null() {
        return None;
    }
    Some(std::slice::from_raw_parts(ptr, len as usize))
}

//! The font bundle the browser fetches: what goes in comes out, and a damaged
//! one is refused rather than half-read.

use std::sync::Arc;

use crate::{Codec, DirRead, MemDir};

/// Three files of the kind a font directory holds: some JSON, something that
/// repeats (fonts have long runs of similar outline data), something tiny.
fn sample() -> MemDir {
    let mut dir = MemDir::default();
    dir.insert("fonts.json", Arc::<[u8]>::from(&b"{\"Helvetica\":\"a.otf\"}"[..]));
    let mut a = Vec::with_capacity(9000);
    for i in 0..4096u32 {
        a.extend_from_slice(&(i % 251).to_le_bytes());
    }
    dir.insert("a.otf", a);
    dir.insert("b.otf", Arc::<[u8]>::from(&b"OTTO\0\0\0\0"[..]));
    dir
}

fn bytes(dir: &MemDir, name: &str) -> Vec<u8> {
    dir.read_file(name).expect("file").to_vec()
}

#[test]
fn every_codec_round_trips() {
    let dir = sample();
    let mut codecs = vec![Codec::Store, Codec::Gzip, Codec::Deflate];
    if cfg!(feature = "bundle-write") {
        codecs.push(Codec::Brotli);
    }
    for codec in codecs {
        let blob = dir.write_bundle(codec);
        let back = MemDir::parse_bundle(&blob).unwrap_or_else(|e| panic!("{codec:?}: {e}"));
        assert_eq!(back.len(), dir.len(), "{codec:?}: file count");
        assert_eq!(back.bytes(), dir.bytes(), "{codec:?}: total bytes");
        for name in ["fonts.json", "a.otf", "b.otf"] {
            assert_eq!(bytes(&back, name), bytes(&dir, name), "{codec:?}: {name}");
        }
    }
}

#[test]
fn a_compressed_bundle_is_smaller_than_the_files() {
    if !cfg!(feature = "bundle-write") {
        return;
    }
    let dir = sample();
    let blob = dir.write_bundle(Codec::Brotli);
    assert!(blob.len() < dir.bytes() / 2, "{} against {}", blob.len(), dir.bytes());
    // The header is a magic, a version, a codec and a length — nothing else.
    assert_eq!(&blob[..4], b"PDFF");
    assert_eq!(blob[4], 2);
    assert_eq!(blob[5], Codec::Brotli as u8);
    assert_eq!(u32::from_le_bytes(blob[6..10].try_into().unwrap()) as usize,
               dir.write_plain().len());
}

#[test]
fn a_short_payload_is_refused() {
    // A font bundle that silently loses bytes would render wrong glyphs, so the
    // declared length is checked against what came out of the decoder.
    let dir = sample();
    let blob = dir.write_bundle(Codec::Store);
    let short = &blob[..blob.len() - 1];
    assert!(MemDir::parse_bundle(short).is_err(), "one byte short was accepted");
}

#[test]
fn a_damaged_stream_is_refused() {
    let dir = sample();
    for codec in [Codec::Store, Codec::Gzip, Codec::Deflate] {
        let mut blob = dir.write_bundle(codec);
        let last = blob.len() - 1;
        blob[last] ^= 0xff;
        let the_same = MemDir::parse_bundle(&blob)
            .map(|back| bytes(&back, "a.otf") == bytes(&dir, "a.otf"))
            .unwrap_or(false);
        // Trailing garbage in an uncompressed bundle is outside the file list
        // and legitimately invisible; a compressed one must notice.
        assert_eq!(the_same, codec == Codec::Store, "{codec:?} after a flipped byte");
    }
}

#[test]
fn nonsense_is_refused() {
    for blob in [&b""[..], b"PDFF", b"PDFF\x09", b"not a bundle at all"] {
        assert!(MemDir::parse_bundle(blob).is_err(), "{blob:?} was accepted");
    }
}

#[test]
fn an_unknown_codec_is_refused() {
    let dir = sample();
    let mut blob = dir.write_bundle(Codec::Store);
    blob[5] = 99;
    assert!(MemDir::parse_bundle(&blob).is_err(), "codec 99 was accepted");
}

#[test]
fn the_plain_layout_is_readable_without_the_reader() {
    // Version 1 is what a host can start reading before the data is all there,
    // so its offsets have to point into the file as written.
    let dir = sample();
    let plain = dir.write_plain();
    assert_eq!(&plain[..4], b"PDFF");
    assert_eq!(plain[4], 1);
    assert_eq!(u32::from_le_bytes(plain[5..9].try_into().unwrap()), 3);

    let mut pos = 9;
    for _ in 0..3 {
        let name_len = u16::from_le_bytes(plain[pos..pos + 2].try_into().unwrap()) as usize;
        pos += 2;
        let name = std::str::from_utf8(&plain[pos..pos + name_len]).unwrap().to_string();
        pos += name_len;
        let offset = u32::from_le_bytes(plain[pos..pos + 4].try_into().unwrap()) as usize;
        let len = u32::from_le_bytes(plain[pos + 4..pos + 8].try_into().unwrap()) as usize;
        pos += 8;
        assert_eq!(&plain[offset..offset + len], &bytes(&dir, &name)[..], "{name}");
    }
}

#[test]
fn a_sub_directory_is_a_bundle_of_its_own() {
    let mut dir = sample();
    dir.insert("db/x.ttf", Arc::<[u8]>::from(&b"OTTO"[..]));
    let db = dir.sub_dir("db").expect("db");
    assert_eq!(db.read_file("x.ttf").expect("x.ttf").as_ref(), b"OTTO");
}

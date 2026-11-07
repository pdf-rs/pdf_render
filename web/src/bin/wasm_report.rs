//! What is in a `.wasm` file, without a toolchain: size, the import list (which
//! must be empty for the viewer), the exports, the custom sections.
//!
//! `cargo run -p pdf_web --bin wasm_report -- <file.wasm>` — run from the
//! workspace root, where cargo does not default to the wasm target.
//!
//! The point of the import list is the promise the page makes: a module that
//! imports nothing cannot call out, and `WebAssembly.Module.imports` in the
//! browser can be checked against this by eye.

use std::process::ExitCode;

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(b: &'a [u8]) -> Self {
        Reader { b, at: 0 }
    }
    fn u8(&mut self) -> Option<u8> {
        let v = *self.b.get(self.at)?;
        self.at += 1;
        Some(v)
    }
    fn leb(&mut self) -> Option<u32> {
        let mut out = 0u32;
        let mut shift = 0;
        loop {
            let byte = self.u8()?;
            out |= ((byte & 0x7f) as u32) << shift;
            if byte & 0x80 == 0 {
                return Some(out);
            }
            shift += 7;
            if shift > 28 {
                return None;
            }
        }
    }
    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n)?;
        let s = self.b.get(self.at..end)?;
        self.at = end;
        Some(s)
    }
    fn name(&mut self) -> Option<String> {
        let n = self.leb()? as usize;
        Some(String::from_utf8_lossy(self.bytes(n)?).into_owned())
    }
    /// The value type of a global, a local or a reference type: one byte.
    fn valtype(&mut self) -> Option<u8> {
        self.u8()
    }
    /// Limits: a flag, then min, then max if the flag says so.
    fn limits(&mut self) -> Option<()> {
        let flag = self.u8()?;
        self.leb()?;
        if flag & 0x01 != 0 {
            self.leb()?;
        }
        Some(())
    }
}

/// Skip the immediates of one import entry, after module::field and the kind
/// byte. Reading the kind and then not the rest is what desynchronises a naive
/// reader: the first entry closes on a function type index, so the next
/// "name length" it reads is a byte out of the middle of a string.
fn skip_import_immediate(r: &mut Reader, kind: u8) -> Option<()> {
    match kind {
        0x00 => {
            r.leb()?; // type index
        }
        0x01 => {
            r.valtype()?; // reference type
            r.limits()?;
        }
        0x02 => {
            r.limits()?;
        }
        0x03 => {
            r.valtype()?; // value type
            r.u8()?; // mutability
        }
        _ => return None,
    }
    Some(())
}

fn main() -> ExitCode {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: wasm_report <file.wasm>");
        return ExitCode::FAILURE;
    };
    let data = match std::fs::read(&path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    if data.len() < 8 || &data[..4] != b"\0asm" {
        eprintln!("{path}: not a wasm module");
        return ExitCode::FAILURE;
    }
    println!("file:  {path}");
    println!("bytes: {}", data.len());
    {
        use std::io::Write;
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        e.write_all(&data).unwrap();
        let gz = e.finish().unwrap();
        println!(
            "gzip:  {} bytes ({:.1} % of the file), what goes over the wire",
            gz.len(),
            100.0 * gz.len() as f64 / data.len() as f64
        );
    }
    let version = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    println!("wasm:  version {version}");

    let mut r = Reader::new(&data[8..]);
    let mut imports: Vec<(String, String, u8)> = Vec::new();
    let mut exports: Vec<(String, u8)> = Vec::new();
    let mut custom: Vec<(String, u32)> = Vec::new();
    let mut funcs = 0u32;
    let mut sections = Vec::new();

    while r.at < r.b.len() {
        let Some(id) = r.u8() else { break };
        let Some(len) = r.leb() else { break };
        let Some(body) = r.bytes(len as usize) else {
            eprintln!("truncated section {id}");
            break;
        };
        let mut s = Reader::new(body);
        match id {
            0 => {
                if let Some(n) = s.name() {
                    custom.push((n, len));
                }
            }
            1 => funcs = s.leb().unwrap_or(0),
            2 => {
                let n = s.leb().unwrap_or(0);
                for _ in 0..n {
                    let (Some(module), Some(field), Some(kind)) =
                        (s.name(), s.name(), s.u8())
                    else {
                        break;
                    };
                    if skip_import_immediate(&mut s, kind).is_none() {
                        eprintln!("import {module}::{field}: unknown kind {kind}");
                        break;
                    }
                    imports.push((module, field, kind));
                }
            }
            7 => {
                let n = s.leb().unwrap_or(0);
                for _ in 0..n {
                    let (Some(name), Some(kind)) = (s.name(), s.u8()) else {
                        break;
                    };
                    s.leb(); // index
                    exports.push((name, kind));
                }
            }
            _ => {}
        }
        sections.push((id, len));
    }

    println!(
        "sections: {} — {}",
        sections.len(),
        sections
            .iter()
            .map(|(id, len)| format!("{}({len})", section_name(*id)))
            .collect::<Vec<_>>()
            .join(" ")
    );
    println!("function types: {funcs}");

    println!("imports: {}", imports.len());
    for (module, field, kind) in &imports {
        println!("  {module}::{field}  (kind {kind})");
    }
    println!("exports: {}", exports.len());
    for (name, kind) in &exports {
        println!("  {name}  (kind {kind})");
    }
    for (name, len) in &custom {
        println!("custom: {name} ({len} bytes)");
    }
    ExitCode::SUCCESS
}

fn section_name(id: u8) -> &'static str {
    match id {
        0 => "custom",
        1 => "type",
        2 => "import",
        3 => "function",
        4 => "table",
        5 => "memory",
        6 => "global",
        7 => "export",
        8 => "start",
        9 => "element",
        10 => "code",
        11 => "data",
        12 => "datacount",
        _ => "?",
    }
}

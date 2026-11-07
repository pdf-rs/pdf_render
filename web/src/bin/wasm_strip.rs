//! Drop the custom sections a served module does not need — the names of every
//! function and type, which is ~14 % of the file and is only useful to a
//! debugger or a profiler.
//!
//! `cargo run -p pdf_web --bin wasm_strip -- in.wasm out.wasm`
//!
//! Custom sections carry no semantics: the core sections either reference each
//! other by index or are self-describing, so removing one cannot change what
//! the module does. That is also why this is a byte-level copy of every other
//! section rather than a re-encode: nothing between the sections moves except
//! what is dropped.

use std::process::ExitCode;

/// What goes: the name section (`name`, ~400 kB of identifiers), and nothing
/// else today. `producers` and `target_features` stay — 225 bytes of
/// provenance, and the latter is a hint a browser may act on.
const DROP: &[&str] = &["name"];

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let (Some(input), Some(output)) = (args.next(), args.next()) else {
        eprintln!("usage: wasm_strip <in.wasm> <out.wasm>");
        return ExitCode::FAILURE;
    };
    let data = match std::fs::read(&input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{input}: {e}");
            return ExitCode::FAILURE;
        }
    };
    if data.len() < 8 || &data[..4] != b"\0asm" {
        eprintln!("{input}: not a wasm module");
        return ExitCode::FAILURE;
    }

    let mut out = Vec::with_capacity(data.len());
    out.extend_from_slice(&data[..8]);
    let mut at = 8;
    let mut dropped = 0usize;
    while at < data.len() {
        let id = data[at];
        let mut p = at + 1;
        // The section's length, as the LEB it is (and where its bytes start).
        let mut len = 0u32;
        let mut shift = 0;
        loop {
            let Some(&byte) = data.get(p) else {
                eprintln!("truncated section header at {at}");
                return ExitCode::FAILURE;
            };
            p += 1;
            len |= ((byte & 0x7f) as u32) << shift;
            if byte & 0x80 == 0 {
                break;
            }
            shift += 7;
            if shift > 28 {
                eprintln!("bad section length at {at}");
                return ExitCode::FAILURE;
            }
        }
        let body = p..p + len as usize;
        if body.end > data.len() {
            eprintln!("section at {at} runs past the end of the file");
            return ExitCode::FAILURE;
        }
        if id == 0 {
            // Custom: a name-length, then the name.
            let mut q = body.start;
            let mut nlen = 0u32;
            let mut shift = 0;
            loop {
                let byte = data[q];
                q += 1;
                nlen |= ((byte & 0x7f) as u32) << shift;
                if byte & 0x80 == 0 {
                    break;
                }
                shift += 7;
            }
            let name = String::from_utf8_lossy(&data[q..q + nlen as usize]).into_owned();
            if DROP.contains(&name.as_str()) {
                dropped += len as usize + (p - at - 1) + 1;
                println!("drop:  {name} ({} bytes)", len);
                at = body.end;
                continue;
            }
        }
        // Copy the section exactly as it was, header included.
        out.extend_from_slice(&data[at..body.end]);
        at = body.end;
    }

    if let Err(e) = std::fs::write(&output, &out) {
        eprintln!("{output}: {e}");
        return ExitCode::FAILURE;
    }
    println!(
        "{} bytes -> {} bytes (dropped {dropped})",
        data.len(),
        out.len()
    );
    ExitCode::SUCCESS
}

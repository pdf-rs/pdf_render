use pdf_vview::application::{App, ViewContext, FileContext};
use pdf::{file::FileOptions, PdfError};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[arg(short, long, required(true))]
    path: String,
}

pub fn main() -> Result<(), PdfError> {
    // TODO: initializing both env_logger and console_logger fails on wasm.
    // Figure out a more principled approach.
    #[cfg(not(target_arch = "wasm32"))]
    env_logger::init();

    let args = Args::parse();
    let path = args.path;

    let file = FileOptions::uncached().open(&path).unwrap();

    let file_ctx = FileContext::new(file);

    App::run(ViewContext::new(vec!(file_ctx)));

    Ok(())
}

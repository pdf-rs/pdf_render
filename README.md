# pdf_render [![Build Status](https://travis-ci.com/pdf-rs/pdf.svg?branch=master)](https://travis-ci.com/pdf-rs/pdf_render)
Experimental PDF viewer building on [pdf](https://github.com/pdf-rs/pdf).

Feel free to contribute with ideas, issues or code! Please join [us on Zulip](https://type.zulipchat.com/#narrow/stream/209232-pdf) if you have any questions or problems.

# Fonts
Get a copy of https://github.com/s3bk/pdf_fonts and set `STANDARD_FONTS` to the directory if `pdf_fonts`.

# Viewer
run it:
  `cargo run --bin view --release YOUR_FILE.pdf`
Right now you can change pages with left and right arrow keys and zoom with '+' and '-'. Works for some files.

## [Try it in your browser](https://pdf-rs.github.io/view-wasm/)

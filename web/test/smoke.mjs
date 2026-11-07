// Smoke test for the built module, without a browser: node's WebAssembly is
// the same engine family the browser uses, and the ABI is plain C.
//
//   node web/test/smoke.mjs [file.pdf]
//
// It checks the promise the page checks — imports === 0 — then opens a real
// document, fetches its ranges out of the file in memory, and renders pages.

import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const www = join(here, "..", "www");
const pdfPath = process.argv[2] ?? "/home/hematite/ia/WG73-WriterGuide.pdf";
const PAGE_ALL = 0xffffffff;

const wasm = await readFile(join(www, "pdf_web.wasm"));
const { module, instance } = await WebAssembly.instantiate(wasm, {});
const imports = WebAssembly.Module.imports(module);
if (imports.length !== 0) {
    throw new Error(`the module wants ${imports.length} import(s): ${JSON.stringify(imports)}`);
}
const m = instance.exports;
const abi = m.pdf_abi();
if (abi !== 1) throw new Error(`module ABI ${abi}, viewer wants 1`);

const u32 = (ptr) => new Uint32Array(m.memory.buffer, ptr, 1);
const outCount = m.pdf_buf_alloc(4);
const outLen = m.pdf_buf_alloc(4);

function text(ptr, len) {
    if (!ptr || len === 0) return "";
    const s = new TextDecoder().decode(new Uint8Array(m.memory.buffer, ptr, len));
    m.pdf_buf_free(ptr, len);
    return s;
}

const file = await readFile(pdfPath);
const doc = m.pdf_doc_new(file.length);

// The standard fonts: the whole bundle, up front, as the page does it.
const bundle = await readFile(join(www, "standard_fonts.brotli.bundle"));
{
    const ptr = m.pdf_buf_alloc(bundle.length);
    new Uint8Array(m.memory.buffer, ptr, bundle.length).set(bundle);
    const rc = m.pdf_fonts(doc, ptr, bundle.length);
    m.pdf_buf_free(ptr, bundle.length);
    if (rc !== 0) {
        throw new Error(`font bundle rejected: ${text(m.pdf_error(doc, outLen), u32(outLen)[0])}`);
    }
}

let fetched = 0;
let passes = 0;

/** Run passes until the module stops asking for bytes — the page, in memory. */
function settle(page) {
    for (let round = 0; round < 32; round++) {
        const needs = m.pdf_page_needs(doc, page, outCount);
        const n = u32(outCount)[0];
        if (n === 0xffffffff) throw new Error(`page: ${text(needs, u32(outLen)[0])}`);
        if (n === 0) return;
        passes++;
        const table = new Uint32Array(m.memory.buffer, needs, n * 2);
        const ranges = [];
        for (let i = 0; i < n; i++) ranges.push([table[i * 2], table[i * 2 + 1]]);
        m.pdf_ranges_free(needs, n);
        for (const [start, len] of ranges) {
            const bytes = file.subarray(start, start + len);
            const ptr = m.pdf_buf_alloc(bytes.length);
            new Uint8Array(m.memory.buffer, ptr, bytes.length).set(bytes);
            m.pdf_supply(doc, start, ptr, bytes.length);
            m.pdf_buf_free(ptr, bytes.length);
            fetched += bytes.length;
        }
    }
    throw new Error("did not settle in 32 rounds");
}

const started = Date.now();
settle(PAGE_ALL);
const pages = m.pdf_num_pages(doc);
console.log(`open:    ${pages} pages, ${passes} pass(es), ${fetched} of ${file.length} bytes (${(100 * fetched / file.length).toFixed(2)} %)`);

for (const page of [2, 0, 10]) {
    if (page >= pages) continue;
    fetched = 0;
    passes = 0;
    settle(page);
    const svgPtr = m.pdf_page_svg(doc, page, outLen);
    const svg = text(svgPtr, u32(outLen)[0]);
    const paths = (svg.match(/<path/g) ?? []).length;
    console.log(`page ${String(page + 1).padStart(3)}: ${passes} pass(es), ${fetched} bytes, svg ${svg.length} bytes, ${paths} path(s)`);
}

console.log(`held:    ${m.pdf_held(doc)} bytes in the range cache`);
m.pdf_doc_free(doc);
console.log(`ok in ${Date.now() - started} ms, imports: 0`);

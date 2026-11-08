// Page preloading, without a browser.
//
//   node web/test/prefetch.mjs [file.pdf] [table.tbl]
//
// `doc.js` is the browser half of the viewer: fetch, the DOM, the font bundle.
// It touches the DOM nowhere, so with `fetch` and the two out-parameters stubbed
// here it runs under node — which is the only way to check the thing prefetching
// is for: that a page turn after a preload costs no request at all.
//
// The file is served out of memory by range, exactly as a share does it.

import { readFile } from "node:fs/promises";
import { brotliDecompressSync } from "node:zlib";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const www = join(here, "..", "www");
const pdfPath = process.argv[2] ?? "/home/hematite/tmp/pdf-adobe-compare/site/std14.pdf";
const tblPath = process.argv[3] ?? `${pdfPath}.tbl`;

const WASM = "https://example.invalid/pdf_web.wasm";
const FONTS = "https://example.invalid/standard_fonts.br";
const PDF = "https://example.invalid/file.pdf";

const wasm = await readFile(join(www, "pdf_web.wasm"));
const fonts = brotliDecompressSync(await readFile(join(www, "standard_fonts.br")));
const pdf = await readFile(pdfPath);
const tbl = await readFile(tblPath, "utf8");

/** Every byte range the page asks the "server" for. */
const reads = [];

globalThis.fetch = async (url, init = {}) => {
    const u = String(url);
    if (u === WASM) {
        return new Response(wasm, { headers: { "content-type": "application/wasm" } });
    }
    if (u === FONTS) return new Response(fonts);
    if (u === PDF) {
        const header = init.headers?.Range ?? init.headers?.range ?? "";
        const m = /bytes=(\d+)-(\d+)/.exec(header);
        if (!m) throw new Error(`the viewer asked for ${u} without a range`);
        const [start, end] = [Number(m[1]), Math.min(Number(m[2]), pdf.length - 1)];
        reads.push([start, end + 1]);
        return new Response(pdf.subarray(start, end + 1), {
            status: 206,
            headers: { "content-range": `bytes ${start}-${end}/${pdf.length}` },
        });
    }
    return new Response("not found", { status: 404 });
};

const { load, Doc, HttpSource, parseTable } = await import(new URL("../www/doc.js", import.meta.url));

await load(WASM);

const doc = new Doc(new HttpSource(PDF, pdf.length));
await doc.fonts(FONTS);
const table = parseTable(tbl);
doc.setTable(table);
const pages = await doc.open();
if (pages < 2) throw new Error(`${pdfPath}: needs two pages to have a neighbour`);

console.log(`${pdfPath}: ${pages} page(s), table ${table.size} line(s)`);
console.log(`open: ${doc.requests} request(s), ${doc.bytes} bytes`);

// Page 1 on its own: the bytes it costs are the bytes it needs.
const before = reads.length;
const first = await doc.page(0);
const own = reads.length - before;
console.log(`page 1: ${own} request(s), ${first.length} bytes of svg`);

// The neighbour, fetched while nobody is looking.
const readBefore = reads.length;
const preloaded = await doc.prefetch(1);
console.log(`prefetch 2: ${reads.length - readBefore} request(s), ${preloaded} bytes`);
if (!preloaded && doc.bytes === pdf.length) {
    // A document this small fits in the first request, so there is nothing to
    // preload and nothing to check. Say so rather than report a pass.
    console.log("not exercised: the table put the whole file in the first request");
    doc.free();
    process.exit(0);
}

// ... and the page itself is now free: no request, no round trip.
const readBeforeAgain = reads.length;
const bytesBefore = doc.bytes;
const second = await doc.page(1);
const turnReads = reads.length - readBeforeAgain;
console.log(`page 2 after preload: ${turnReads} request(s), ${doc.bytes - bytesBefore} bytes of traffic, ${second.length} bytes of svg`);
if (turnReads !== 0) throw new Error(`a preloaded page still made ${turnReads} request(s)`);

// Asking twice must not fetch twice.
const readBeforeTwice = reads.length;
await Promise.all([doc.prefetch(2), doc.prefetch(2)]);
const twice = reads.length - readBeforeTwice;
console.log(`prefetch 3, twice at once: ${twice} request(s)`);
if (twice > 1) throw new Error(`the same page was fetched ${twice} times`);

// Neighbours that do not exist are not an error, and cost nothing.
const readBeforeEdge = reads.length;
await doc.prefetch(-1);
await doc.prefetch(pages);
if (reads.length !== readBeforeEdge) throw new Error("an edge page made a request");

doc.free();
console.log("ok");

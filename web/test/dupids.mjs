// Do the ids of a combined page ever repeat?
//
//   node web/test/dupids.mjs file.pdf table.tbl [maxPages]

import { readFile } from "node:fs/promises";
import { brotliDecompressSync } from "node:zlib";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const www = join(here, "..", "www");
const pdfPath = process.argv[2];
const tblPath = process.argv[3];
const max = Number(process.argv[4] ?? 0) || Infinity;

const WASM = "https://example.invalid/pdf_web.wasm";
const FONTS = "https://example.invalid/standard_fonts.br";
const PDF = "https://example.invalid/file.pdf";

const wasm = await readFile(process.env.PDF_WEB_WASM ?? join(www, "pdf_web.wasm"));
const fonts = brotliDecompressSync(await readFile(join(www, "standard_fonts.br")));
const pdf = await readFile(pdfPath);
const tbl = await readFile(tblPath, "utf8");

globalThis.fetch = async (url, init = {}) => {
    const u = String(url);
    if (u === WASM) return new Response(wasm, { headers: { "content-type": "application/wasm" } });
    if (u === FONTS) return new Response(fonts);
    if (u === PDF) {
        const header = init.headers?.Range ?? init.headers?.range ?? "";
        const m = /bytes=(\d+)-(\d+)/.exec(header);
        if (!m) throw new Error("no range");
        const [start, end] = [Number(m[1]), Math.min(Number(m[2]), pdf.length - 1)];
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

let dupPages = 0, imgPages = 0, defs = 0, uses = 0, stray = 0, missingPages = 0, missingRefs = 0;
for (let i = 0; i < pages && i < max; i++) {
    await doc.prefetch(i);
    const svg = await doc.page(i);
    const ids = [...svg.matchAll(/ id="([^"]+)"/g)].map((m) => m[1]);
    const seen = new Set();
    const dup = ids.filter((id) => (seen.has(id) ? true : (seen.add(id), false)));
    if (dup.length) {
        dupPages++;
        if (dupPages <= 5) console.log(`page ${i + 1}: ${dup.length} repeated id(s), e.g. ${dup.slice(0, 4).join(", ")}`);
    }
    // Every reference must point at something the page defines.
    const refs = [...svg.matchAll(/(?:href="#|url\(#)([^")]+)/g)].map((m) => m[1]);
    const missing = refs.filter((r) => !seen.has(r));
    if (missing.length) {
        missingPages++;
        missingRefs += missing.length;
        if (missingPages <= 3) console.log(`page ${i + 1}: ${missing.length} dangling ref(s), e.g. ${missing.slice(0, 4).join(", ")}`);
    }
    if (svg.includes("<image ")) imgPages++;
    defs += ids.length;
    uses += (svg.match(/<use /g) ?? []).length;
    // Anything the backend did not write: an id whose prefix is a foreign one.
    stray += ids.filter((id) => !/^(l\d+_)?[gc]\d+$/.test(id) && !/^l\d+_clip$/.test(id)).length;
}
console.log(`${Math.min(pages, max)} page(s): ${defs} defs, ${uses} <use>, ${imgPages} with a raster, ${dupPages} with repeated ids, ${stray} foreign id(s), ${missingPages} page(s) with ${missingRefs} dangling ref(s)`);
doc.free();

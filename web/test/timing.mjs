// Where a page turn's milliseconds go, without a browser.
//
//   node web/test/timing.mjs file.pdf [table.tbl] [page …]
//
// The viewer's own complaint was "600 ms for the next page", and the number
// alone says nothing: it is fetch + module + DOM. Here the fetch is local and
// the DOM is absent, so what is left is the module — and the gap between this
// and 600 ms is what the page itself costs.

import { readFile } from "node:fs/promises";
import { brotliDecompressSync } from "node:zlib";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { performance } from "node:perf_hooks";

const here = dirname(fileURLToPath(import.meta.url));
const www = join(here, "..", "www");
const pdfPath = process.argv[2] ?? "/home/hematite/tmp/nasa.pdf";
const tblPath = process.argv[3] ?? `${pdfPath}.tbl`;
const rest = process.argv.slice(4);
const all = rest[0] === "all";
const wanted = rest.filter((a) => a !== "all").map(Number);

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

/** `[value, ms]`. */
const t = async (f) => { const a = performance.now(); const v = await f(); return [v, performance.now() - a]; };
const ms = (x) => `${x.toFixed(1).padStart(7)} ms`;

let [_, msLoad] = await t(() => load(WASM));
console.log(`load module   ${ms(msLoad)}  ${(wasm.length / 1024).toFixed(0)} KiB`);

const doc = new Doc(new HttpSource(PDF, pdf.length));
const [, msFonts] = await t(() => doc.fonts(FONTS));
console.log(`fonts         ${ms(msFonts)}  ${(fonts.length / 1024).toFixed(0)} KiB`);

const table = parseTable(tbl);
doc.setTable(table);
const [pages, msOpen] = await t(() => doc.open());
console.log(`open          ${ms(msOpen)}  ${pages} page(s), table ${table.size} line(s)`);

if (all) {
    const samples = [];
    for (let i = 0; i < pages; i++) {
        await doc.prefetch(i);
        const [, msRender] = await t(() => doc.page(i));
        const [svg] = await t(() => doc.page(i));
        samples.push({ page: i + 1, ms: msRender, svg: svg.length });
    }
    const sorted = [...samples].sort((a, b) => b.ms - a.ms);
    const total = samples.reduce((a, s) => a + s.ms, 0);
    console.log(`all ${pages} pages: mean ${(total / pages).toFixed(1)} ms`);
    console.log("slowest:\n" + sorted.slice(0, 8)
        .map((s) => `  page ${String(s.page).padStart(3)}  ${ms(s.ms)}  ${(s.svg / 1024).toFixed(0)} KiB svg`).join("\n"));
    console.log("fastest:\n" + sorted.slice(-3)
        .map((s) => `  page ${String(s.page).padStart(3)}  ${ms(s.ms)}  ${(s.svg / 1024).toFixed(0)} KiB svg`).join("\n"));
    const buckets = [50, 100, 200, 300, 500];
    for (const b of buckets) console.log(`  > ${String(b).padStart(3)} ms: ${samples.filter((s) => s.ms > b).length}`);
} else {
    const list = wanted.length ? wanted : [1, 2, 3, 10, 20, Math.min(21, pages), pages];
    for (const n of list) {
        if (n < 1 || n > pages) continue;
        const i = n - 1;
        await doc.prefetch(i);
        const [svg, msRender] = await t(() => doc.page(i));
        const [, msAgain] = await t(() => doc.page(i));
        console.log(`page ${String(n).padStart(3)}  render ${ms(msRender)}  again ${ms(msAgain)}  ${(svg.length / 1024).toFixed(0)} KiB svg`);
    }
}

doc.free();

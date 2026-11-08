// The browser half of `pdf_web`: fetch ranges, hand them to the module, put
// the SVG it returns on the page.
//
// Everything that touches the outside world is here — fetch, the DOM, the
// font bundle. The module itself has no imports at all, which `load()` checks
// rather than trusts: `WebAssembly.Module.imports()` must be empty, so the
// code that parses a stranger's PDF cannot reach the network, the DOM or
// `eval` even in principle.

/** `0` is not the "whole document" page number: this is. */
const PAGE_ALL = 0xffffffff;
/** …and this is "not a page number either: the table of contents". */
export const OUTLINE_ALL = 0xfffffffe;

let mod = null;
/** Two `u32` out-parameters in the module's memory, allocated once. */
let outCount = 0;
let outLen = 0;

export async function load(wasmUrl) {
    const response = await fetch(wasmUrl);
    const { instance, module } = await WebAssembly.instantiateStreaming(response, {});
    const imports = WebAssembly.Module.imports(module);
    if (imports.length !== 0) {
        throw new Error(`the module wants ${imports.length} import(s): ${imports.map(i => i.module + "." + i.name)}`);
    }
    mod = instance.exports;
    const abi = mod.pdf_abi();
    if (abi !== 1) throw new Error(`module ABI ${abi}, viewer wants 1`);
    outCount = mod.pdf_buf_alloc(4);
    outLen = mod.pdf_buf_alloc(4);
    return mod;
}

/** A fresh view every time: any allocation may have grown the memory. */
const u32 = (ptr) => new Uint32Array(mod.memory.buffer, ptr, 1);

function takeText(ptr, len) {
    if (!ptr || len === 0) return "";
    const text = new TextDecoder().decode(new Uint8Array(mod.memory.buffer, ptr, len));
    mod.pdf_buf_free(ptr, len);
    return text;
}

/** Copy `bytes` into the module and hand them over as the range at `start`. */
function supply(doc, start, bytes) {
    const ptr = mod.pdf_buf_alloc(bytes.length);
    new Uint8Array(mod.memory.buffer, ptr, bytes.length).set(bytes);
    mod.pdf_supply(doc, start, ptr, bytes.length);
    mod.pdf_buf_free(ptr, bytes.length);
}

/** A per-page range table, as `page_table` writes it — the sender's list of
 *  what each page reads. Returns `page number -> [[start, len], …]`, 1-based,
 *  with page 0 being the document itself (what it costs to count the pages).
 *
 *  Text, not binary, on purpose: it is a few dozen bytes per page, a server
 *  compresses it to almost nothing, and anyone can read it in a browser. The
 *  ranges are already merged by the sender, so the client takes them as they
 *  are. */
export function parseTable(text) {
    const lines = text.split("\n");
    if (lines[0].trim() !== "pdfranges 1") throw new Error("not a pdfranges table");
    const pages = new Map();
    const toc = [];
    let inToc = false;
    for (const line of lines.slice(2)) {
        const trimmed = line.trim();
        if (!trimmed) continue;
        // The sender's table may end with the document's contents, whose page
        // numbers it resolved while it had the whole file open. Read it here
        // and no page tree has to be walked to number the sidebar.
        if (!inToc && trimmed.startsWith("toc")) { inToc = true; continue; }
        if (inToc) {
            const m = /^(\d+)\s+(\d+|-)\s*(.*)$/.exec(trimmed);
            if (!m) continue;
            toc.push([Number(m[1]), m[2] === "-" ? null : Number(m[2]) - 1, m[3]]);
            continue;
        }
        const field = trimmed.split(/\s+/).filter(Boolean).map(Number);
        if (field.length < 2 || !Number.isSafeInteger(field[0])) continue;
        const [page, count] = field;
        const ranges = [];
        for (let i = 0; i < count; i++) {
            const start = field[2 + i * 2], len = field[3 + i * 2];
            if (!Number.isSafeInteger(start) || !Number.isSafeInteger(len)) break;
            ranges.push([start, len]);
        }
        if (ranges.length) pages.set(page, ranges);
    }
    return { pages, toc };
}

/** A plain file, read by range over HTTP. */
export class HttpSource {
    constructor(url, length) {
        this.url = url;
        this.length = length;
    }

    /** Bytes `[start, start + len)`; one request, whatever the size. */
    async read(start, len) {
        const end = start + len - 1;
        const response = await fetch(this.url, { headers: { Range: `bytes=${start}-${end}` } });
        if (response.status === 200) {
            // The server ignored the range and sent everything: take the
            // window we asked for and cost the caller nothing extra.
            const all = new Uint8Array(await response.arrayBuffer());
            return all.subarray(start, Math.min(start + len, all.length));
        }
        if (response.status !== 206) throw new Error(`${this.url}: HTTP ${response.status}`);
        return new Uint8Array(await response.arrayBuffer());
    }
}

/** A document fetched by range. */
export class Doc {
    #doc = 0;
    #source = null;
    /** The sender's table, if any: page index -> ranges. */
    #table = null;
    /** The contents that came with that table: `[depth, page, title]` rows. */
    #toc = [];
    /** What we hold, sorted and disjoint: `[start, end)` pairs. */
    #have = [];
    /** What the page has read, for the readout. */
    bytes = 0;
    requests = 0;
    /** What was fetched ahead of the page being shown, kept apart so the cost
     *  of a page stays the cost of *that* page. */
    preload = { bytes: 0, requests: 0 };
    /** Pages whose bytes are being fetched right now: page index -> promise. */
    #warm = new Map();
    /** One fetch pass at a time; see `#fetch`. */
    #turn = Promise.resolve();

    constructor(source) {
        this.#source = source;
        this.#doc = mod.pdf_doc_new(source.length);
    }

    /** Hand the page tables in: one round trip per page, no discovery loop. */
    setTable(table) {
        this.#table = table.pages;
        this.#toc = table.toc || [];
        return table.pages.size;
    }

    /** The contents the sender numbered, if it shipped them with its table:
     *  `[[depth, page, title], …]`, the same shape the module returns. Having
     *  them means neither the outline nor the page tree gets walked on the
     *  client at all — the sidebar is drawn from the file that was already
     *  fetched to make page turns cheap. */
    get tableToc() {
        return this.#toc;
    }

    /** Hand in the standard fonts: one bundle, every one of them, up front —
     *  so which fonts a document uses is not visible on the wire.
     *
     *  `url` points at the brotli-compressed bundle and the server sends it with
     *  `Content-Encoding: br`, so `arrayBuffer()` is already the plain bundle:
     *  one 547 kB response, decoded by the network layer, not by the module. */
    async fonts(url) {
        const response = await fetch(url);
        if (!response.ok) throw new Error(`${url}: HTTP ${response.status}`);
        const bytes = new Uint8Array(await response.arrayBuffer());
        const ptr = mod.pdf_buf_alloc(bytes.length);
        new Uint8Array(mod.memory.buffer, ptr, bytes.length).set(bytes);
        const rc = mod.pdf_fonts(this.#doc, ptr, bytes.length);
        mod.pdf_buf_free(ptr, bytes.length);
        if (rc !== 0) throw new Error(`bad font bundle: ${this.#lastError()}`);
        return bytes.length;
    }

    get numPages() {
        return mod.pdf_num_pages(this.#doc);
    }

    /** Whether the sender shipped a range table. With one, a page can be
     *  fetched before it is asked for — which is what lets the contents warm
     *  the heading the reader is pointing at. */
    get hasTable() {
        return this.#table !== null;
    }

    /** How many pages, and what that cost. */
    async open() {
        // The table has a line for the document itself, so even the first
        // request is complete instead of a discovery round.
        const known = this.#table && this.#table.get(0);
        if (known) {
            const missing = this.#missing(known);
            if (missing.length) await this.#fetch(missing);
        }
        await this.#settle(PAGE_ALL);
        return this.numPages;
    }

    /** One page, as a self-contained SVG document. */
    async page(index) {
        // If the sender told us what this page reads, ask for all of it at
        // once — the page then renders in this pass instead of the next one.
        const known = this.#table && this.#table.get(index + 1);
        if (known) {
            const missing = this.#missing(known);
            if (missing.length) await this.#fetch(missing);
        }
        await this.#settle(index);
        const ptr = mod.pdf_page_svg(this.#doc, index, outLen);
        const len = u32(outLen)[0];
        if (!ptr) throw new Error(this.#lastError() || `page ${index + 1} is not ready`);
        return takeText(ptr, len);
    }

    /** The document's own table of contents: `[[depth, page, title], …]`,
     *  page 0-based, `page` null for a row that points out of the document.
     *
     *  One pass over the outline, so it costs one round trip on top of what
     *  the page tree and the destinations need — a fraction of a page. An
     *  empty list is an answer, not a failure: most documents have none. */
    async outline() {
        if (!this.#doc) return [];
        // Shipped with the table: already resolved, and free.
        if (this.#toc.length) return this.#toc;
        await this.#settle(OUTLINE_ALL);
        const ptr = mod.pdf_outline(this.#doc, outLen);
        const len = u32(outLen)[0];
        if (!ptr || !len) return [];
        const text = takeText(ptr, len);
        try {
            return JSON.parse(text);
        } catch (e) {
            console.log(`outline: ${e.message}`);
            return [];
        }
    }

    /** Start fetching what page `index` will need, without rendering it, so a
     *  click on ←/→ costs no round trip. Bytes only: rendering a page is local
     *  work (tens of ms), the network is what a page turn waits for.
     *
     *  Needs the sender's table — without one the ranges are only known by
     *  asking, which is the render itself. Safe to call for the same page
     *  twice: the second call joins the first. */
    prefetch(index) {
        if (!this.#doc || !(index >= 0) || index >= this.numPages) return Promise.resolve(0);
        const known = this.#table && this.#table.get(index + 1);
        if (!known) return Promise.resolve(0);
        const running = this.#warm.get(index);
        if (running) return running;
        const before = this.preload.bytes;
        const job = this.#fetch(known, false)
            .then(() => this.preload.bytes - before)
            .finally(() => this.#warm.delete(index));
        this.#warm.set(index, job);
        return job;
    }

    /** Run passes until the module stops asking for bytes. */
    async #settle(page) {
        for (let round = 0; round < 32; round++) {
            const needs = this.#needs(page);
            if (needs === null) return;
            await this.#fetch(needs);
        }
        throw new Error(`page ${page}: did not settle in 32 rounds`);
    }

    /** The ranges the module wants, or null when it wants nothing more. */
    #needs(page) {
        u32(outCount)[0] = 0;
        const ptr = mod.pdf_page_needs(this.#doc, page, outCount);
        const n = u32(outCount)[0];
        if (n === 0xffffffff) {
            throw new Error(this.#lastError() || "the document could not be rendered");
        }
        if (n === 0) return null;
        const table = new Uint32Array(mod.memory.buffer, ptr, n * 2);
        const ranges = [];
        for (let i = 0; i < n; i++) ranges.push([table[i * 2], table[i * 2 + 1]]);
        mod.pdf_ranges_free(ptr, n);
        return ranges;
    }

    /** The parts of `ranges` we do not hold yet, as map-able `[start, len]`. */
    #missing(ranges) {
        const out = [];
        for (const [start, len] of ranges) {
            const end = start + len;
            let pos = start;
            for (const [s, e] of this.#have) {
                if (e <= pos) continue;
                if (s >= end) break;
                if (s > pos) out.push([pos, Math.min(s, end) - pos]);
                pos = Math.max(pos, e);
                if (pos >= end) break;
            }
            if (pos < end) out.push([pos, end - pos]);
        }
        return out;
    }

    /** Write down that `[start, start + len)` is here now, merged. */
    #remember(start, len) {
        const all = [[start, start + len], ...this.#have].sort((a, b) => a[0] - b[0]);
        const out = [];
        for (const r of all) {
            const last = out[out.length - 1];
            if (last && r[0] <= last[1]) last[1] = Math.max(last[1], r[1]);
            else out.push([r[0], r[1]]);
        }
        this.#have = out;
    }

    /** Every range of one pass, in parallel. They are separate requests —
     *  a browser HTTP/2 connection carries them together — and the module is
     *  told about each as it arrives.
     *
     *  One pass at a time: a prefetch of the next page and a render of this one
     *  would otherwise ask the server for the same bytes twice. `cost` keeps a
     *  prefetch out of the numbers the readout shows for the page itself. */
    #fetch(ranges, cost = true) {
        const run = async () => {
            const missing = this.#missing(ranges);
            if (!missing.length) return;
            if (cost) this.requests++;
            else this.preload.requests++;
            const bodies = await Promise.all(missing.map(async ([start, len]) => {
                const bytes = await this.#source.read(start, len);
                if (cost) this.bytes += bytes.length;
                else this.preload.bytes += bytes.length;
                this.#remember(start, bytes.length);
                return [start, bytes];
            }));
            for (const [start, bytes] of bodies) supply(this.#doc, start, bytes);
        };
        const pass = this.#turn.then(run, run);
        this.#turn = pass.then(() => {}, () => {});
        return pass;
    }

    #lastError() {
        const ptr = mod.pdf_error(this.#doc, outLen);
        return takeText(ptr, u32(outLen)[0]);
    }

    free() {
        if (this.#doc) mod.pdf_doc_free(this.#doc);
        this.#doc = 0;
    }
}

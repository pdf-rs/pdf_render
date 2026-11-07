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

/** A document fetched by range. */
export class Doc {
    #doc = 0;
    #url = "";
    /** What the page has read, for the readout. */
    bytes = 0;
    requests = 0;

    constructor(url, length) {
        this.#url = url;
        this.#doc = mod.pdf_doc_new(length);
    }

    /** Hand in the standard fonts: one bundle, every one of them, up front —
     *  so which fonts a document uses is not visible on the wire. */
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

    /** How many pages, and what that cost. */
    async open() {
        await this.#settle(PAGE_ALL);
        return this.numPages;
    }

    /** One page, as a self-contained SVG document. */
    async page(index) {
        await this.#settle(index);
        const ptr = mod.pdf_page_svg(this.#doc, index, outLen);
        const len = u32(outLen)[0];
        if (!ptr) throw new Error(this.#lastError() || `page ${index + 1} is not ready`);
        return takeText(ptr, len);
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

    /** Every range of one pass, in parallel. They are separate requests —
     *  a browser HTTP/2 connection carries them together — and the module is
     *  told about each as it arrives. */
    async #fetch(ranges) {
        this.requests++;
        const bodies = await Promise.all(ranges.map(async ([start, len]) => {
            const end = start + len - 1;
            const response = await fetch(this.#url, { headers: { Range: `bytes=${start}-${end}` } });
            if (response.status !== 206) {
                // 200 means the server ignored the range: it sent the whole
                // file, which is correct behaviour and costs too much.
                if (!response.ok) throw new Error(`${this.#url}: HTTP ${response.status}`);
            }
            const bytes = new Uint8Array(await response.arrayBuffer());
            this.bytes += bytes.length;
            return [start, bytes];
        }));
        for (const [start, bytes] of bodies) supply(this.#doc, start, bytes);
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

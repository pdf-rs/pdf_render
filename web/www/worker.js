// The document, on a thread of its own.
//
// Parsing a stranger's PDF and drawing a page out of it is the viewer's one
// expensive job — tens to hundreds of milliseconds of wasm and SVG building.
// It used to run in the page, and the page is the thread the reader types in:
// a chat message beside an open document paid for the document's drawing. Here
// the same work runs in a worker. The page asks for a page's SVG and gets a
// string back; everything else — fetch by range, the font bundle, the range
// table, the outline — happens in here too, so the network and the module are
// as far from the page's keystrokes as the browser allows.
//
// The module still has no imports at all (`doc.js` checks that, not trusts it),
// and a worker has no DOM to reach even in principle.

import { load, Doc, HttpSource, parseTable } from './doc.js';
import { Sealed } from './seal.js';

let doc = null;

/** Bytes we already hold — a file the reader dropped on the page. */
class LocalSource {
    constructor(bytes) { this.bytes = bytes; this.length = bytes.length; }
    read(start, len) { return this.bytes.subarray(start, start + len); }
}

/** Bytes of a PDF at `url`, without reading it: HEAD, else a one-byte range. */
async function fileLength(url) {
    const head = await fetch(url, { method: 'HEAD' });
    const len = head.headers.get('content-length');
    if (head.ok && len) return Number(len);
    const probe = await fetch(url, { headers: { Range: 'bytes=0-0' } });
    const range = probe.headers.get('content-range');   // "bytes 0-0/123456"
    const total = range && range.split('/').pop();
    if (!total || total === '*') throw new Error(`${url}: the server does not say how large it is`);
    return Number(total);
}

/** The sender's per-page table, if it shipped one: `<pdf>.tbl` next to the
 *  file. No table is not an error — the client then discovers the ranges page
 *  by page. */
async function loadTable(url) {
    try {
        const response = await fetch(`${url}.tbl`);
        if (!response.ok) return null;
        const table = parseTable(await response.text());
        console.log(`table: ${table.pages.size} page(s), ${table.toc.length} heading(s)`);
        return table;
    } catch (e) {
        console.log(`no range table: ${e.message || e}`);
        return null;
    }
}

/** What the page's readout shows, sent along with every reply so the page
 *  never has to ask a second question to draw one line of text. */
function stats() {
    return doc
        ? { bytes: doc.bytes, requests: doc.requests, preload: { ...doc.preload } }
        : {};
}

function forget() {
    if (doc) doc.free();
    doc = null;
}

async function openUrl({ url, key, wasm, fonts }) {
    forget();
    const mod = await load(wasm);
    let source;
    try {
        source = key ? await Sealed.open(url, key) : new HttpSource(url, await fileLength(url));
    } catch (e) {
        if (key) throw e;
        // No key, and the plain file is not there: say what is actually wrong
        // rather than "Failed to fetch".
        const sealed = await fetch(`${url}.sealed`, { headers: { Range: 'bytes=0-0' } }).catch(() => null);
        if (sealed && sealed.ok) {
            throw new Error(
                `${url} is sealed: this link is missing its key. Open the whole link, including ` +
                `the part after # (it looks like #k=…); without it the file is ciphertext and ` +
                `nothing here can read it.`);
        }
        throw e;
    }
    const length = source.length;
    const d = new Doc(source);
    const fontBytes = await d.fonts(fonts);
    const table = await loadTable(url);
    if (table) d.setTable(table);
    const pages = await d.open();
    doc = d;
    void mod;
    return {
        pages, length, fonts: fontBytes,
        hasTable: !!table,
        tableToc: table ? table.toc : [],
        tablePages: table ? table.pages.size : 0,
        ...stats(),
    };
}

async function openLocal({ bytes, wasm, fonts }) {
    forget();
    await load(wasm);
    const all = new Uint8Array(bytes);
    // No server: every byte is here already, so the module is handed the file
    // before it asks for a single range.
    const d = new Doc(new LocalSource(all));
    d.give(all);
    const fontBytes = await d.fonts(fonts);
    const pages = await d.open();
    doc = d;
    return { pages, length: all.length, fonts: fontBytes, hasTable: false, tableToc: [], tablePages: 0, ...stats() };
}

const ops = {
    open: (args) => openUrl(args),
    openLocal: (args) => openLocal(args),

    async page({ index }) {
        if (!doc) throw new Error('no document');
        const t0 = performance.now();
        const svg = await doc.page(index);
        return { svg, ms: Math.round(performance.now() - t0), ...stats() };
    },

    async prefetch({ index }) {
        if (!doc) return { got: 0 };
        const got = await doc.prefetch(index);
        return { got, ...stats() };
    },

    async outline() {
        if (!doc) return { entries: [] };
        return { entries: await doc.outline(), ...stats() };
    },

    /** Hand the page what the sender's table already resolved, without a round
     *  trip: the numbers `open` sent are enough for the sidebar. */
    async tableToc() {
        return doc ? { entries: doc.tableToc, hasTable: doc.hasTable } : { entries: [], hasTable: false };
    },

    async close() {
        forget();
        return {};
    },
};

/** One job at a time: the module keeps a single set of out-parameters, so two
 *  pages half-rendered at once would read each other's numbers. The page
 *  serialises its own renders; this makes the worker honest about it too. */
let turn = Promise.resolve();

self.onmessage = (event) => {
    const { id, op, args } = event.data || {};
    const run = ops[op];
    const job = async () => {
        if (!run) throw new Error(`unknown op ${op}`);
        return await run(args || {});
    };
    const settled = turn.then(job, job);
    turn = settled.then(() => {}, () => {});
    settled.then(
        (value) => self.postMessage({ id, ok: true, value }),
        (error) => self.postMessage({
            id,
            ok: false,
            error: {
                message: error && error.message ? String(error.message) : String(error),
                stack: error && error.stack ? String(error.stack) : '',
            },
        }),
    );
};

// The page's half of the conversation with `worker.js`.
//
// What the document used to be on this thread — `open`, `page`, `prefetch`,
// `outline`, `free`, and the byte counts the readout shows — is a proxy now:
// the bytes, the module and the drawing live in a worker, and this is the
// handle the page holds. Same surface, so nothing above has to know.

let worker = null;
let seq = 0;
/** id -> {resolve, reject}, for the calls still in flight. */
const waiting = new Map();

function ensureWorker() {
    if (worker) return worker;
    worker = new Worker(new URL('./worker.js', import.meta.url), { type: 'module' });
    worker.onmessage = (event) => {
        const { id, ok, value, error } = event.data || {};
        const job = waiting.get(id);
        if (!job) return;
        waiting.delete(id);
        if (ok) {
            job.resolve(value);
            return;
        }
        // The page's `fail()` prints the message and then the stack when they
        // differ: a worker's stack is the useful part of a stranger's PDF
        // going wrong, so carry both.
        const err = new Error(error && error.message ? error.message : 'the viewer failed');
        if (error && error.stack) err.stack = error.stack;
        job.reject(err);
    };
    worker.onerror = (event) => {
        const err = new Error(event.message || 'the viewer worker failed to start');
        for (const job of waiting.values()) job.reject(err);
        waiting.clear();
        worker = null;          // a dead worker is not a worker: the next call starts one
    };
    return worker;
}

function call(op, args = {}, transfer = []) {
    const w = ensureWorker();
    const id = ++seq;
    return new Promise((resolve, reject) => {
        waiting.set(id, { resolve, reject });
        w.postMessage({ id, op, args }, transfer);
    });
}

export class Remote {
    pages = 0;
    length = 0;
    hasTable = false;
    tableToc = [];
    /** What the page has read, and what it fetched ahead: the readout's
     *  numbers, refreshed by every reply. */
    bytes = 0;
    requests = 0;
    preload = { bytes: 0, requests: 0 };
    /** Milliseconds the worker spent drawing the last page. */
    workerMs = 0;
    #live = false;

    async open({ url, key, wasm, fonts }) {
        return this.#take(await call('open', { url, key, wasm, fonts }));
    }

    async openLocal({ bytes, wasm, fonts }) {
        return this.#take(await call('openLocal', { bytes, wasm, fonts }, [bytes]));
    }

    #take(reply) {
        this.pages = reply.pages;
        this.length = reply.length;
        this.hasTable = reply.hasTable;
        this.tableToc = reply.tableToc || [];
        this.#stats(reply);
        this.#live = true;
        return reply;
    }

    #stats(reply) {
        if (!reply) return;
        if (typeof reply.bytes === 'number') this.bytes = reply.bytes;
        if (typeof reply.requests === 'number') this.requests = reply.requests;
        if (reply.preload) this.preload = { ...reply.preload };
    }

    async page(index) {
        const reply = await call('page', { index });
        this.#stats(reply);
        this.workerMs = reply.ms || 0;
        return reply.svg;
    }

    async prefetch(index) {
        const reply = await call('prefetch', { index });
        this.#stats(reply);
        return reply.got;
    }

    async outline() {
        const reply = await call('outline');
        this.#stats(reply);
        return reply.entries || [];
    }

    free() {
        if (!this.#live) return;
        this.#live = false;
        call('close').catch(() => {});
    }
}

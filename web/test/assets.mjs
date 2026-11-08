// Checks for web/www/assets.js: a shared viewer reads its wasm module and its
// font bundle from a content-addressed URL listed in `assets.json`, and from
// beside the page when there is no manifest.
//
//     node web/test/assets.mjs

import assert from "node:assert/strict";

// A module-level cache of manifests is fine in a browser and wrong in a test
// that wants several pages, so each case imports a fresh copy.
let generation = 0;
async function fresh() {
    return import(`../www/assets.js?g=${++generation}`);
}

const MANIFEST = {
    "pdf_web.wasm": "https://share.qdat.net/a/pdf_web.9f2c1a4b.wasm",
    "standard_fonts.br": "https://share.qdat.net/a/standard_fonts.4d2e8b17.br",
};

function stub(handler) {
    const calls = [];
    globalThis.fetch = async (url, opts) => {
        calls.push(String(url));
        return handler(String(url), opts || {});
    };
    return calls;
}

function json(body) {
    return { ok: true, status: 200, text: async () => JSON.stringify(body) };
}

// 1. A share: both files come from /a/, not from beside the page.
{
    const a = await fresh();
    const calls = stub(() => json(MANIFEST));
    const wasm = await a.resolve("pdf_web.wasm", "https://share.qdat.net/s/abc/f/site/index.html");
    const fonts = await a.resolve("standard_fonts.br", "https://share.qdat.net/s/abc/f/site/index.html");
    assert.equal(wasm, MANIFEST["pdf_web.wasm"]);
    assert.equal(fonts, MANIFEST["standard_fonts.br"]);
    assert.deepEqual(calls, ["https://share.qdat.net/s/abc/f/site/assets.json"], "one manifest fetch, cached");
    assert.deepEqual(await a.listed("https://share.qdat.net/s/abc/f/site/index.html"), [
        "pdf_web.wasm",
        "standard_fonts.br",
    ]);
}

// 2. A plain checkout: no manifest, the files are siblings.
{
    const a = await fresh();
    stub(() => ({ ok: false, status: 404, text: async () => "" }));
    assert.equal(
        await a.resolve("pdf_web.wasm", "http://127.0.0.1:8000/web/www/index.html"),
        "http://127.0.0.1:8000/web/www/pdf_web.wasm",
    );
}

// 3. A name the manifest does not list stays local (an app.js, a test page).
{
    const a = await fresh();
    stub(() => json(MANIFEST));
    assert.equal(
        await a.resolve("doc.js", "https://share.qdat.net/s/abc/f/site/index.html"),
        "https://share.qdat.net/s/abc/f/site/doc.js",
    );
}

// 4. A broken manifest is not fatal, and a non-network URL is not followed.
{
    const a = await fresh();
    stub(() => ({ ok: true, status: 200, text: async () => "{ not json" }));
    assert.equal(
        await a.resolve("pdf_web.wasm", "https://share.qdat.net/s/abc/f/site/index.html"),
        "https://share.qdat.net/s/abc/f/site/pdf_web.wasm",
    );

    const b = await fresh();
    stub(() => json({ "pdf_web.wasm": "javascript:alert(1)" }));
    assert.equal(
        await b.resolve("pdf_web.wasm", "https://share.qdat.net/s/abc/f/site/index.html"),
        "https://share.qdat.net/s/abc/f/site/pdf_web.wasm",
    );
}

// 5. No `document` (node, a worker): resolve still needs a base, and the
//    default one is harmless.
{
    const a = await fresh();
    stub(() => json({}));
    const url = await a.resolve("pdf_web.wasm");
    assert.match(url, /^http:\/\/localhost\/pdf_web\.wasm$/, url);
}

console.log("assets.js: ok");

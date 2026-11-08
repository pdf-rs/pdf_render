// The browser half of sealing, without a browser: node's WebCrypto is the same
// API the page uses, and `fetch` is stubbed with an in-memory Range server, so
// this exercises the real code path in `www/seal.js`.
//
//   node web/test/seal.mjs
//
// The vector is pinned in Rust as well (`agent/src/tools/seal.rs`,
// `interop_vector_is_what_the_browser_expects`): if either side changes the
// format, one of the two tests fails instead of a link silently going dark.

import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { Sealed, keyFromFragment, parseHeader, HEADER, TAG } from "../www/seal.js";

const here = dirname(fileURLToPath(import.meta.url));
const www = join(here, "..", "www");

let failures = 0;
function check(name, ok, detail = "") {
    if (ok) return console.log(`ok   ${name}`);
    failures++;
    console.log(`FAIL ${name}${detail ? `: ${detail}` : ""}`);
}

// ---------------------------------------------------------------- vector

const VECTOR =
    "U1lTSFNFQUwBAAAAAAAAAEBBQkNERUZHSElKS0xNTk87AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAEkNNQbY2s24ksY8XUNnNwy26zWmK91nVQp93NueszYnVIaUzFY+l2jKrlP1wo+qv7gzHZTwgMEAX36AvwPu6+QqWeKNuFwMuzhhAK5zOmMyF00TatVo0ik6H6Q=";
const PLAIN =
    "%PDF-1.7\n1 0 obj\n<< /Type /Catalog >>\nendobj\ntrailer\n%%EOF\n";
// key 00..1f, IV 40..4f — the fixed vector from the Rust test.
const KEY = new Uint8Array(32).map((_, i) => i);

const sealedBytes = new Uint8Array(Buffer.from(VECTOR, "base64"));
check("vector is one header, the payload and a tag", sealedBytes.length === HEADER + PLAIN.length + TAG);

const header = parseHeader(sealedBytes);
check("vector header length is the plaintext length", header.length === PLAIN.length);
check("vector header IV", header.iv.every((b, i) => b === 0x40 + i));

// ---------------------------------------------------------------- a fake server

/** A `fetch` that serves one file with Range support, and 404s everything
 *  else — the smallest thing that behaves like the share service. */
function server(files) {
    const urls = Object.keys(files);
    const log = [];
    const fetch = async (url, opts = {}) => {
        log.push(url);
        const body = files[url];
        if (!body) return new Response("not found", { status: 404 });
        const range = opts.headers?.Range;
        if (!range) {
            return new Response(body, { status: 200, headers: { "content-length": String(body.length) } });
        }
        const [, from, to] = /bytes=(\d+)-(\d+)/.exec(range);
        const start = Number(from), end = Math.min(Number(to) + 1, body.length);
        return new Response(body.subarray(start, end), {
            status: 206,
            headers: { "content-range": `bytes ${start}-${end - 1}/${body.length}` },
        });
    };
    return { fetch, log, urls };
}

const KEY_FRAGMENT = "#k=" + Buffer.from(KEY).toString("base64url");
check("the key survives the fragment", keyFromFragment(KEY_FRAGMENT)?.[0] === 0);
check("a fragment without a key is null", keyFromFragment("#page=2") === null);
check("a truncated key is refused", keyFromFragment("#k=AAAA") === null);

// ---------------------------------------------------------------- range reading

const fake = server({ "http://share.test/nas.pdf.sealed": sealedBytes });
globalThis.fetch = fake.fetch;

const sealed = await Sealed.open("http://share.test/nas.pdf", KEY);
check("the payload length comes from the header", sealed.length === PLAIN.length);
check("the sealed name is what it reads", sealed.url.endsWith(".sealed"), sealed.url);

const whole = await sealed.read(0, PLAIN.length);
check("the whole file", Buffer.from(whole).toString() === PLAIN, Buffer.from(whole).toString());

// Every offset, every length: the arithmetic that has to be right.
let bad = null;
for (let start = 0; start < PLAIN.length && !bad; start++) {
    for (let len = 1; len <= PLAIN.length - start; len++) {
        const got = Buffer.from(await sealed.read(start, len)).toString();
        if (got !== PLAIN.slice(start, start + len)) {
            bad = `${start}+${len}: ${JSON.stringify(got)}`;
            break;
        }
    }
}
check("every range decrypts on its own", bad === null, bad);

// And the tag, for the case where the whole file is in hand.
const again = Buffer.from(await sealed.whole()).toString();
check("whole() checks the tag and returns the file", again === PLAIN);

// A wrong key is refused, not silently garbage.
const wrong = new Uint8Array(KEY);
wrong[0] ^= 1;
const bad1 = await Sealed.open("http://share.test/nas.pdf", wrong);
let refused = false;
try {
    await bad1.whole();
} catch {
    refused = true;
}
check("a wrong key fails the tag", refused);

// Without a key there is nothing to open with.
let noKey = false;
try {
    await Sealed.open("http://share.test/nas.pdf", null);
} catch {
    noKey = true;
}
check("a sealed file without a key is an error", noKey);

// ---------------------------------------------------------------- the page

const html = await readFile(join(www, "index.html"), "utf8");
check("the page imports the sealed reader", /from '\.\/seal\.js'/.test(html));
check("the page takes the key from the fragment", /keyFromFragment\(location\.hash\)/.test(html));

const doc = await readFile(join(www, "doc.js"), "utf8");
check("doc.js reads bytes through a source", /this\.#source\.read\(start, len\)/.test(doc));
check("doc.js takes the source, not a url", /constructor\(source\)/.test(doc) && /pdf_doc_new\(source\.length\)/.test(doc));
check("doc.js can still fetch a plain file", /class HttpSource/.test(doc));

console.log(failures ? `\n${failures} check(s) failed` : "\nall checks passed");
process.exit(failures ? 1 : 0);

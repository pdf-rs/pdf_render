// Sealed files: the browser half of `SYSHSEAL` (see agent/src/tools/seal.rs
// for the format and why it is a stream cipher).
//
// A share can carry files that the server must not be able to read. They are
// stored as `<name>.sealed` — a 64-byte header, the ciphertext, a tag — and
// the key travels in the link's `#fragment`, which the browser never sends:
//
//     https://share.qdat.net/s/<id>/f/site/index.html?pdf=nas.pdf#k=BASE64URL
//
// Everything here is WebCrypto, so nothing has to be decompiled to see that no
// key leaves the page. AES-CTR is seekable: keystream position == plaintext
// offset, so a page's bytes decrypt on their own, in one range request, which
// is what makes sealing a range-read PDF worth doing at all.

const MAGIC = "SYSHSEAL";
const VERSION = 1;
export const HEADER = 64;
export const TAG = 32;
const BLOCK = 16;

const ENC_LABEL = "syshseal/enc/v1";
const MAC_LABEL = "syshseal/mac/v1";

const enc = new TextEncoder();

/** `#k=…` (or `k=…`) out of a fragment, as 32 raw bytes, or null. */
export function keyFromFragment(hash) {
    for (const part of (hash || "").replace(/^#/, "").split("&")) {
        if (!part.startsWith("k=")) continue;
        const key = base64urlDecode(part.slice(2));
        if (key && key.length === 32) return key;
    }
    return null;
}

export function base64urlDecode(text) {
    const t = text.replace(/-/g, "+").replace(/_/g, "/");
    const pad = t.length % 4 ? "=".repeat(4 - (t.length % 4)) : "";
    try {
        const bin = atob(t + pad);
        const out = new Uint8Array(bin.length);
        for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
        return out;
    } catch {
        return null;
    }
}

function cat(...parts) {
    const len = parts.reduce((n, p) => n + p.length, 0);
    const out = new Uint8Array(len);
    let at = 0;
    for (const p of parts) { out.set(p, at); at += p.length; }
    return out;
}

/** The two keys that come out of the one in the link, exactly as `seal.rs`
 *  derives them: SHA-256 of a label followed by the key. */
async function deriveKeys(key) {
    const subtle = crypto.subtle;
    const [encKey, macKey] = await Promise.all([
        subtle.digest("SHA-256", cat(enc.encode(ENC_LABEL), key)),
        subtle.digest("SHA-256", cat(enc.encode(MAC_LABEL), key)),
    ]);
    const [aes, hmac] = await Promise.all([
        subtle.importKey("raw", encKey, "AES-CTR", false, ["decrypt"]),
        subtle.importKey("raw", macKey, { name: "HMAC", hash: "SHA-256" }, false, ["verify"]),
    ]);
    return { aes, hmac };
}

/** The counter block for the 16-byte block that starts at `block`: the IV plus
 *  `block`, as a 128-bit big-endian integer (AES-CTR increments it that way). */
function counterAt(iv, block) {
    let c = 0n;
    for (const b of iv) c = (c << 8n) | BigInt(b);
    c = (c + BigInt(block)) & ((1n << 128n) - 1n);
    const out = new Uint8Array(16);
    for (let i = 15; i >= 0; i--) { out[i] = Number(c & 0xffn); c >>= 8n; }
    return out;
}

/** One HTTP range, as bytes. A server that ignores `Range` and sends the whole
 *  file is handled by taking the window we asked for. */
async function range(url, from, to) {
    const response = await fetch(url, { headers: { Range: `bytes=${from}-${to - 1}` } });
    if (response.status === 200) {
        const all = new Uint8Array(await response.arrayBuffer());
        return all.subarray(from, Math.min(to, all.length));
    }
    if (response.status !== 206) throw new Error(`${url}: HTTP ${response.status}`);
    return new Uint8Array(await response.arrayBuffer());
}

/** A sealed file, opened for reading.
 *
 *  `Sealed.open(url, key)` takes the plain name (`nas.pdf`); the sealed form it
 *  actually reads is `<url>.sealed`, found on the first request, so a link and
 *  a viewer stay readable while the bytes on the server are not. */
export class Sealed {
    #url = "";
    #key = null;
    #keys = null;
    #iv = null;
    /** Plaintext bytes. Offsets in a page table are these. */
    length = 0;

    static async open(url, key) {
        if (!key) throw new Error("sealed file without a key");
        const sealed = new Sealed();
        sealed.#key = key;
        sealed.#keys = await deriveKeys(key);
        const found = await sealed.#resolve(url);
        sealed.#url = found.url;
        const { iv, length } = parseHeader(found.header);
        sealed.#iv = iv;
        sealed.length = length;
        return sealed;
    }

    /** The sealed form of `url`, as the first request. The link names the
     *  plain file (`nas.pdf`) because that is what the page asked for; what is
     *  on the server is `nas.pdf.sealed`. A plain file is tried second, so a
     *  link that carries a key for a document that is not sealed still opens. */
    async #resolve(url) {
        const candidates = url.endsWith(".sealed") ? [url] : [`${url}.sealed`, url];
        let firstError = null;
        for (const candidate of candidates) {
            try {
                const header = await range(candidate, 0, HEADER);
                if (new TextDecoder().decode(header.subarray(0, 8)) === MAGIC) {
                    return { url: candidate, header };
                }
            } catch (e) {
                firstError ??= e;
            }
        }
        throw firstError ?? new Error(`${url}: no sealed form, and no plain file either`);
    }

    /** Plaintext bytes `[start, start + len)`, decrypted from the ciphertext
     *  range they sit in. Block alignment is this function's business: AES-CTR
     *  only starts on a block boundary, so it may read up to 15 bytes before
     *  what was asked for and drops them. */
    async read(start, len) {
        if (len === 0) return new Uint8Array(0);
        const end = Math.min(start + len, this.length);
        if (end <= start) return new Uint8Array(0);
        const aligned = start - (start % BLOCK);
        const bytes = await range(this.#url, HEADER + aligned, HEADER + end);
        const plain = await crypto.subtle.decrypt(
            { name: "AES-CTR", counter: counterAt(this.#iv, aligned / BLOCK), length: 128 },
            this.#keys.aes,
            bytes,
        );
        return new Uint8Array(plain, start - aligned, end - start);
    }

    /** The whole file, tag checked. Only worth it when every byte is wanted:
     *  a page-by-page reader never calls this. */
    async whole() {
        const body = await range(this.#url, 0, HEADER + this.length);
        const tag = await range(this.#url, HEADER + this.length, HEADER + this.length + TAG);
        const ok = await crypto.subtle.verify(
            "HMAC",
            this.#keys.hmac,
            tag,
            cat(body),
        );
        if (!ok) throw new Error("tag mismatch: wrong key, or the file was changed");
        const plain = await crypto.subtle.decrypt(
            { name: "AES-CTR", counter: counterAt(this.#iv, 0), length: 128 },
            this.#keys.aes,
            body.subarray(HEADER),
        );
        return new Uint8Array(plain);
    }

    get url() {
        return this.#url;
    }
}

export function parseHeader(bytes) {
    if (bytes.length < HEADER) throw new Error(`sealed header is ${bytes.length} bytes, wanted ${HEADER}`);
    if (new TextDecoder().decode(bytes.subarray(0, 8)) !== MAGIC) {
        throw new Error("not a sealed file (bad magic)");
    }
    if (bytes[8] !== VERSION) throw new Error(`sealed version ${bytes[8]}, this page reads ${VERSION}`);
    const iv = bytes.subarray(16, 32);
    const view = new DataView(bytes.buffer, bytes.byteOffset + 32, 8);
    return { iv, length: Number(view.getBigUint64(0, true)) };
}

// Where the viewer's own files live.
//
// In a share, the files that are the same in every bundle — the wasm module,
// the standard fonts — are not in the bundle at all. `share` uploads them once
// to a content-addressed URL on the same host (`/a/pdf_web.9f2c1a4b.wasm`)
// and writes an `assets.json` next to the page that says which URL is which:
//
//     { "pdf_web.wasm": "https://share.qdat.net/a/pdf_web.9f2c1a4b.wasm" }
//
// That is what makes the URL immutable: the name changes when the bytes do. In
// a plain checkout there is no manifest and every file is simply beside the
// page, so `resolve` falls back to that.

const CACHE = new Map(); // manifest URL -> Promise<manifest>

function pageBase() {
    return typeof document !== "undefined" && document.baseURI
        ? document.baseURI
        : "http://localhost/";
}

/** The manifest in `base`'s directory, fetched once. Missing or broken: {} —
 *  a page without a manifest is an ordinary page, not an error. */
function manifestAt(base) {
    const url = new URL("assets.json", base).href;
    if (!CACHE.has(url)) {
        CACHE.set(
            url,
            (async () => {
                try {
                    const response = await fetch(url, { cache: "no-cache" });
                    if (!response.ok) return {};
                    const text = await response.text();
                    if (!text.trim()) return {};
                    const json = JSON.parse(text);
                    return json && typeof json === "object" ? json : {};
                } catch {
                    return {};
                }
            })(),
        );
    }
    return CACHE.get(url);
}

/** The URL for `name` (`pdf_web.wasm`), from the manifest when it lists it,
 *  else the file beside the page. */
export async function resolve(name, base = pageBase()) {
    const manifest = await manifestAt(base);
    const href = manifest[name];
    if (typeof href === "string" && href) {
        try {
            const url = new URL(href, base);
            // Only ever a network URL: a manifest is fetched from the same
            // origin as the page, and nothing else can run here anyway, but a
            // `javascript:` or `data:` entry should not become a script src.
            if (url.protocol === "https:" || url.protocol === "http:") return url.href;
        } catch {}
    }
    return new URL(`./${name}`, base).href;
}

/** Names the manifest lists, for diagnostics. */
export async function listed(base = pageBase()) {
    return Object.keys(await manifestAt(base)).sort();
}

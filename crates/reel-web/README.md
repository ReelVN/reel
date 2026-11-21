# Reel browser preview

Embed a live Aside snippet in a documentation page or a small playground. The preview runs the same compiler, runtime, UI projection, and WGPU renderer as Reel. Each run starts a new session on the canvas. The host page supplies any assets the snippet uses.

## Build

Install the `wasm32-unknown-unknown` target for Reel's pinned Rust toolchain and `wasm-bindgen-cli` **0.2.129**. Run from the repository root.

```sh
rustup target add wasm32-unknown-unknown
cargo build --locked --release --target wasm32-unknown-unknown -p reel-web
wasm-bindgen target/wasm32-unknown-unknown/release/reel_web.wasm --target web --remove-name-section --out-name reel_web --out-dir target/reel-web
```

If Cargo uses a custom target directory, take `reel_web.wasm` from that directory. Copy `web/preview.js` and `web/preview.d.ts` next to the generated `reel_web.js` and `reel_web_bg.wasm`. Serve them over HTTPS or localhost, with `application/wasm` for the WASM file. A WebGPU adapter is required.

On documentation pages, mount the preview when the reader chooses to run a snippet. The module loads once per page and can serve multiple canvases. Serve the WASM file with gzip or Brotli compression.

## Embed

Give the canvas its display size in CSS. The preview runs while its page has focus. The canvas receives keyboard input while it has focus.

```js
import { mountPreview } from "./reel/preview.js";

const preview = await mountPreview(document.querySelector("canvas"), {
    assets: { "fonts/body.ttf": new URL("./fonts/body.ttf", import.meta.url) },
    onError: (error) => console.error(error),
});

const result = preview.run('fn main() { stage.replace(text("Hello, Reel")); }');
if (result.status === "compile_error") showDiagnostics(result.diagnostics);
```

Treat the editor contents as `src/main.aside` with a `main` function. Compilation uses `build.target == "web"`. A positioned diagnostic includes a zero-based line, UTF-16 column, and UTF-8 byte offset at both ends. Errors without a source range have null positions. Use `onError` for faults during playback.

Asset keys match the logical paths used in Aside, and their values may be URLs or bytes. `fonts/body.ttf` is the default text font when supplied. A script may choose its own fonts with `game.configure`. Call `preview.dispose()` when removing the canvas.

Effects declared in the snippet use the same WGSL preparation as desktop export. Prepared effect assets belong to the current preview session.

Use the generated module's `WebAnalysis` from a Web Worker for editor queries. Call `setSource(source)` before requesting `completion(line, character)`, `hover(line, character)`, or `diagnostics()`. Queries return JSON in LSP format with zero-based UTF-16 positions. The analysis runs on the current snippet and can start without a canvas or WebGPU.

For a runnable editor example, copy `web/example.html` into the bundle directory and put the font and its `OFL.txt` from [the arcade example](../../examples/author_arcade/resources/fonts) in its `assets/` subdirectory. The browser preview covers source snippets and supplied image/font assets. Web audio, video and persistent saves are outside this embedding API.

# Reel 浏览器预览

[English](README.md)

可以在文档页面或小型游乐场里嵌入可运行的 Aside 片段。预览沿用 Reel 的编译器、运行时、UI 处理和 WGPU 渲染器。每次运行时，画布都会开启一个新的会话。片段所用的资源由宿主页面提供。

## 构建

为 Reel 指定的 Rust 工具链安装 `wasm32-unknown-unknown` 目标，并安装 `wasm-bindgen-cli` **0.2.129**。在仓库根目录执行以下命令。

```sh
rustup target add wasm32-unknown-unknown
cargo build --locked --release --target wasm32-unknown-unknown -p reel-web
wasm-bindgen target/wasm32-unknown-unknown/release/reel_web.wasm --target web --remove-name-section --out-name reel_web --out-dir target/reel-web
```

如果 Cargo 使用自定义构建目录，从该目录取出 `reel_web.wasm`。将 [`web/preview.js`](web/preview.js) 和 [`web/preview.d.ts`](web/preview.d.ts) 复制到生成的 `reel_web.js` 和 `reel_web_bg.wasm` 旁。通过 HTTPS 或 localhost 提供这些文件，把 WASM 文件的 MIME 类型设为 `application/wasm`。浏览器需要可用的 WebGPU 适配器。

文档页面可以等读者点击运行后再挂载预览。模块在每个页面只加载一次，可供多个画布使用。建议通过 gzip 或 Brotli 压缩传输 WASM 文件。

## 嵌入

用 CSS 控制画布的显示尺寸。页面获得焦点时，预览会继续运行。画布本身获得焦点时，会接收键盘输入。

```js
import { mountPreview } from "./reel/preview.js";

const preview = await mountPreview(document.querySelector("canvas"), {
    assets: { "fonts/body.ttf": new URL("./fonts/body.ttf", import.meta.url) },
    onError: (error) => console.error(error),
});

const result = preview.run('fn main() { stage.replace(text("Hello, Reel")); }');
if (result.status === "compile_error") showDiagnostics(result.diagnostics);
```

编辑器里的代码按 `src/main.aside` 编译，需提供 `main` 函数。编译时 `build.target == "web"`。带源码范围的诊断在起点和终点都提供从零开始的行号、UTF-16 列号和 UTF-8 字节偏移。缺少源码范围的诊断以 null 表示位置。播放过程中的错误会交给 `onError`。

资源键名与 Aside 中使用的逻辑路径一致，值可以是 URL 或字节数据。提供 `fonts/body.ttf` 后，它会作为默认字体。脚本也可以用 `game.configure` 指定其他字体。移除画布时调用 `preview.dispose()`。

片段里声明的效果沿用桌面导出的 WGSL 编译流程。编译出的效果资源随本次预览会话使用。

可以在 Web Worker 中使用生成模块的 `WebAnalysis` 为编辑器提供语言服务。先调用 `setSource(source)`，再请求 `completion(line, character)`、`hover(line, character)` 或 `diagnostics()`。结果采用 LSP 的 JSON 格式，位置使用从零开始的 UTF-16 列号。分析以当前片段为输入，无需画布或 WebGPU 即可启动。

要运行编辑器示例，将 [`web/example.html`](web/example.html) 复制到生成目录，再将字体及其 `OFL.txt` 放入该目录的 `assets/` 子目录。浏览器预览支持源码片段以及宿主提供的图片和字体。字体与许可证见[街机示例](../../examples/author_arcade/resources/fonts)。

音频、视频和持久存档请用原生播放器测试。

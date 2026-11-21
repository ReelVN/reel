import init, { WebPreview } from "./reel_web.js";

let initialization;

function initialize() {
    initialization ??= init().catch((error) => {
        initialization = undefined;
        throw error;
    });
    return initialization;
}

async function loadAsset(source, signal) {
    if (source instanceof Uint8Array) return source;
    const response = await fetch(source, { signal });
    if (!response.ok) {
        throw new Error(`Could not load preview asset ${source} (HTTP ${response.status}).`);
    }
    return new Uint8Array(await response.arrayBuffer());
}

const buttons = [1, 4, 2];
const navigationKeys = new Set(["Space", "Enter", "ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"]);

export async function mountPreview(canvas, options = {}) {
    const { assets = {}, locale = "en", onError = console.error, signal } = options;
    signal?.throwIfAborted();
    const [, inputs] = await Promise.all([
        initialize(),
        Promise.all(Object.entries(assets).map(async ([path, source]) => [
            path, await loadAsset(source, signal),
        ])),
    ]);
    signal?.throwIfAborted();
    const player = await WebPreview.create(canvas, locale);

    const lifetime = new AbortController();
    const view = canvas.ownerDocument.defaultView;
    const keys = new Set();
    let heldButtons = 0;
    let animation;
    let observer;
    let addedTabIndex = false;
    let running = false;
    let size = {};

    function stopFrames() {
        running = false;
        if (animation !== undefined) view.cancelAnimationFrame(animation);
        animation = undefined;
    }

    function dispose() {
        if (lifetime.signal.aborted) return;
        lifetime.abort();
        stopFrames();
        observer?.disconnect();
        if (addedTabIndex) canvas.removeAttribute("tabindex");
        player.free();
    }

    function resize(width, height) {
        if (width <= 0 || height <= 0) return;
        const scale = view.devicePixelRatio;
        const pixels = {
            width: Math.max(1, Math.round(width * scale)),
            height: Math.max(1, Math.round(height * scale)),
            scale,
        };
        if (pixels.width !== size.width || pixels.height !== size.height || scale !== size.scale) {
            player.resize(pixels.width, pixels.height, pixels.scale);
            size = pixels;
        }
    }

    function report(error) {
        stopFrames();
        onError(error);
    }

    function frame() {
        animation = undefined;
        try {
            if (player.frame()) {
                animation = view.requestAnimationFrame(frame);
            } else {
                stopFrames();
            }
        } catch (error) {
            report(error);
        }
    }

    function pointerButtons(next) {
        if (heldButtons === next) return;
        for (const [button, bit] of buttons.entries()) {
            if ((heldButtons ^ next) & bit) player.pointerButton(button, (next & bit) !== 0);
        }
        heldButtons = next;
    }

    function movePointer(event) {
        const rect = canvas.getBoundingClientRect();
        if (rect.width === 0 || rect.height === 0) return;
        const x = (event.clientX - rect.left) * canvas.offsetWidth / rect.width - canvas.clientLeft;
        const y = (event.clientY - rect.top) * canvas.offsetHeight / rect.height - canvas.clientTop;
        player.pointerMove(x, y);
    }

    function pointer(event) {
        if (!running || !event.isPrimary) return;
        movePointer(event);
        pointerButtons(event.buttons);
    }

    function cancelPointer() {
        if (!running) return;
        player.pointerCancel();
        heldButtons = 0;
    }

    function wheel(event) {
        if (!running || event.ctrlKey || canvas.ownerDocument.activeElement !== canvas) return;
        event.preventDefault();
        movePointer(event);
        const units = event.deltaMode;
        const xScale = units === WheelEvent.DOM_DELTA_PAGE ? canvas.clientWidth
            : units === WheelEvent.DOM_DELTA_LINE ? 16 : 1;
        const yScale = units === WheelEvent.DOM_DELTA_PAGE ? canvas.clientHeight
            : units === WheelEvent.DOM_DELTA_LINE ? 16 : 1;
        player.scroll(event.deltaX * xScale, event.deltaY * yScale);
    }

    function key(event) {
        if (!running || !event.code || event.isComposing) return;
        if (navigationKeys.has(event.code)) event.preventDefault();
        if (event.type === "keydown") {
            if (!keys.has(event.code)) {
                keys.add(event.code);
                player.key(event.code, event.key, true);
            }
        } else if (keys.delete(event.code)) {
            player.key(event.code, event.key, false);
        }
    }

    function focus(focused) {
        if (!running) return;
        player.focus(focused);
        if (!focused) {
            keys.clear();
            if (heldButtons) cancelPointer();
        }
    }

    function listen(target, name, handler, eventOptions) {
        target.addEventListener(name, (event) => {
            try {
                handler(event);
            } catch (error) {
                report(error);
            }
        }, { ...eventOptions, signal: lifetime.signal });
    }

    function requireOpen() {
        if (lifetime.signal.aborted) throw new Error("The Reel preview has been disposed.");
    }

    try {
        signal?.throwIfAborted();
        for (const [path, bytes] of inputs) player.setAsset(path, bytes);
        if (!canvas.hasAttribute("tabindex")) {
            canvas.tabIndex = 0;
            addedTabIndex = true;
        }
        resize(canvas.clientWidth, canvas.clientHeight);
        observer = new ResizeObserver(([entry]) => {
            try {
                resize(entry.contentRect.width, entry.contentRect.height);
            } catch (error) {
                report(error);
            }
        });
        observer.observe(canvas);
        listen(view, "resize", () => resize(canvas.clientWidth, canvas.clientHeight));
        listen(canvas, "pointerdown", (event) => {
            if (!running || !event.isPrimary) return;
            canvas.focus({ preventScroll: true });
            if (event.isTrusted) canvas.setPointerCapture(event.pointerId);
            pointer(event);
        });
        for (const type of ["pointermove", "pointerup"]) listen(canvas, type, pointer);
        listen(canvas, "pointerleave", () => {
            if (running && !heldButtons) player.pointerMove(-1, -1);
        });
        listen(canvas, "pointercancel", (event) => { if (event.isPrimary) cancelPointer(); });
        listen(canvas, "lostpointercapture", () => { if (heldButtons) cancelPointer(); });
        listen(canvas, "wheel", wheel, { passive: false });
        listen(canvas, "keydown", key);
        listen(canvas, "keyup", key);
        listen(canvas, "focus", () => focus(true));
        listen(canvas, "blur", () => focus(false));
        listen(view, "focus", () => {
            if (canvas.ownerDocument.activeElement === canvas) focus(true);
        });
        listen(view, "blur", () => focus(false));
        if (signal) listen(signal, "abort", dispose);
    } catch (error) {
        dispose();
        throw error;
    }

    return {
        run(source) {
            requireOpen();
            stopFrames();
            keys.clear();
            heldButtons = 0;
            const result = JSON.parse(player.run(source));
            if (result.status === "running") {
                player.focus(canvas.ownerDocument.activeElement === canvas);
                running = true;
                animation = view.requestAnimationFrame(frame);
            }
            return result;
        },
        async setAsset(path, source) {
            requireOpen();
            const bytes = await loadAsset(source, lifetime.signal);
            requireOpen();
            player.setAsset(path, bytes);
        },
        dispose,
    };
}

/** Binary data or a URL resolved from the embedding page. */
export type PreviewAsset = Uint8Array | URL | string;

export interface PreviewOptions {
    /** Asset input keyed by the same paths used in Aside source. */
    assets?: Record<string, PreviewAsset>;
    locale?: string;
    /** Reports asynchronous playback failures. */
    onError?: (error: unknown) => void;
    /** Aborting cancels loading and disposes the preview. */
    signal?: AbortSignal;
}

export interface SourcePosition {
    line: number;
    utf16Column: number;
    byteOffset: number;
}

export type PreviewResult =
    | { status: "running" }
    | { status: "compile_error"; diagnostics: PreviewDiagnostic[] }
    | { status: "runtime_error"; message: string };

export interface PreviewDiagnostic {
    source: string;
    code: string;
    message: string;
    start: SourcePosition | null;
    end: SourcePosition | null;
}

export interface Preview {
    /** Compiles the supplied src/main.aside and starts a fresh session on success. */
    run(source: string): PreviewResult;
    /** Supplies or replaces an asset before the next run. */
    setAsset(path: string, source: PreviewAsset): Promise<void>;
    dispose(): void;
}

/** Mounts a browser player on a canvas whose display size is controlled by CSS. */
export function mountPreview(canvas: HTMLCanvasElement, options?: PreviewOptions): Promise<Preview>;

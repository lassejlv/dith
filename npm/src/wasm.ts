import { initSync } from "../wasm/dith.js";

/** Where to load the WebAssembly module from: a URL, a fetch Response, bytes, or a compiled Module. */
export type WasmInput = string | URL | Request | Response | ArrayBuffer | ArrayBufferView | WebAssembly.Module;

let compiled: Promise<WebAssembly.Module> | undefined;
let instantiated: WebAssembly.Module | undefined;

async function compileResponse(response: Response): Promise<WebAssembly.Module> {
  if (!response.ok) throw new Error("fetching dith_bg.wasm failed with HTTP " + response.status);
  const fallback = response.clone();
  try {
    return await WebAssembly.compileStreaming(response);
  } catch {
    // Wrong MIME type or no streaming support: compile from the bytes instead.
    return WebAssembly.compile(await fallback.arrayBuffer());
  }
}

async function compileFrom(input: WasmInput | undefined): Promise<WebAssembly.Module> {
  if (input instanceof WebAssembly.Module) return input;
  if (typeof Response !== "undefined" && input instanceof Response) return compileResponse(input);
  if (input instanceof ArrayBuffer || ArrayBuffer.isView(input)) return WebAssembly.compile(input as BufferSource);
  let url: URL | Request;
  if (input === undefined) url = new URL("../wasm/dith_bg.wasm", import.meta.url);
  else if (typeof input === "string") url = new URL(input, import.meta.url);
  else if (input instanceof URL || input instanceof Request) url = input;
  else throw new TypeError("unsupported wasm input");
  if (url instanceof URL && url.protocol === "file:") {
    const fsModule = "node:fs/promises";
    const fs = (await import(/* @vite-ignore */ fsModule)) as { readFile(path: URL): Promise<Uint8Array<ArrayBuffer>> };
    return WebAssembly.compile(await fs.readFile(url));
  }
  return compileResponse(await fetch(url));
}

/**
 * Compile the WebAssembly module once (streaming where possible). Later calls return the same
 * Module, which workers instantiate without fetching it again.
 */
export function compileModule(input?: WasmInput | Promise<WasmInput>): Promise<WebAssembly.Module> {
  compiled ??= Promise.resolve(input).then(compileFrom);
  return compiled;
}

/** Instantiate the compiled module in this thread (idempotent). */
export function instantiate(module: WebAssembly.Module): void {
  if (instantiated) return;
  initSync({ module });
  instantiated = module;
}

export function isInstantiated(): boolean {
  return instantiated !== undefined;
}

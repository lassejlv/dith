import { DithError } from "./errors.js";
import type { SourcePayload } from "./engine.js";
import type { GeneratorSpec, ImageInput, RawImage } from "./types.js";

export interface PreparedInput {
  payload: SourcePayload;
  transfer: Transferable[];
  /** Pixel count when known before decoding (used to keep tiny jobs on the main thread). */
  pixels?: number;
  procedural: boolean;
  name: string;
}

export interface InputOptions {
  /** Label for reports and cliCommand (defaults to the file or URL name). */
  name?: string;
  /**
   * Transfer (zero-copy) the caller's ArrayBuffer, typed array, ImageData or ImageBitmap to the
   * worker. The caller's object becomes detached or closed. Default false.
   */
  transfer?: boolean;
}

const has = (name: string): boolean => typeof (globalThis as Record<string, unknown>)[name] !== "undefined";
const isGenerator = (v: object): v is GeneratorSpec => typeof (v as { pattern?: unknown }).pattern === "string";
const isRaw = (v: object): v is RawImage =>
  ArrayBuffer.isView((v as { data?: unknown }).data) &&
  typeof (v as RawImage).width === "number" &&
  typeof (v as RawImage).height === "number";

function bytesInput(bytes: Uint8Array, name: string, owned: boolean): PreparedInput {
  return { payload: { kind: "encoded", bytes, name }, transfer: owned ? [bytes.buffer] : [], procedural: false, name };
}

function bitmapInput(bitmap: ImageBitmap, name: string, owned: boolean): PreparedInput {
  return {
    payload: { kind: "bitmap", bitmap, name },
    transfer: owned ? [bitmap] : [],
    pixels: bitmap.width * bitmap.height,
    procedural: false,
    name,
  };
}

function lastSegment(url: string): string {
  const path = new URL(url, "http://local/").pathname;
  return decodeURIComponent(path.slice(path.lastIndexOf("/") + 1)) || "image";
}

/** Normalize any {@link ImageInput} into a payload the engine accepts. */
export async function prepareInput(input: ImageInput, options: InputOptions = {}): Promise<PreparedInput> {
  const transfer = options.transfer ?? false;
  if (typeof input === "string" || input instanceof URL) {
    const url = String(input);
    const response = await fetch(url);
    if (!response.ok)
      throw new DithError(
        "INVALID_INPUT",
        "fetching " + url + " failed with HTTP " + response.status,
        "Check the URL and CORS headers.",
      );
    return bytesInput(new Uint8Array(await response.arrayBuffer()), options.name ?? lastSegment(url), true);
  }
  if (input instanceof ArrayBuffer) return bytesInput(new Uint8Array(input), options.name ?? "input", transfer);
  if (has("Blob") && input instanceof Blob) {
    const name = options.name ?? ("name" in input && typeof input.name === "string" ? input.name : "input");
    return bytesInput(new Uint8Array(await input.arrayBuffer()), name, true);
  }
  if (input instanceof Uint8Array && !isRaw(input)) {
    const whole = input.byteOffset === 0 && input.byteLength === input.buffer.byteLength;
    return bytesInput(input, options.name ?? "input", transfer && whole);
  }
  if (has("ImageBitmap") && input instanceof ImageBitmap) return bitmapInput(input, options.name ?? "bitmap", transfer);
  if (has("OffscreenCanvas") && input instanceof OffscreenCanvas)
    return bitmapInput(input.transferToImageBitmap(), options.name ?? "canvas", true);
  if (has("HTMLImageElement") && input instanceof HTMLImageElement) {
    if (!input.complete) await input.decode();
    const bitmap = await createImageBitmap(input, { premultiplyAlpha: "none", colorSpaceConversion: "none" });
    return bitmapInput(bitmap, options.name ?? lastSegment(input.currentSrc || input.src || "image"), true);
  }
  if (has("HTMLCanvasElement") && input instanceof HTMLCanvasElement) {
    return bitmapInput(await createImageBitmap(input), options.name ?? "canvas", true);
  }
  if (typeof input === "object" && isGenerator(input)) {
    const spec: GeneratorSpec = { ...input, stops: input.stops ? [...input.stops] : undefined };
    return {
      payload: { kind: "generator", spec },
      transfer: [],
      pixels: (spec.width ?? 640) * (spec.height ?? 480),
      procedural: true,
      name: "generate:" + spec.pattern,
    };
  }
  if (typeof input === "object" && isRaw(input)) {
    const { width, height } = input;
    if (input.data.byteLength !== width * height * 4) {
      throw new DithError(
        "INVALID_PIXELS",
        "expected " + width + "x" + height + "x4 RGBA bytes",
        "Pass width * height * 4 bytes of RGBA8 pixel data.",
      );
    }
    const data = new Uint8Array(input.data.buffer, input.data.byteOffset, input.data.byteLength);
    const whole = data.byteOffset === 0 && data.byteLength === data.buffer.byteLength;
    const name = options.name ?? "pixels";
    return {
      payload: { kind: "rgba", data, width, height, name },
      transfer: transfer && whole ? [data.buffer] : [],
      pixels: width * height,
      procedural: false,
      name,
    };
  }
  throw new DithError(
    "INVALID_INPUT",
    "unsupported input",
    "Pass a File, Blob, URL, ArrayBuffer, Uint8Array, image, canvas, ImageBitmap, ImageData, {data, width, height} or a generator spec.",
  );
}

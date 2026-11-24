import { Source as WasmSource } from "../wasm/dith.js";
import { DithError, toPayload, type ErrorPayload } from "./errors.js";
import type { EncodedFormat, GeneratorSpec, TextFormat } from "./types.js";

/** A source in the form it crosses thread boundaries in. */
export type SourcePayload =
  | { kind: "encoded"; bytes: Uint8Array; name: string }
  | { kind: "rgba"; data: Uint8Array; width: number; height: number; name: string }
  | { kind: "bitmap"; bitmap: ImageBitmap; name: string }
  | { kind: "generator"; spec: GeneratorSpec };

export interface SourceInfo {
  id: number;
  width: number;
  height: number;
  procedural: boolean;
}

export type EngineFormat = EncodedFormat | TextFormat | "bitmap" | "rgba";

export interface RenderJob {
  recipe: string;
  format: EngineFormat;
  preview?: number;
}

export interface EngineOutput {
  format: EngineFormat;
  mime: string;
  width: number;
  height: number;
  preview: boolean;
  report: string;
  bytes?: Uint8Array<ArrayBuffer>;
  bitmap?: ImageBitmap;
}

export type EngineRequest =
  | { op: "open"; payload: SourcePayload }
  | { op: "render"; source: number; job: RenderJob }
  | { op: "original"; source: number; job: RenderJob }
  | { op: "close"; source: number }
  | { op: "oneshot"; payload: SourcePayload; job: RenderJob };

export type EngineResponse = { id: number; ok: true; value: unknown } | { id: number; ok: false; error: ErrorPayload };

const sources = new Map<number, WasmSource>();
let nextId = 1;

function bitmapPixels(bitmap: ImageBitmap): Uint8Array {
  const canvas = new OffscreenCanvas(bitmap.width, bitmap.height);
  const ctx = canvas.getContext("2d", { willReadFrequently: true });
  if (!ctx)
    throw new DithError(
      "UNSUPPORTED",
      "OffscreenCanvas 2D is not available",
      "Pass encoded bytes or raw RGBA pixels instead.",
    );
  ctx.drawImage(bitmap, 0, 0);
  bitmap.close();
  const data = ctx.getImageData(0, 0, canvas.width, canvas.height).data;
  return new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
}

function open(payload: SourcePayload): SourceInfo {
  let source: WasmSource;
  switch (payload.kind) {
    case "encoded":
      source = WasmSource.fromBytes(payload.bytes, payload.name);
      break;
    case "rgba":
      source = WasmSource.fromRgba(payload.data, payload.width, payload.height, payload.name);
      break;
    case "bitmap": {
      const { width, height } = payload.bitmap;
      source = WasmSource.fromRgba(bitmapPixels(payload.bitmap), width, height, payload.name);
      break;
    }
    case "generator": {
      const { spec } = payload;
      source = WasmSource.fromGenerator(
        JSON.stringify({
          pattern: spec.pattern,
          canvas_width: spec.width ?? 640,
          canvas_height: spec.height ?? 480,
          angle: spec.angle ?? 135,
          frequency: spec.frequency ?? 1,
          stops: spec.stops,
        }),
      );
      break;
    }
  }
  const id = nextId++;
  sources.set(id, source);
  return { id, width: source.width, height: source.height, procedural: source.procedural };
}

function get(id: number): WasmSource {
  const source = sources.get(id);
  if (!source) throw new DithError("INVALID_INPUT", "this source was disposed", "Create a new source.");
  return source;
}

async function toBitmap(bytes: Uint8Array<ArrayBuffer>, width: number, height: number): Promise<ImageBitmap> {
  if (typeof createImageBitmap === "undefined" || typeof ImageData === "undefined") {
    throw new DithError(
      "UNSUPPORTED",
      "ImageBitmap output needs a browser",
      "Use format 'rgba' or an encoded format in this runtime.",
    );
  }
  return createImageBitmap(
    new ImageData(new Uint8ClampedArray(bytes.buffer, bytes.byteOffset, bytes.byteLength), width, height),
  );
}

async function render(source: WasmSource, job: RenderJob): Promise<EngineOutput> {
  const raw = job.format === "bitmap" || job.format === "rgba";
  const out = source.render(job.recipe, raw ? "rgba" : job.format, job.preview);
  try {
    const bytes = out.takeBytes() as Uint8Array<ArrayBuffer>;
    const base = {
      format: job.format,
      mime: out.mime,
      width: out.width,
      height: out.height,
      preview: out.preview,
      report: out.report,
    };
    if (job.format === "bitmap") return { ...base, bitmap: await toBitmap(bytes, out.width, out.height) };
    return { ...base, bytes };
  } finally {
    out.free();
  }
}

async function original(source: WasmSource, job: RenderJob): Promise<EngineOutput> {
  const out = source.original(job.recipe, job.preview);
  try {
    const bytes = out.takeBytes() as Uint8Array<ArrayBuffer>;
    const base = {
      format: job.format,
      mime: out.mime,
      width: out.width,
      height: out.height,
      preview: out.preview,
      report: "",
    };
    if (job.format === "bitmap") return { ...base, bitmap: await toBitmap(bytes, out.width, out.height) };
    return { ...base, bytes };
  } finally {
    out.free();
  }
}

/** Run one request against this thread's engine. */
export async function dispatch(request: EngineRequest): Promise<unknown> {
  switch (request.op) {
    case "open":
      return open(request.payload);
    case "render":
      return render(get(request.source), request.job);
    case "original":
      return original(get(request.source), request.job);
    case "close":
      sources.get(request.source)?.free();
      sources.delete(request.source);
      return undefined;
    case "oneshot": {
      const { id } = open(request.payload);
      try {
        return await render(get(id), request.job);
      } finally {
        sources.get(id)?.free();
        sources.delete(id);
      }
    }
  }
}

/** Buffers and bitmaps in a value that can be transferred instead of copied. */
export function transferablesOf(value: unknown): Transferable[] {
  if (!value || typeof value !== "object") return [];
  const out = value as Partial<EngineOutput>;
  const list: Transferable[] = [];
  if (out.bytes) list.push(out.bytes.buffer);
  if (out.bitmap) list.push(out.bitmap);
  return list;
}

export async function respond(
  id: number,
  request: EngineRequest,
): Promise<{ response: EngineResponse; transfer: Transferable[] }> {
  try {
    const value = await dispatch(request);
    return { response: { id, ok: true, value }, transfer: transferablesOf(value) };
  } catch (error) {
    return { response: { id, ok: false, error: toPayload(error) }, transfer: [] };
  }
}

import type { AlgorithmId, ColorSpace, GeneratorId, HexColor, Recipe } from "./generated/schema.js";
import type { DitherOptions } from "./recipe.js";

/** Formats rendered as encoded image bytes. */
export type EncodedFormat = "png" | "webp" | "jpeg" | "gif" | "bmp" | "tiff";
/** Formats rendered as text (SVG markup, Unicode art). */
export type TextFormat = "svg" | "braille" | "ascii";
/** Every render output: an encoded file, text, an ImageBitmap, or raw RGBA pixels. */
export type RenderFormat = EncodedFormat | TextFormat | "bitmap" | "rgba";

/** The MIME type of each file format. */
export interface MimeTypes {
  png: "image/png";
  webp: "image/webp";
  jpeg: "image/jpeg";
  gif: "image/gif";
  bmp: "image/bmp";
  tiff: "image/tiff";
  svg: "image/svg+xml";
  braille: "text/plain;charset=utf-8";
  ascii: "text/plain;charset=utf-8";
}

/** A procedural source, like "dith generate <pattern>". */
export interface GeneratorSpec {
  pattern: GeneratorId;
  /** Canvas width in pixels (default 640). CLI: --canvas-width */
  width?: number;
  /** Canvas height in pixels (default 480). CLI: --canvas-height */
  height?: number;
  /** Direction or light angle in degrees (default 135). CLI: --angle */
  angle?: number;
  /** Pattern frequency or scale (default 1). CLI: --frequency */
  frequency?: number;
  /** Source gradient stops (default near-black to near-white). CLI: --stops */
  stops?: readonly HexColor[];
}

/** Raw RGBA8 pixels, row-major. */
export interface RawImage {
  data: Uint8Array | Uint8ClampedArray;
  width: number;
  height: number;
}

/**
 * Everything a source can be. Encoded bytes (File, Blob, URL, ArrayBuffer, Uint8Array) are
 * decoded by the engine exactly like the CLI; elements, bitmaps and ImageData use the browser's
 * decoded pixels.
 */
export type ImageInput =
  | Blob
  | URL
  | string
  | ArrayBuffer
  | Uint8Array
  | ImageBitmap
  | ImageData
  | OffscreenCanvas
  | HTMLImageElement
  | HTMLCanvasElement
  | RawImage
  | GeneratorSpec;

/** Where render() can draw instead of returning data. */
export type CanvasTarget = HTMLCanvasElement | OffscreenCanvas;

/** Options for one render: every recipe knob plus the output. */
export interface RenderOptions extends DitherOptions {
  /** Output format (default "png"). */
  format?: RenderFormat;
  /** Return a Blob instead of bytes or text. */
  output?: "blob";
  /** Draw into this canvas (resized to the output) instead of returning data. */
  target?: CanvasTarget;
  /**
   * Preview mode: when the output's longest side exceeds this many pixels, downscale before
   * dithering and render an approximation at this size. Omit for the exact output.
   */
  preview?: number;
  /** Cancel the render; it rejects with DithError code "ABORTED". */
  signal?: AbortSignal;
}

/** Per-color usage in a report. */
export interface ColorUsage {
  hex: HexColor;
  pixels: number;
  percent: number;
}

/** The engine's report: the same fields as "dith apply --json", plus preview. */
export interface RenderReport {
  status: "ok";
  dry_run: false;
  source: string;
  output: null;
  format: string;
  preset: string | null;
  algorithm: AlgorithmId;
  color_space: ColorSpace;
  palette_name: string;
  palette_colors: HexColor[];
  source_dimensions: [number, number];
  working_dimensions: [number, number];
  output_dimensions: [number, number];
  bytes_written: number;
  elapsed_ms: number;
  color_usage: ColorUsage[];
  preview: boolean;
  recipe: Recipe;
}

interface ResultBase {
  width: number;
  height: number;
  /** True for a reduced preview (see RenderOptions.preview). */
  preview: boolean;
  report: RenderReport;
}

/** An encoded image. */
export interface BytesResult<F extends EncodedFormat = EncodedFormat> extends ResultBase {
  format: F;
  mime: MimeTypes[F];
  bytes: Uint8Array;
}

/** SVG markup or Unicode text art. */
export interface TextResult<F extends TextFormat = TextFormat> extends ResultBase {
  format: F;
  mime: MimeTypes[F];
  text: string;
}

/** A file as a Blob (with output: "blob"). */
export interface BlobResult<F extends EncodedFormat | TextFormat = EncodedFormat | TextFormat> extends ResultBase {
  format: F;
  mime: MimeTypes[F];
  blob: Blob;
}

/** A GPU-ready bitmap; transfer or draw it, then close() it. */
export interface BitmapResult extends ResultBase {
  format: "bitmap";
  bitmap: ImageBitmap;
}

/** Raw RGBA8 pixels (new ImageData(pixels, width, height) shows them). */
export interface PixelsResult extends ResultBase {
  format: "rgba";
  pixels: Uint8ClampedArray<ArrayBuffer>;
}

/** The result of drawing into a target canvas. */
export interface DrawResult extends ResultBase {
  format: "canvas";
  canvas: CanvasTarget;
}

/** The result type for a format. */
export type ResultOf<F extends RenderFormat> = F extends EncodedFormat
  ? BytesResult<F>
  : F extends TextFormat
    ? TextResult<F>
    : F extends "bitmap"
      ? BitmapResult
      : PixelsResult;

type FormatOf<O> = O extends { format: infer F extends RenderFormat } ? F : "png";

/** The result type of render() for an options object. */
export type RenderResult<O extends RenderOptions = RenderOptions> = O extends { target: CanvasTarget }
  ? DrawResult
  : O extends { output: "blob" }
    ? BlobResult<Extract<FormatOf<O>, EncodedFormat | TextFormat>>
    : ResultOf<FormatOf<O>>;

/** The undithered source at the size render() would use. */
export interface OriginalResult {
  width: number;
  height: number;
  preview: boolean;
  bitmap?: ImageBitmap;
  pixels?: Uint8ClampedArray<ArrayBuffer>;
}

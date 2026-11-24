import {
  dispatch,
  transferablesOf,
  type EngineOutput,
  type EngineRequest,
  type EngineResponse,
  type RenderJob,
  type SourceInfo,
} from "./engine.js";
import { abortError, DithError, fromPayload, toPayload } from "./errors.js";
import { prepareInput, type InputOptions, type PreparedInput } from "./input.js";
import { resolveRecipe } from "./recipe.js";
import type {
  CanvasTarget,
  DrawResult,
  ImageInput,
  OriginalResult,
  RenderFormat,
  RenderOptions,
  RenderReport,
  RenderResult,
} from "./types.js";
import { compileModule, instantiate, type WasmInput } from "./wasm.js";

export interface DithConfig {
  /** The WebAssembly module: a URL (default: next to this file), Response, bytes, or Module. */
  wasm?: WasmInput | Promise<WasmInput>;
  /**
   * Worker pool size. "auto" (default) is hardwareConcurrency - 1, capped at 4; false or 0 runs
   * everything on the calling thread. Ignored where Worker is unavailable (Node).
   */
  workers?: number | "auto" | false;
  /** Worker factory, for bundlers that need it, e.g. Vite's "dith/worker?worker" import. */
  worker?: () => Worker;
  /** Sources up to this many pixels (when known up front) render on the calling thread. Default 65536. */
  mainThreadMaxPixels?: number;
}

interface Job {
  request: EngineRequest;
  transfer: Transferable[];
  resolve: (value: unknown) => void;
  reject: (error: unknown) => void;
  signal?: AbortSignal;
  onAbort?: () => void;
}

interface Lane {
  readonly main: boolean;
  /** Jobs waiting or running. */
  busy(): number;
  /** Sources pinned to this lane. */
  sources: number;
  call(request: EngineRequest, transfer: Transferable[], signal?: AbortSignal): Promise<unknown>;
  terminate(): void;
}

function discard(value: unknown): void {
  (value as Partial<EngineOutput> | undefined)?.bitmap?.close();
}

class MainLane implements Lane {
  readonly main = true;
  sources = 0;
  #running = 0;
  constructor(private readonly module: Promise<WebAssembly.Module>) {}
  busy(): number {
    return this.#running;
  }
  async call(request: EngineRequest, _transfer: Transferable[], signal?: AbortSignal): Promise<unknown> {
    if (signal?.aborted) throw abortError();
    instantiate(await this.module);
    if (signal?.aborted) throw abortError();
    this.#running++;
    try {
      return await dispatch(request);
    } catch (error) {
      throw error instanceof DithError ? error : fromPayload(toPayload(error));
    } finally {
      this.#running--;
    }
  }
  terminate(): void {}
}

class WorkerLane implements Lane {
  readonly main = false;
  sources = 0;
  #worker: Worker;
  #queue: Job[] = [];
  #current: (Job & { id: number }) | undefined;
  #nextId = 1;
  /** Resolves once the worker has instantiated the module. */
  readonly ready: Promise<void>;
  #readyResolve: () => void = () => undefined;

  constructor(factory: () => Worker, module: Promise<WebAssembly.Module>) {
    this.#worker = factory();
    this.#worker.onmessage = (event: MessageEvent<EngineResponse>) => this.#settle(event.data);
    this.#worker.onerror = (event) =>
      this.#fail(
        new DithError(
          "WORKER_ERROR",
          event.message || "worker failed",
          "Check that the worker script and wasm URL are reachable.",
        ),
      );
    // Hold the queue until the worker has instantiated the shared module (job id 0).
    const fail = (error: unknown): void => this.#fail(error);
    this.ready = new Promise((resolve) => (this.#readyResolve = resolve));
    this.#current = {
      id: 0,
      request: { op: "close", source: 0 },
      transfer: [],
      resolve: () => this.#readyResolve(),
      reject: fail,
    };
    module.then((m) => this.#worker.postMessage({ id: 0, op: "init", module: m }), fail);
  }

  busy(): number {
    return this.#queue.length + (this.#current ? 1 : 0);
  }

  call(request: EngineRequest, transfer: Transferable[], signal?: AbortSignal): Promise<unknown> {
    if (signal?.aborted) return Promise.reject(abortError());
    return new Promise((resolve, reject) => {
      const job: Job = { request, transfer, resolve, reject, signal };
      if (signal) {
        job.onAbort = () => {
          const index = this.#queue.indexOf(job);
          if (index >= 0) this.#queue.splice(index, 1);
          reject(abortError());
        };
        signal.addEventListener("abort", job.onAbort, { once: true });
      }
      this.#queue.push(job);
      this.#pump();
    });
  }

  #pump(): void {
    if (this.#current) return;
    const job = this.#queue.shift();
    if (!job) return;
    const id = this.#nextId++;
    this.#current = { ...job, id };
    this.#worker.postMessage({ id, ...job.request }, job.transfer);
  }

  #settle(response: EngineResponse): void {
    const job = this.#current;
    if (!job || job.id !== response.id) return;
    this.#current = undefined;
    if (job.signal && job.onAbort) job.signal.removeEventListener("abort", job.onAbort);
    if (job.signal?.aborted) {
      if (response.ok) discard(response.value);
    } else if (response.ok) job.resolve(response.value);
    else job.reject(fromPayload(response.error));
    this.#pump();
  }

  #fail(error: unknown): void {
    const jobs = [...(this.#current ? [this.#current] : []), ...this.#queue];
    this.#current = undefined;
    this.#queue = [];
    for (const job of jobs) job.reject(error);
  }

  terminate(): void {
    this.#fail(new DithError("WORKER_ERROR", "the dith instance was terminated", "Create a new instance."));
    this.#worker.terminate();
  }
}

function canvasDraw(target: CanvasTarget, bitmap: ImageBitmap): void {
  target.width = bitmap.width;
  target.height = bitmap.height;
  const ctx = target.getContext("2d") as CanvasRenderingContext2D | OffscreenCanvasRenderingContext2D | null;
  if (ctx) {
    ctx.drawImage(bitmap, 0, 0);
    bitmap.close();
    return;
  }
  const renderer = target.getContext("bitmaprenderer") as ImageBitmapRenderingContext | null;
  if (!renderer)
    throw new DithError("UNSUPPORTED", "the target canvas has no 2d or bitmaprenderer context", "Pass a fresh canvas.");
  renderer.transferFromImageBitmap(bitmap);
}

const decoder = new TextDecoder();

function finish(out: EngineOutput, options: RenderOptions): unknown {
  const report = JSON.parse(out.report) as RenderReport;
  const base = { width: out.width, height: out.height, preview: out.preview, report };
  if (options.target) {
    if (!out.bitmap)
      throw new DithError("UNSUPPORTED", "drawing needs ImageBitmap support", "Use an encoded format in this runtime.");
    canvasDraw(options.target, out.bitmap);
    return { ...base, format: "canvas", canvas: options.target } satisfies DrawResult;
  }
  if (out.format === "bitmap") return { ...base, format: "bitmap", bitmap: out.bitmap };
  const bytes = out.bytes ?? new Uint8Array();
  if (out.format === "rgba")
    return { ...base, format: "rgba", pixels: new Uint8ClampedArray(bytes.buffer, bytes.byteOffset, bytes.byteLength) };
  if (options.output === "blob")
    return { ...base, format: out.format, mime: out.mime, blob: new Blob([bytes], { type: out.mime }) };
  if (out.format === "svg" || out.format === "braille" || out.format === "ascii")
    return { ...base, format: out.format, mime: out.mime, text: decoder.decode(bytes) };
  return { ...base, format: out.format, mime: out.mime, bytes };
}

function job(options: RenderOptions, procedural: boolean): RenderJob {
  const format: RenderFormat = options.target ? "bitmap" : (options.format ?? "png");
  return { recipe: JSON.stringify(resolveRecipe(options, { procedural })), format, preview: options.preview };
}

function defaultWorkers(): number {
  if (typeof Worker === "undefined") return 0;
  const cores = typeof navigator !== "undefined" && navigator.hardwareConcurrency ? navigator.hardwareConcurrency : 2;
  return Math.min(4, Math.max(1, cores - 1));
}

/**
 * A dith engine with an optional worker pool. Nothing loads until the first render; the module
 * is compiled once (streaming) and shared with every worker.
 */
export class Dith {
  readonly #config: DithConfig;
  #module: Promise<WebAssembly.Module> | undefined;
  #main: MainLane | undefined;
  #workers: WorkerLane[] = [];
  readonly #maxWorkers: number;

  constructor(config: DithConfig = {}) {
    this.#config = config;
    const requested = config.workers ?? "auto";
    this.#maxWorkers =
      typeof Worker === "undefined" || requested === false
        ? 0
        : requested === "auto"
          ? defaultWorkers()
          : Math.max(0, requested);
  }

  #compiled(): Promise<WebAssembly.Module> {
    this.#module ??= compileModule(this.#config.wasm);
    return this.#module;
  }

  #mainLane(): MainLane {
    this.#main ??= new MainLane(this.#compiled());
    return this.#main;
  }

  #spawn(): WorkerLane {
    const factory =
      this.#config.worker ??
      (() => new Worker(new URL("./worker.js", import.meta.url), { type: "module", name: "dith" }));
    const lane = new WorkerLane(factory, this.#compiled());
    this.#workers.push(lane);
    return lane;
  }

  #lane(prepared: PreparedInput): Lane {
    const small = prepared.pixels !== undefined && prepared.pixels <= (this.#config.mainThreadMaxPixels ?? 65_536);
    if (this.#maxWorkers === 0 || small) return this.#mainLane();
    // An idle worker (fewest pinned sources first), else a new one, else the least busy.
    const idle = this.#workers.filter((lane) => lane.busy() === 0).sort((x, y) => x.sources - y.sources)[0];
    if (idle) return idle;
    if (this.#workers.length < this.#maxWorkers) return this.#spawn();
    return this.#workers.reduce((best, lane) => (lane.busy() < best.busy() ? lane : best));
  }

  /** Compile the module and start one worker (or instantiate here) now, so the first render is faster. */
  async ready(): Promise<void> {
    const module = await this.#compiled();
    if (this.#maxWorkers === 0) instantiate(module);
    else await (this.#workers[0] ?? this.#spawn()).ready;
  }

  /** Decode once and keep the pixels in a worker for many renders (live previews). */
  async source(input: ImageInput, options: InputOptions & { signal?: AbortSignal } = {}): Promise<DithSource> {
    const prepared = await prepareInput(input, options);
    const lane = this.#lane(prepared);
    lane.sources++;
    try {
      const info = (await lane.call(
        { op: "open", payload: prepared.payload },
        prepared.transfer,
        options.signal,
      )) as SourceInfo;
      return new DithSource(lane, info, prepared.name);
    } catch (error) {
      lane.sources--;
      throw error;
    }
  }

  /** Render one input with one set of options. */
  async render<const O extends RenderOptions = Record<never, never>>(
    input: ImageInput,
    options?: O & InputOptions,
  ): Promise<RenderResult<O>> {
    const opts: RenderOptions & InputOptions = options ?? {};
    const prepared = await prepareInput(input, opts);
    const lane = this.#lane(prepared);
    const out = (await lane.call(
      { op: "oneshot", payload: prepared.payload, job: job(opts, prepared.procedural) },
      prepared.transfer,
      opts.signal,
    )) as EngineOutput;
    return finish(out, opts) as RenderResult<O>;
  }

  /** Stop every worker; pending renders reject with WORKER_ERROR. */
  terminate(): void {
    for (const lane of this.#workers) lane.terminate();
    this.#workers = [];
  }
}

/** A decoded source pinned to one worker (or the calling thread). */
export class DithSource {
  readonly #lane: Lane;
  readonly #id: number;
  #disposed = false;
  /** Source width in pixels (canvas width for generators). */
  readonly width: number;
  /** Source height in pixels. */
  readonly height: number;
  /** True for generator sources ("dith generate"), false for images ("dith apply"). */
  readonly procedural: boolean;
  /** The name used in reports and cliCommand. */
  readonly name: string;

  constructor(lane: Lane, info: SourceInfo, name: string) {
    this.#lane = lane;
    this.#id = info.id;
    this.width = info.width;
    this.height = info.height;
    this.procedural = info.procedural;
    this.name = name;
  }

  /** Whether this source renders on the calling thread rather than in a worker. */
  get onMainThread(): boolean {
    return this.#lane.main;
  }

  /** Render with these options. */
  async render<const O extends RenderOptions = Record<never, never>>(options?: O): Promise<RenderResult<O>> {
    const opts: RenderOptions = options ?? {};
    const out = (await this.#lane.call(
      { op: "render", source: this.#id, job: job(opts, this.procedural) },
      [],
      opts.signal,
    )) as EngineOutput;
    return finish(out, opts) as RenderResult<O>;
  }

  /** The undithered source at the size render() would use, as a bitmap (or raw pixels). */
  async original(
    options: Omit<RenderOptions, "format" | "output"> & { format?: "bitmap" | "rgba" } = {},
  ): Promise<OriginalResult> {
    const format = options.target ? "bitmap" : (options.format ?? "bitmap");
    const request: EngineRequest = {
      op: "original",
      source: this.#id,
      job: { ...job(options, this.procedural), format },
    };
    const out = (await this.#lane.call(request, [], options.signal)) as EngineOutput;
    if (options.target && out.bitmap) canvasDraw(options.target, out.bitmap);
    const pixels = out.bytes
      ? new Uint8ClampedArray(out.bytes.buffer, out.bytes.byteOffset, out.bytes.byteLength)
      : undefined;
    return {
      width: out.width,
      height: out.height,
      preview: out.preview,
      bitmap: options.target ? undefined : out.bitmap,
      pixels,
    };
  }

  /** Free the decoded pixels. */
  dispose(): void {
    if (this.#disposed) return;
    this.#disposed = true;
    this.#lane.sources--;
    void this.#lane.call({ op: "close", source: this.#id }, []).catch(() => undefined);
  }
}

/**
 * Latest-wins runner for live UIs: each call aborts the previous one. A superseded call
 * resolves to undefined instead of rejecting.
 *
 *     const live = latest<BitmapResult>();
 *     slider.oninput = async () => {
 *       const out = await live((signal) => source.render({ format: "bitmap", preview: 800, signal }));
 *       if (out) draw(out.bitmap);
 *     };
 */
export function latest<T>(): (task: (signal: AbortSignal) => Promise<T>) => Promise<T | undefined> {
  let controller: AbortController | undefined;
  return async (task) => {
    controller?.abort();
    const current = new AbortController();
    controller = current;
    try {
      const value = await task(current.signal);
      if (!current.signal.aborted) return value;
      (value as { bitmap?: ImageBitmap } | undefined)?.bitmap?.close();
      return undefined;
    } catch (error) {
      if (current.signal.aborted) return undefined;
      throw error;
    }
  };
}

export { transferablesOf };

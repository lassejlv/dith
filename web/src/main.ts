import {
  ALGORITHMS,
  DEFAULT_RECIPE,
  GENERATORS,
  PALETTES,
  PRESETS,
  cliCommand,
  createDith,
  hexToRgb,
  isDithError,
  latest,
  presetRecipe,
  rgbToHex,
  stringifyRecipe,
  type BitmapResult,
  type DithSource,
  type GeneratorId,
  type GeneratorSpec,
  type PaletteId,
  type PresetId,
  type Recipe,
  type Rgb,
} from "dith";
import wasmUrl from "dith/wasm/dith_bg.wasm?url";
import DithWorker from "dith/worker?worker";

import "./style.css";

const t0 = performance.now();
const perf = { workerReadyMs: 0, firstRenderMs: 0, renders: [] as { ms: number; preview: boolean; width: number; height: number }[] };
(window as unknown as { __dith: typeof perf }).__dith = perf;

const dith = createDith({ wasm: wasmUrl, worker: () => new DithWorker({ name: "dith" }) });

const $ = <T extends HTMLElement>(id: string): T => document.getElementById(id) as T;
const stage = $<HTMLElement>("stage");
const resultCanvas = $<HTMLCanvasElement>("result");
const originalCanvas = $<HTMLCanvasElement>("original");
const meta = $<HTMLElement>("meta");
const commandText = $<HTMLElement>("command");
const generatorSelect = $<HTMLSelectElement>("generator");
const fileInput = $<HTMLInputElement>("file");

const GENERATOR_CANVAS = { width: 1200, height: 800 };
const SIZES = [
  ["original", "Original size"],
  ["2400", "2400 px"],
  ["1600", "1600 px"],
  ["1200", "1200 px"],
  ["800", "800 px"],
  ["480", "480 px"],
] as const;

interface SliderSpec {
  path: string;
  label: string;
  min: number;
  max: number;
  step: number;
}

const SLIDERS: SliderSpec[] = [
  { path: "dither.strength", label: "Strength", min: 0, max: 1.5, step: 0.05 },
  { path: "preprocess.contrast", label: "Contrast", min: -1, max: 1, step: 0.05 },
  { path: "preprocess.brightness", label: "Brightness", min: -1, max: 1, step: 0.05 },
  { path: "preprocess.gamma", label: "Gamma", min: 0.2, max: 3, step: 0.05 },
  { path: "preprocess.sharpen", label: "Sharpen", min: 0, max: 3, step: 0.05 },
  { path: "preprocess.blur", label: "Blur", min: 0, max: 5, step: 0.1 },
  { path: "dither.noise", label: "Noise", min: 0, max: 0.5, step: 0.01 },
  { path: "dither.bias", label: "Bias", min: -0.5, max: 0.5, step: 0.01 },
  { path: "preprocess.pixel_size", label: "Pixel size", min: 1, max: 16, step: 1 },
  { path: "dither.pattern_scale", label: "Pattern scale", min: 1, max: 8, step: 1 },
];

const TOGGLES = [
  { path: "preprocess.grayscale", label: "Grayscale" },
  { path: "preprocess.auto_levels", label: "Auto levels" },
  { path: "preprocess.invert", label: "Invert" },
  { path: "dither.serpentine", label: "Serpentine" },
];

type SourceRef = { kind: "image"; name: string } | { kind: "generator"; spec: GeneratorSpec };

const state = {
  recipe: presetRecipe("editorial-print", { procedural: true }),
  source: { kind: "generator", spec: { pattern: "sphere", ...GENERATOR_CANVAS } } as SourceRef,
  size: "1200",
  view: "result" as "result" | "original",
  customColors: "#111111, #f5efe6",
  autoColors: 8,
  stopsText: "",
};

let current: Promise<DithSource> | undefined;

function getPath(obj: unknown, path: string): unknown {
  return path.split(".").reduce<unknown>((o, k) => (o as Record<string, unknown>)[k], obj);
}

function setPath(obj: unknown, path: string, value: unknown): void {
  const keys = path.split(".");
  const last = keys.pop() as string;
  const target = keys.reduce<unknown>((o, k) => (o as Record<string, unknown>)[k], obj) as Record<string, unknown>;
  target[last] = value;
}

function format(value: number, step: number): string {
  return value.toFixed(step >= 1 ? 0 : step >= 0.1 ? 1 : 2);
}

function parseColors(text: string): Rgb[] {
  const seen = new Set<string>();
  const out: Rgb[] = [];
  for (const match of text.matchAll(/#?([0-9a-f]{6}|[0-9a-f]{3})\b/gi)) {
    const color = hexToRgb(match[0]);
    const key = rgbToHex(color);
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(color);
  }
  return out;
}

function procedural(): boolean {
  return state.source.kind === "generator";
}

/** The recipe with playground-level settings (output size) applied. */
function effectiveRecipe(): Recipe {
  const recipe = structuredClone(state.recipe);
  if (!procedural() && state.size !== "original") recipe.preprocess.max_dim = Number(state.size);
  return recipe;
}

function openSource(): void {
  const previous = current;
  const ref = state.source;
  current = ref.kind === "generator" ? dith.source(ref.spec) : current;
  void previous?.then((s) => {
    if (previous !== current) s.dispose();
  });
}

// ---------------------------------------------------------------- rendering

const live = latest<BitmapResult>();
const liveOriginal = latest<{ bitmap?: ImageBitmap; width: number; height: number; preview: boolean }>();
let previewFrame = 0;
let fullTimer = 0;
let lastFull: string | undefined;

function displayCap(): number {
  const rect = stage.getBoundingClientRect();
  return Math.max(64, Math.round(Math.max(rect.width, rect.height) * Math.min(window.devicePixelRatio || 1, 2)));
}

async function draw(preview: number | undefined): Promise<void> {
  const pending = current;
  if (!pending) return;
  const recipe = effectiveRecipe();
  const started = performance.now();
  try {
    const src = await pending;
    if (state.view === "original") {
      const out = await liveOriginal((signal) => src.original({ recipe, preview, signal }));
      if (out?.bitmap) show(originalCanvas, out.bitmap, out.width, out.height, out.preview);
      return;
    }
    const out = await live((signal) => src.render({ recipe, format: "bitmap", preview, signal }));
    if (!out) return;
    const ms = performance.now() - started;
    show(resultCanvas, out.bitmap, out.width, out.height, out.preview);
    shown = { width: out.width, height: out.height, preview: out.preview };
    perf.renders.push({ ms: Math.round(ms), preview: out.preview, width: out.width, height: out.height });
    if (!perf.firstRenderMs) perf.firstRenderMs = Math.round(performance.now() - t0);
    if (!out.preview) delete meta.dataset.busy;
    const [w, h] = out.report.output_dimensions;
    meta.textContent = w + " × " + h + " · " + Math.round(ms) + " ms";
    const command =
      src.procedural && state.source.kind === "generator"
        ? cliCommand({ recipe, generator: state.source.spec, output: state.source.spec.pattern + "-dith.png" })
        : cliCommand({ recipe, input: src.name, output: src.name.replace(/\.[^.]+$/, "") + "-dith.png" });
    commandText.textContent = command.command;
  } catch (error) {
    delete meta.dataset.busy;
    meta.textContent = isDithError(error) ? error.message : String(error);
  }
}

function requestRender(): void {
  cancelAnimationFrame(previewFrame);
  previewFrame = requestAnimationFrame(() => {
    // The first frame is a small preview so something appears while the engine warms up.
    void draw(perf.renders.length === 0 ? Math.min(displayCap(), 480) : displayCap());
  });
  clearTimeout(fullTimer);
  fullTimer = window.setTimeout(requestFull, 180);
}

function requestFull(): void {
  if (state.view === "original") return;
  const key = JSON.stringify([effectiveRecipe(), state.source]);
  if (key === lastFull) return;
  lastFull = key;
  meta.dataset.busy = "true";
  void draw(undefined);
}

function fit(canvas: HTMLCanvasElement, width: number, height: number, preview: boolean): void {
  const rect = stage.getBoundingClientRect();
  const dpr = window.devicePixelRatio || 1;
  let scale = Math.min((rect.width - 32) / width, (rect.height - 72) / height);
  if (!preview && scale >= 1) scale = Math.max(1, Math.floor(scale * dpr) / dpr);
  canvas.style.width = Math.round(width * scale) + "px";
  canvas.style.height = Math.round(height * scale) + "px";
  // Crisp pixels when enlarging; smooth averaging when shrinking, so dots don't alias.
  canvas.style.imageRendering = scale * dpr >= 1 ? "pixelated" : "auto";
}

let shown = { width: 0, height: 0, preview: false };

function show(canvas: HTMLCanvasElement, bitmap: ImageBitmap, width: number, height: number, preview: boolean): void {
  canvas.getContext("bitmaprenderer")?.transferFromImageBitmap(bitmap);
  fit(canvas, width, height, preview);
  canvas.dataset.ready = "";
}

// ---------------------------------------------------------------- controls

function selectEl(id: string, label: string, options: readonly (readonly [string, string])[]): HTMLSelectElement {
  const wrap = document.createElement("label");
  wrap.className = "field";
  const caption = document.createElement("span");
  caption.className = "label";
  caption.textContent = label;
  const select = document.createElement("select");
  select.id = id;
  for (const [value, text] of options) select.add(new Option(text, value));
  const box = document.createElement("span");
  box.className = "select-wrap";
  box.append(select);
  wrap.append(caption, box);
  return select;
}

function slider(spec: SliderSpec, onInput: (v: number) => void): { root: HTMLElement; input: HTMLInputElement; value: HTMLElement } {
  const root = document.createElement("label");
  root.className = "slider";
  const head = document.createElement("span");
  head.className = "slider-head";
  const name = document.createElement("span");
  name.className = "label";
  name.textContent = spec.label;
  const value = document.createElement("span");
  value.className = "value";
  head.append(name, value);
  const input = document.createElement("input");
  input.type = "range";
  input.min = String(spec.min);
  input.max = String(spec.max);
  input.step = String(spec.step);
  input.addEventListener("input", () => {
    value.textContent = format(Number(input.value), spec.step);
    onInput(Number(input.value));
  });
  root.append(head, input);
  return { root, input, value };
}

const sliders = new Map<string, { input: HTMLInputElement; value: HTMLElement; spec: SliderSpec }>();
const toggles = new Map<string, HTMLInputElement>();
const presetSelect = selectEl("preset", "Preset", [["", "None"], ...PRESETS.map((p) => [p.id, p.name] as const)]);
const algorithmSelect = selectEl(
  "algorithm",
  "Algorithm",
  ALGORITHMS.map((a) => [a.id, a.name] as const),
);
const paletteSelect = selectEl("palette", "Palette", [
  ...PALETTES.map((p) => [p.id, p.name] as const),
  ["custom", "Custom colours"],
  ["auto", "From image"],
]);
const sizeSelect = selectEl("size", "Size", SIZES);
sizeSelect.value = state.size;

function buildControls(): void {
  generatorSelect.replaceChildren(...GENERATORS.map((g) => new Option(g.name, g.id)));
  generatorSelect.value = "sphere";
  $<HTMLElement>("selects").replaceChildren(
    ...[presetSelect, algorithmSelect, paletteSelect, sizeSelect].map((s) => s.closest("label") as HTMLElement),
  );

  presetSelect.addEventListener("change", () => selectPreset((presetSelect.value || null) as PresetId | null));
  algorithmSelect.addEventListener("change", () => {
    state.recipe.dither.algorithm = algorithmSelect.value as Recipe["dither"]["algorithm"];
    requestRender();
  });
  paletteSelect.addEventListener("change", () => {
    applyPaletteChoice();
    renderPaletteExtra();
    requestRender();
  });
  sizeSelect.addEventListener("change", () => {
    state.size = sizeSelect.value;
    requestRender();
  });

  const sliderHost = $<HTMLElement>("sliders");
  for (const spec of SLIDERS) {
    const s = slider(spec, (v) => {
      setPath(state.recipe, spec.path, v);
      requestRender();
    });
    sliders.set(spec.path, { input: s.input, value: s.value, spec });
    sliderHost.append(s.root);
  }

  const toggleHost = $<HTMLElement>("toggles");
  for (const t of TOGGLES) {
    const label = document.createElement("label");
    label.className = "toggle";
    const input = document.createElement("input");
    input.type = "checkbox";
    input.addEventListener("change", () => {
      setPath(state.recipe, t.path, input.checked);
      requestRender();
    });
    const text = document.createElement("span");
    text.textContent = t.label;
    label.append(input, text);
    toggles.set(t.path, input);
    toggleHost.append(label);
  }
  renderGeneratorControls();
}

function applyPaletteChoice(): void {
  const choice = paletteSelect.value;
  if (choice === "custom") {
    const colors = parseColors(state.customColors);
    if (colors.length >= 2) state.recipe.palette = { mode: "colors", value: colors };
  } else if (choice === "auto") {
    state.recipe.palette = { mode: "adaptive", value: state.autoColors };
  } else {
    state.recipe.palette = { mode: "builtin", value: choice as PaletteId };
  }
}

function renderPaletteExtra(): void {
  const host = $<HTMLElement>("palette-extra");
  host.replaceChildren();
  const choice = paletteSelect.value;
  if (choice === "custom") {
    const label = document.createElement("label");
    label.className = "field";
    label.innerHTML = '<span class="label">Colours</span>';
    const input = document.createElement("input");
    input.type = "text";
    input.className = "text";
    input.id = "colors";
    input.spellcheck = false;
    input.value = state.customColors;
    input.addEventListener("input", () => {
      state.customColors = input.value;
      applyPaletteChoice();
      requestRender();
    });
    label.append(input);
    host.append(label);
  } else if (choice === "auto") {
    const s = slider({ path: "", label: "Colours", min: 2, max: 24, step: 1 }, (v) => {
      state.autoColors = v;
      applyPaletteChoice();
      requestRender();
    });
    s.input.value = String(state.autoColors);
    s.value.textContent = String(state.autoColors);
    host.append(s.root);
  }
  const row = document.createElement("div");
  row.className = "swatches";
  for (const hex of paletteSwatches()) {
    const sw = document.createElement("span");
    sw.style.background = hex;
    sw.title = hex;
    row.append(sw);
  }
  host.append(row);
}

function paletteSwatches(): readonly string[] {
  const p = state.recipe.palette;
  if (p.mode === "builtin") return PALETTES.find((x) => x.id === p.value)?.colors ?? [];
  if (p.mode === "colors") return p.value.map(rgbToHex);
  return [];
}

function renderGeneratorControls(): void {
  const host = $<HTMLElement>("generator-controls");
  host.replaceChildren();
  sizeSelect.closest("label")?.toggleAttribute("hidden", procedural());
  if (state.source.kind !== "generator") return;
  const spec = state.source.spec;
  const angle = slider({ path: "", label: "Angle", min: 0, max: 360, step: 5 }, (v) => updateGenerator({ angle: v }));
  angle.input.value = String(spec.angle ?? 135);
  angle.value.textContent = String(spec.angle ?? 135);
  const freq = slider({ path: "", label: "Frequency", min: 0.25, max: 4, step: 0.05 }, (v) => updateGenerator({ frequency: v }));
  freq.input.value = String(spec.frequency ?? 1);
  freq.value.textContent = format(spec.frequency ?? 1, 0.05);
  const stops = document.createElement("label");
  stops.className = "field";
  stops.innerHTML = '<span class="label">Colours</span>';
  const input = document.createElement("input");
  input.type = "text";
  input.className = "text";
  input.placeholder = "#0a0a0d, #f7f7f9";
  input.spellcheck = false;
  input.value = state.stopsText;
  input.addEventListener("change", () => {
    state.stopsText = input.value;
    const colors = parseColors(input.value);
    updateGenerator({ stops: colors.length >= 2 ? colors.map(rgbToHex) : undefined });
  });
  stops.append(input);
  host.append(angle.root, freq.root, stops);
}

function updateGenerator(patch: Partial<GeneratorSpec>): void {
  if (state.source.kind !== "generator") return;
  const spec: GeneratorSpec = { ...state.source.spec, ...patch };
  if (spec.angle === 135) delete spec.angle;
  if (spec.frequency === 1) delete spec.frequency;
  if (!spec.stops) delete spec.stops;
  state.source = { kind: "generator", spec };
  openSource();
  requestRender();
}

function syncControls(): void {
  const r = state.recipe;
  presetSelect.value = r.preset ?? "";
  algorithmSelect.value = r.dither.algorithm;
  paletteSelect.value = r.palette.mode === "builtin" ? r.palette.value : r.palette.mode === "adaptive" ? "auto" : "custom";
  if (r.palette.mode === "adaptive") state.autoColors = r.palette.value;
  for (const { input, value, spec } of sliders.values()) {
    const v = Number(getPath(r, spec.path));
    input.value = String(v);
    value.textContent = format(v, spec.step);
  }
  for (const [path, input] of toggles) input.checked = Boolean(getPath(r, path));
  renderPaletteExtra();
}

function selectPreset(name: PresetId | null): void {
  state.recipe = name ? presetRecipe(name, { procedural: procedural() }) : structuredClone(DEFAULT_RECIPE);
  syncControls();
  requestRender();
}

buildControls();
syncControls();
document.querySelector(".panel")?.removeAttribute("data-pending");
openSource();
requestRender();
void current?.then(() => {
  perf.workerReadyMs = Math.round(performance.now() - t0);
});

// ---------------------------------------------------------------- sources

async function openFile(file: File): Promise<void> {
  const previous = current;
  state.source = { kind: "image", name: file.name };
  current = dith.source(file);
  void previous?.then((s) => s.dispose());
  let option = generatorSelect.querySelector<HTMLOptionElement>('option[value="image"]');
  if (!option) {
    option = new Option("", "image");
    generatorSelect.prepend(option);
  }
  option.textContent = file.name;
  generatorSelect.value = "image";
  renderGeneratorControls();
  selectPreset(state.recipe.preset ?? "editorial-print");
}

$<HTMLButtonElement>("open").addEventListener("click", () => fileInput.click());
fileInput.addEventListener("change", () => {
  const file = fileInput.files?.[0];
  if (file) void openFile(file);
  fileInput.value = "";
});

generatorSelect.addEventListener("change", () => {
  if (generatorSelect.value === "image") return;
  generatorSelect.querySelector('option[value="image"]')?.remove();
  state.source = { kind: "generator", spec: { pattern: generatorSelect.value as GeneratorId, ...GENERATOR_CANVAS } };
  openSource();
  renderGeneratorControls();
  selectPreset(state.recipe.preset);
});

const drop = $<HTMLElement>("drop");
let dragDepth = 0;
window.addEventListener("dragenter", (e) => {
  if (!e.dataTransfer?.types.includes("Files")) return;
  dragDepth++;
  drop.hidden = false;
});
window.addEventListener("dragleave", () => {
  dragDepth = Math.max(0, dragDepth - 1);
  if (!dragDepth) drop.hidden = true;
});
window.addEventListener("dragover", (e) => e.preventDefault());
window.addEventListener("drop", (e) => {
  e.preventDefault();
  dragDepth = 0;
  drop.hidden = true;
  const file = e.dataTransfer?.files[0];
  if (file) void openFile(file);
});

// ---------------------------------------------------------------- view toggle, export, command

const showOriginal = $<HTMLButtonElement>("show-original");
const showResult = $<HTMLButtonElement>("show-result");

function setView(view: "result" | "original"): void {
  state.view = view;
  showOriginal.setAttribute("aria-pressed", String(view === "original"));
  showResult.setAttribute("aria-pressed", String(view === "result"));
  originalCanvas.hidden = view !== "original";
  resultCanvas.hidden = view !== "result";
  if (view === "original") requestRender();
}

showOriginal.addEventListener("click", () => setView("original"));
showResult.addEventListener("click", () => setView("result"));

function fileStem(): string {
  return state.source.kind === "image" ? state.source.name.replace(/\.[^.]+$/, "") + "-dith" : state.source.spec.pattern + "-dith";
}

function saveBlob(blob: Blob, name: string): void {
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), 10_000);
}

for (const button of document.querySelectorAll<HTMLButtonElement>("[data-download]")) {
  button.addEventListener("click", async () => {
    const pending = current;
    if (!pending) return;
    const fmt = button.dataset.download as "png" | "webp" | "svg";
    button.disabled = true;
    try {
      const out = await (await pending).render({ recipe: effectiveRecipe(), format: fmt, output: "blob" });
      saveBlob(out.blob, fileStem() + "." + fmt);
    } finally {
      button.disabled = false;
    }
  });
}

const copyButton = $<HTMLButtonElement>("copy");
copyButton.addEventListener("click", async () => {
  const text = commandText.textContent ?? "";
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    const range = document.createRange();
    range.selectNodeContents(commandText);
    getSelection()?.removeAllRanges();
    getSelection()?.addRange(range);
  }
  copyButton.textContent = "Copied";
  setTimeout(() => (copyButton.textContent = "Copy command"), 1200);
});

$<HTMLButtonElement>("recipe").addEventListener("click", () => {
  saveBlob(new Blob([stringifyRecipe(effectiveRecipe())], { type: "application/json" }), "recipe.json");
});

// ---------------------------------------------------------------- theme and resize

const themeButton = $<HTMLButtonElement>("theme");
const stored = localStorage.getItem("dith-theme");
if (stored === "light" || stored === "dark") document.documentElement.dataset.theme = stored;
themeButton.addEventListener("click", () => {
  const theme = document.documentElement.dataset.theme ?? (matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark");
  const next = theme === "light" ? "dark" : "light";
  document.documentElement.dataset.theme = next;
  localStorage.setItem("dith-theme", next);
});

new ResizeObserver(() => {
  if (shown.width) fit(resultCanvas, shown.width, shown.height, shown.preview);
}).observe(stage);

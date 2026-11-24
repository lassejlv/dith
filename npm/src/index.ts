/**
 * dith: deterministic dithering for browsers and Node, compiled from the dith Rust engine.
 * Renders match the dith CLI byte for byte for the same recipe.
 *
 * @packageDocumentation
 */
import { Dith, type DithConfig, type DithSource } from "./dith.js";
import type { InputOptions } from "./input.js";
import type { ImageInput, RenderOptions, RenderResult } from "./types.js";

export { Dith, DithSource, latest, type DithConfig } from "./dith.js";
export { cliCommand, type CliCommand, type CliCommandOptions } from "./cli.js";
export {
  DithError,
  isDithError,
  type AnyDithError,
  type ClientErrorCode,
  type DithErrorCode,
  type ValidValuesOf,
} from "./errors.js";
export { auto, hexToRgb, parsePalette, readPalette, rgbToHex, type PaletteInput } from "./palette.js";
export {
  definePreset,
  parseRecipe,
  presetRecipe,
  registerPreset,
  resolveRecipe,
  stringifyRecipe,
  type CustomPreset,
  type DitherOptions,
  type PresetInput,
  type PresetRegistry,
} from "./recipe.js";
export type { InputOptions } from "./input.js";
export type * from "./types.js";
export type { WasmInput } from "./wasm.js";
export {
  ALGORITHMS,
  ALPHA_MODES,
  COLOR_SPACES,
  DEFAULT_RECIPE,
  ENGINE_ERROR_CODES,
  ENGINE_VERSION,
  FIT_MODES,
  GENERATORS,
  OUTPUT_FORMATS,
  PALETTES,
  PARAMETER_RANGES,
  PRESET_RECIPES,
  PRESETS,
  SVG_SHAPES,
  type AlgorithmId,
  type AlphaMode,
  type ColorSpace,
  type EngineErrorCode,
  type FitMode,
  type GeneratorId,
  type HexColor,
  type OutputFormat,
  type PaletteId,
  type PaletteSpec,
  type PresetId,
  type Recipe,
  type RecipeDither,
  type RecipeExport,
  type RecipeOptions,
  type RecipePreprocess,
  type Rgb,
} from "./generated/schema.js";

let shared: Dith | undefined;
let sharedConfig: DithConfig = {};

/** Configure the shared instance behind render() and source() (call before the first render). */
export function configure(config: DithConfig): void {
  shared?.terminate();
  shared = undefined;
  sharedConfig = config;
}

function instance(): Dith {
  shared ??= new Dith(sharedConfig);
  return shared;
}

/** Create a separate engine with its own worker pool. */
export function createDith(config: DithConfig = {}): Dith {
  return new Dith(config);
}

/** Load the engine ahead of the first render (optional: everything loads lazily). */
export function preload(): Promise<void> {
  return instance().ready();
}

/** Render an input with the shared engine. The result type follows options.format. */
export function render<const O extends RenderOptions = Record<never, never>>(
  input: ImageInput,
  options?: O & InputOptions,
): Promise<RenderResult<O>> {
  return instance().render(input, options);
}

/** Decode an input once with the shared engine, for many renders. */
export function source(input: ImageInput, options?: InputOptions & { signal?: AbortSignal }): Promise<DithSource> {
  return instance().source(input, options);
}

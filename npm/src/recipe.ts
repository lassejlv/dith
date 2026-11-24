import { DithError } from "./errors.js";
import {
  ALPHA_MODES,
  COLOR_SPACES,
  DEFAULT_RECIPE,
  FIT_MODES,
  OPTION_FIELDS,
  OUTPUT_FORMATS,
  PRESET_RECIPES,
  SVG_SHAPES,
  type HexColor,
  type PresetId,
  type Recipe,
  type RecipeOptions,
} from "./generated/schema.js";
import { hexToRgb, toPaletteSpec, type PaletteInput } from "./palette.js";

/** A custom preset made with {@link definePreset}. */
export interface CustomPreset<N extends string = string> {
  readonly name: N;
  readonly extends: PresetId | CustomPreset | null;
  readonly description: string;
  readonly options: Readonly<DitherOptions>;
}

/**
 * Names of presets added with {@link registerPreset}. Augment it to make them type-check as
 * preset: "name":
 *
 *     declare module "dith" { interface PresetRegistry { "brand-hero": true } }
 */
// oxlint-disable-next-line typescript/no-empty-interface, typescript/no-empty-object-type
export interface PresetRegistry {}

/** A built-in preset id, a registered preset name, or a {@link CustomPreset}. */
// PresetRegistry is empty until augmented, so its keys are never by default.
// oxlint-disable-next-line typescript/no-redundant-type-constituents
export type PresetInput = PresetId | Extract<keyof PresetRegistry, string> | CustomPreset;

/** Every recipe knob, typed. Layering matches the CLI: recipe, then preset, then the knobs. */
export interface DitherOptions extends RecipeOptions {
  /** Start from a recipe (the --emit-recipe JSON, as an object or string), like --recipe. */
  recipe?: Recipe | string;
  /** Start from a preset; replaces recipe, like --preset. */
  preset?: PresetInput;
  /** Built-in palette id, color list, auto(n), or PaletteSpec. CLI: --palette, --colors, --auto-palette */
  palette?: PaletteInput;
}

const registry = new Map<string, CustomPreset>();

/** Define a preset that extends a built-in (or another custom) preset with extra options. */
export function definePreset<const N extends string>(definition: {
  name: N;
  extends?: PresetId | CustomPreset;
  description?: string;
  options: Omit<DitherOptions, "preset" | "recipe">;
}): CustomPreset<N> {
  return Object.freeze({
    name: definition.name,
    extends: definition.extends ?? null,
    description: definition.description ?? "",
    options: Object.freeze({ ...definition.options }),
  });
}

/** Define a preset and make it usable by name (preset: "name"). Returns it. */
export function registerPreset<const N extends string>(
  definition: Parameters<typeof definePreset<N>>[0],
): CustomPreset<N> {
  if (definition.name in PRESET_RECIPES) {
    throw new DithError(
      "INVALID_PRESET_NAME",
      "'" + definition.name + "' is a built-in preset",
      "Choose a name that is not a built-in preset id.",
    );
  }
  const preset = definePreset(definition);
  registry.set(preset.name, preset);
  return preset;
}

function clone(recipe: Recipe): Recipe {
  return structuredClone(recipe);
}

/**
 * The recipe a built-in preset resolves to. With procedural, photo preparation (auto levels,
 * contrast, sharpen) is dropped exactly as "dith generate --preset" does.
 */
export function presetRecipe(name: PresetId, options: { procedural?: boolean } = {}): Recipe {
  const base = PRESET_RECIPES[name];
  if (!base) {
    throw new DithError(
      "UNKNOWN_PRESET",
      "unknown preset '" + String(name) + "'",
      "Use one of the built-in preset ids.",
      Object.keys(PRESET_RECIPES) as PresetId[],
    );
  }
  const recipe = clone(base);
  if (options.procedural) {
    recipe.preprocess.auto_levels = false;
    recipe.preprocess.contrast = 0;
    recipe.preprocess.sharpen = 0;
  }
  return recipe;
}

function invalid(reason: string): DithError<"INVALID_RECIPE_JSON"> {
  return new DithError(
    "INVALID_RECIPE_JSON",
    "failed to parse JSON recipe: " + reason,
    "Run 'dith capabilities' to inspect valid parameter names and value ranges.",
  );
}

function oneOf(section: string, field: string, value: unknown, allowed: readonly string[]): void {
  if (!allowed.includes(value as string))
    throw invalid(section + "." + field + " must be one of " + allowed.join(", "));
}

/**
 * Read a recipe (the JSON "dith apply --emit-recipe" prints, as a string or object). Missing
 * fields take their defaults; enum values are checked here and ranges when rendering.
 */
export function parseRecipe(input: string | object): Recipe {
  let raw: unknown = input;
  if (typeof input === "string") {
    try {
      raw = JSON.parse(input);
    } catch (error) {
      throw invalid(error instanceof Error ? error.message : String(error));
    }
  }
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) throw invalid("expected a JSON object");
  const r = raw as Partial<Recipe>;
  const recipe: Recipe = {
    preset: r.preset ?? null,
    palette: r.palette ?? DEFAULT_RECIPE.palette,
    dither: { ...DEFAULT_RECIPE.dither, ...r.dither },
    preprocess: { ...DEFAULT_RECIPE.preprocess, ...r.preprocess },
    export: { ...DEFAULT_RECIPE.export, ...r.export },
  };
  const modes = ["builtin", "colors", "file", "adaptive"];
  if (!modes.includes(recipe.palette.mode)) throw invalid("palette.mode must be one of " + modes.join(", "));
  oneOf("dither", "color_space", recipe.dither.color_space, COLOR_SPACES);
  oneOf("preprocess", "fit", recipe.preprocess.fit, FIT_MODES);
  oneOf("preprocess", "alpha_mode", recipe.preprocess.alpha_mode, ALPHA_MODES);
  oneOf("export", "svg_shape", recipe.export.svg_shape, SVG_SHAPES);
  if (recipe.export.format !== null) oneOf("export", "format", recipe.export.format, OUTPUT_FORMATS);
  return clone(recipe);
}

/** Serialize a recipe the way the CLI's --recipe reads it (pretty-printed JSON). */
export function stringifyRecipe(recipe: Recipe): string {
  return JSON.stringify(recipe, null, 2) + "\n";
}

function lookupPreset(input: PresetInput): CustomPreset | PresetId {
  if (typeof input !== "string") return input;
  if (input in PRESET_RECIPES) return input as PresetId;
  const custom = registry.get(input);
  if (!custom) {
    const valid = [...Object.keys(PRESET_RECIPES), ...registry.keys()];
    throw new DithError(
      "UNKNOWN_PRESET",
      "unknown preset '" + input + "'",
      "Use a built-in preset id or register one with registerPreset().",
      valid as PresetId[],
    );
  }
  return custom;
}

/**
 * Resolve options into a complete recipe with the CLI's precedence: recipe, then preset (which
 * replaces it), then palette and every other knob. Pass procedural for generator sources.
 */
export function resolveRecipe(options: DitherOptions = {}, context: { procedural?: boolean } = {}): Recipe {
  let recipe = options.recipe === undefined ? clone(DEFAULT_RECIPE) : parseRecipe(options.recipe);
  if (options.preset !== undefined) {
    const preset = lookupPreset(options.preset);
    recipe =
      typeof preset === "string"
        ? presetRecipe(preset, context)
        : resolveRecipe({ ...preset.options, ...(preset.extends ? { preset: preset.extends } : {}) }, context);
  }
  if (options.palette !== undefined) recipe.palette = toPaletteSpec(options.palette);
  const target = recipe as unknown as Record<string, Record<string, unknown>>;
  for (const field of OPTION_FIELDS) {
    const value = options[field.key];
    if (value === undefined) continue;
    const section = target[field.section];
    if (section) section[field.field] = field.kind === "color" ? hexToRgb(value as HexColor) : value;
  }
  return recipe;
}

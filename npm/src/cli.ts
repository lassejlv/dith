import { DEFAULT_RECIPE, OPTION_FIELDS, PRESET_RECIPES, type PresetId, type Recipe } from "./generated/schema.js";
import { rgbToHex } from "./palette.js";
import { presetRecipe, resolveRecipe, type DitherOptions } from "./recipe.js";
import type { GeneratorSpec, RenderFormat } from "./types.js";

export interface CliCommandOptions {
  /** The recipe to reproduce; or pass options and they are resolved first. */
  recipe?: Recipe;
  /** Options to resolve into the recipe (ignored when recipe is given). */
  options?: DitherOptions;
  /** Input image path for "dith apply". Defaults to "input.png". */
  input?: string;
  /** Generator source: emits "dith generate" instead of "dith apply". */
  generator?: GeneratorSpec;
  /** Output path. Defaults to "out.<format>". */
  output?: string;
  /** Output format for the default output extension. Defaults to the recipe's, else PNG. */
  format?: RenderFormat;
  /** Path for the --recipe fallback. Defaults to "recipe.json". */
  recipeFile?: string;
}

export interface CliCommand {
  /** Shell command that reproduces the recipe byte for byte. */
  command: string;
  /** The same command as argv (unquoted). */
  argv: string[];
  /** False when a recipe field has no flag, so the command uses --recipe <file> instead. */
  flagsOnly: boolean;
  /** Recipe fields that no flag can express (empty when flagsOnly). */
  unsupported: string[];
}

function quote(arg: string): string {
  return /^[A-Za-z0-9_./:=+,-]+$/.test(arg) ? arg : "'" + arg.replaceAll("'", "'\\''") + "'";
}

const same = (a: unknown, b: unknown): boolean => JSON.stringify(a) === JSON.stringify(b);

/**
 * The "dith apply" or "dith generate" command that reproduces a recipe exactly: --preset plus
 * one flag per field that differs from it. Fields with no flag fall back to --recipe recipe.json.
 */
export function cliCommand(options: CliCommandOptions): CliCommand {
  const { generator } = options;
  const procedural = generator !== undefined;
  const recipe = options.recipe ?? resolveRecipe(options.options, { procedural });
  const builtin = recipe.preset !== null && recipe.preset in PRESET_RECIPES;
  const base = builtin ? presetRecipe(recipe.preset as PresetId, { procedural }) : structuredClone(DEFAULT_RECIPE);
  const format =
    options.format === undefined || options.format === "bitmap" || options.format === "rgba"
      ? (recipe.export.format ?? "png")
      : options.format;
  const output = options.output ?? "out." + (format === "jpeg" ? "jpg" : format);

  const head = procedural ? ["dith", "generate", generator.pattern] : ["dith", "apply", options.input ?? "input.png"];
  head.push("-o", output);
  if (procedural) {
    if ((generator.width ?? 640) !== 640) head.push("--canvas-width", String(generator.width));
    if ((generator.height ?? 480) !== 480) head.push("--canvas-height", String(generator.height));
    if ((generator.angle ?? 135) !== 135) head.push("--angle", String(generator.angle));
    if ((generator.frequency ?? 1) !== 1) head.push("--frequency", String(generator.frequency));
    if (generator.stops?.length) head.push("--stops", generator.stops.join(","));
  }

  const flags: string[] = [];
  const unsupported: string[] = [];
  if (builtin) flags.push("--preset", recipe.preset as string);
  else if (recipe.preset !== null) unsupported.push("preset");

  const p = recipe.palette;
  if (!same(p, base.palette)) {
    if (p.mode === "builtin") flags.push("--palette", p.value);
    else if (p.mode === "colors") flags.push("--colors", p.value.map(rgbToHex).join(","));
    else if (p.mode === "adaptive") flags.push("--auto-palette", String(p.value));
    else flags.push("--palette-file", p.value);
  }

  const sections = recipe as unknown as Record<string, Record<string, unknown>>;
  const baseSections = base as unknown as Record<string, Record<string, unknown>>;
  for (const field of OPTION_FIELDS) {
    const value = sections[field.section]?.[field.field];
    const before = baseSections[field.section]?.[field.field];
    if (same(value, before)) continue;
    const path = field.section + "." + field.field;
    if (field.kind === "toggle") flags.push(value ? field.flag : field.flag.replace("--", "--no-"));
    else if (field.kind === "enable") {
      if (value) flags.push(field.flag);
      else unsupported.push(path);
    } else if (field.kind === "color") flags.push(field.flag, rgbToHex(value as Recipe["preprocess"]["background"]));
    else if (value === null) unsupported.push(path);
    else flags.push(field.flag, String(value as string | number));
  }
  if (!same(recipe.dither.pattern_offset, base.dither.pattern_offset)) unsupported.push("dither.pattern_offset");
  if (recipe.export.format !== base.export.format) {
    if (recipe.export.format === null) unsupported.push("export.format");
    else flags.push("--format", recipe.export.format);
  }
  for (const key of ["text_width", "view_cols", "view_rows"] as const) {
    if (recipe.export[key] === base.export[key]) continue;
    if (recipe.export[key] === null) unsupported.push("export." + key);
    else flags.push("--" + key.replace("_", "-"), String(recipe.export[key]));
  }

  const argv =
    unsupported.length === 0 ? [...head, ...flags] : [...head, "--recipe", options.recipeFile ?? "recipe.json"];
  return { command: argv.map(quote).join(" "), argv, flagsOnly: unsupported.length === 0, unsupported };
}

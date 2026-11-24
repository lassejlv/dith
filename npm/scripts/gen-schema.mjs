// Generates src/generated/schema.ts from the dith Rust engine: the capabilities manifest and enum
// values come from the built WebAssembly module, field docs from the Rust doc comments, and error
// codes from RecipeError::code and the wasm runtime errors. Run after scripts/build-wasm.sh.
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { initSync, schema } from "../wasm/dith.js";

const root = new URL("../../", import.meta.url);
const read = (path) => readFileSync(new URL(path, root), "utf8");
initSync({ module: readFileSync(new URL("../wasm/dith_bg.wasm", import.meta.url)) });
const { capabilities: caps, enums } = JSON.parse(schema());

/** Doc comments of the public fields of a Rust struct, joined into one paragraph each. */
function fieldDocs(file, struct) {
  const src = read(file);
  const start = src.indexOf("pub struct " + struct + " {");
  if (start < 0) throw new Error("struct " + struct + " not found in " + file);
  const body = src.slice(start, src.indexOf("\n}", start));
  const docs = {};
  let pending = [];
  for (const line of body.split("\n").slice(1)) {
    const t = line.trim();
    if (t.startsWith("///")) pending.push(t.slice(3).trim());
    else if (t.startsWith("pub ")) {
      const name = t.slice(4, t.indexOf(":"));
      docs[name] = pending.join(" ");
      pending = [];
    } else if (!t.startsWith("#[")) pending = [];
  }
  return docs;
}

// Fields whose Rust definitions carry no doc comment.
const FALLBACK_DOCS = {
  algorithm: "Dithering algorithm; see ALGORITHMS for what each one is best for.",
  color_space: "Color space for error diffusion and nearest-color matching (oklab is the most accurate).",
};

const docs = {
  dither: { ...FALLBACK_DOCS, ...fieldDocs("src/dither.rs", "DitherOptions") },
  preprocess: fieldDocs("src/preprocess.rs", "PreprocessOptions"),
  export: fieldDocs("src/output.rs", "ExportOptions"),
};

const recipeCodes = [...read("src/recipe.rs").matchAll(/=> "([A-Z_]+)"/g)].map((m) => m[1]);
const runtimeCodes = [...read("src/wasm.rs").matchAll(/runtime_error\(\s*"([A-Z_]+)"/g)].map((m) => m[1]);
const errorCodes = [...new Set([...recipeCodes, ...runtimeCodes])];

const ranges = Object.fromEntries(caps.parameter_ranges.map((r) => [r.flag, r]));

for (const algo of caps.algorithms) {
  if (!enums.algorithms.includes(algo.id)) throw new Error("algorithm id mismatch: " + algo.id);
}

// Flat options: every recipe knob, named after its CLI flag. The same table drives option
// resolution and cliCommand(), so the two cannot drift apart.
const OPTIONS = [
  ["algorithm", "dither", "algorithm", "--algorithm", "value", "AlgorithmId"],
  ["colorSpace", "dither", "color_space", "--color-space", "value", "ColorSpace"],
  ["strength", "dither", "strength", "--strength", "value", "number"],
  ["serpentine", "dither", "serpentine", "--serpentine", "toggle", "boolean"],
  ["bias", "dither", "bias", "--bias", "value", "number"],
  ["noise", "dither", "noise", "--noise", "value", "number"],
  ["patternScale", "dither", "pattern_scale", "--pattern-scale", "value", "number"],
  ["seed", "dither", "seed", "--seed", "value", "number"],
  ["pixelSize", "preprocess", "pixel_size", "--pixel-size", "value", "number"],
  ["width", "preprocess", "width", "--width", "value", "number"],
  ["height", "preprocess", "height", "--height", "value", "number"],
  ["maxDim", "preprocess", "max_dim", "--max-dim", "value", "number"],
  ["scale", "preprocess", "scale", "--scale", "value", "number"],
  ["fit", "preprocess", "fit", "--fit", "value", "FitMode"],
  ["brightness", "preprocess", "brightness", "--brightness", "value", "number"],
  ["contrast", "preprocess", "contrast", "--contrast", "value", "number"],
  ["gamma", "preprocess", "gamma", "--gamma", "value", "number"],
  ["saturation", "preprocess", "saturation", "--saturation", "value", "number"],
  ["sharpen", "preprocess", "sharpen", "--sharpen", "value", "number"],
  ["blur", "preprocess", "blur", "--blur", "value", "number"],
  ["autoLevels", "preprocess", "auto_levels", "--auto-levels", "toggle", "boolean"],
  ["grayscale", "preprocess", "grayscale", "--grayscale", "toggle", "boolean"],
  ["invert", "preprocess", "invert", "--invert", "enable", "boolean"],
  ["alphaMode", "preprocess", "alpha_mode", "--alpha-mode", "value", "AlphaMode"],
  ["background", "preprocess", "background", "--background", "color", "HexColor"],
  ["svgShape", "export", "svg_shape", "--svg-shape", "value", "SvgShape"],
  ["svgSkipBg", "export", "svg_skip_bg", "--svg-skip-bg", "enable", "boolean"],
  ["optimize", "export", "optimize", "--optimize", "enable", "boolean"],
];

const DEFAULT = caps.default_recipe;
const q = (s) => JSON.stringify(s);
const doc = (text, indent = "  ") => {
  const lines = text.split("\n").filter(Boolean);
  return lines.length
    ? indent + "/**\n" + lines.map((l) => indent + " * " + l).join("\n") + "\n" + indent + " */\n"
    : "";
};
function fieldDoc(section, field, flag) {
  const parts = [docs[section][field] || FALLBACK_DOCS[field] || ""];
  const r = ranges[flag];
  if (r) parts.push("Range " + r.range.replace("..=", " to ") + ", default " + r.default + ". " + r.effect + ".");
  else if (DEFAULT[section] && field in DEFAULT[section])
    parts.push("Default " + JSON.stringify(DEFAULT[section][field]) + ".");
  if (flag) parts.push("CLI: " + flag);
  return parts.filter(Boolean).join("\n");
}

const tsType = {
  number: "number",
  boolean: "boolean",
  AlgorithmId: "AlgorithmId",
  ColorSpace: "ColorSpace",
  FitMode: "FitMode",
  AlphaMode: "AlphaMode",
  SvgShape: "SvgShape",
  HexColor: "HexColor",
};
const optionalFields = new Set(["width", "height", "max_dim", "scale"]);
function recipeFieldType(section, field, value) {
  const opt = OPTIONS.find((o) => o[1] === section && o[2] === field);
  if (field === "background") return "Rgb";
  if (field === "pattern_offset") return "[number, number]";
  if (field === "format") return "OutputFormat | null";
  if (["text_width", "view_cols", "view_rows"].includes(field)) return "number | null";
  if (opt) return tsType[opt[5]] + (optionalFields.has(field) ? " | null" : "");
  return typeof value;
}

function recipeSection(section, iface) {
  let out = "/** The " + q(section) + " section of a {@link Recipe}. */\nexport interface " + iface + " {\n";
  for (const [field, value] of Object.entries(DEFAULT[section])) {
    const opt = OPTIONS.find((o) => o[1] === section && o[2] === field);
    out +=
      doc(fieldDoc(section, field, opt?.[3])) + "  " + field + ": " + recipeFieldType(section, field, value) + ";\n";
  }
  return out + "}\n";
}

const pick = (obj, keys) => Object.fromEntries(keys.map((k) => [k, obj[k]]));

let out = "// Generated by scripts/gen-schema.mjs from the dith " + caps.version + " Rust engine. Do not edit.\n\n";
out +=
  "/** Version of the dith engine the types were generated from. */\nexport const ENGINE_VERSION = " +
  q(caps.version) +
  ";\n\n";
out +=
  '/** A CSS-style hex color such as "#0f172a" ("#rgb" and "#rrggbbaa" are accepted too). */\nexport type HexColor = `#${string}`;\n\n';
out +=
  "/** An 8-bit sRGB color as stored in recipes. */\nexport interface Rgb {\n  r: number;\n  g: number;\n  b: number;\n}\n\n";

const enumBlock = (name, type, values, what) =>
  "/** Every " +
  what +
  ". */\nexport const " +
  name +
  " = " +
  JSON.stringify(values) +
  " as const;\n/** One of {@link " +
  name +
  "}. */\nexport type " +
  type +
  " = (typeof " +
  name +
  ")[number];\n\n";
out += enumBlock("COLOR_SPACES", "ColorSpace", enums.color_spaces, "color space for diffusion and matching");
out += enumBlock("FIT_MODES", "FitMode", enums.fit_modes, "fit mode used when both width and height are set");
out += enumBlock("ALPHA_MODES", "AlphaMode", enums.alpha_modes, "transparency handling mode");
out += enumBlock("SVG_SHAPES", "SvgShape", enums.svg_shapes, "SVG shape primitive");
out += enumBlock(
  "OUTPUT_FORMATS",
  "OutputFormat",
  enums.output_formats,
  "output format a recipe can name (the CLI's --format)",
);

out +=
  "/** Every dithering algorithm with its metadata. */\nexport const ALGORITHMS = " +
  JSON.stringify(caps.algorithms, null, 2) +
  ' as const;\n/** One of the {@link ALGORITHMS} ids. */\nexport type AlgorithmId = (typeof ALGORITHMS)[number]["id"];\n\n';
out +=
  "/** Every built-in palette with its colors. */\nexport const PALETTES = " +
  JSON.stringify(caps.palettes, null, 2) +
  ' as const;\n/** A built-in palette id, or any "grayN" ramp with N in 2..=64. */\nexport type PaletteId = (typeof PALETTES)[number]["id"] | `gray${number}`;\n\n';
out +=
  "/** Every procedural generator with its metadata. */\nexport const GENERATORS = " +
  JSON.stringify(caps.generators, null, 2) +
  ' as const;\n/** One of the {@link GENERATORS} ids. */\nexport type GeneratorId = (typeof GENERATORS)[number]["id"];\n\n';
out +=
  "/** Every built-in preset (its recipe is in {@link PRESET_RECIPES}). */\nexport const PRESETS = " +
  JSON.stringify(
    caps.presets.map((p) => pick(p, ["id", "name", "description", "tags", "algorithm"])),
    null,
    2,
  ) +
  ' as const;\n/** One of the {@link PRESETS} ids. */\nexport type PresetId = (typeof PRESETS)[number]["id"];\n\n';
out +=
  "/** Error codes the engine reports, matching the CLI's --json error codes. */\nexport const ENGINE_ERROR_CODES = " +
  JSON.stringify(errorCodes) +
  " as const;\n/** One of {@link ENGINE_ERROR_CODES}. */\nexport type EngineErrorCode = (typeof ENGINE_ERROR_CODES)[number];\n\n";

out +=
  '/** How a recipe picks its palette. */\nexport type PaletteSpec =\n  | { mode: "builtin"; value: PaletteId }\n  | { mode: "colors"; value: Rgb[] }\n  | { mode: "file"; value: string }\n  | { mode: "adaptive"; value: number };\n\n';
out +=
  recipeSection("dither", "RecipeDither") +
  "\n" +
  recipeSection("preprocess", "RecipePreprocess") +
  "\n" +
  recipeSection("export", "RecipeExport") +
  "\n";
out +=
  '/** A complete recipe, exactly the JSON "dith apply --emit-recipe" prints and --recipe reads. */\nexport interface Recipe {\n  /** The preset the recipe started from, if any. */\n  preset: PresetId | null;\n  palette: PaletteSpec;\n  dither: RecipeDither;\n  preprocess: RecipePreprocess;\n  export: RecipeExport;\n}\n\n';
out +=
  "/** The default recipe (bw palette, Floyd-Steinberg). */\nexport const DEFAULT_RECIPE: Recipe = " +
  JSON.stringify(DEFAULT, null, 2) +
  ";\n\n";
out +=
  "/** The recipe each built-in preset resolves to for photos. */\nexport const PRESET_RECIPES: { readonly [K in PresetId]: Recipe } = " +
  JSON.stringify(Object.fromEntries(caps.presets.map((p) => [p.id, p.recipe])), null, 2) +
  ";\n\n";

out += "/** Every recipe knob as a flat, typed option named after its CLI flag. */\nexport interface RecipeOptions {\n";
for (const [key, section, field, flag, , type] of OPTIONS) {
  out += doc(fieldDoc(section, field, flag)) + "  " + key + "?: " + tsType[type] + ";\n";
}
out += "}\n\n";
out +=
  "/** How each flat option maps onto the recipe and the CLI. */\nexport const OPTION_FIELDS = " +
  JSON.stringify(
    OPTIONS.map(([key, section, field, flag, kind]) => ({ key, section, field, flag, kind })),
    null,
    2,
  ) +
  " as const;\n\n";
out +=
  "/** Numeric parameter ranges from the capabilities manifest. */\nexport const PARAMETER_RANGES = " +
  JSON.stringify(
    caps.parameter_ranges.map((r) => pick(r, ["flag", "range", "default"])),
    null,
    2,
  ) +
  " as const;\n";

mkdirSync(new URL("../src/generated/", import.meta.url), { recursive: true });
writeFileSync(new URL("../src/generated/schema.ts", import.meta.url), out);
console.log("wrote src/generated/schema.ts (" + out.length + " bytes, " + errorCodes.length + " error codes)");

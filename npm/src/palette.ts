import { DithError } from "./errors.js";
import type { HexColor, PaletteId, PaletteSpec, Rgb } from "./generated/schema.js";

/** Anything accepted as a palette: a built-in id, a color list, auto(n), or a recipe PaletteSpec. */
export type PaletteInput = PaletteId | readonly (HexColor | Rgb)[] | PaletteSpec;

/** Extract n colors from the image itself (Oklab k-means++, 2..=64), like --auto-palette n. */
export function auto(colors: number): { mode: "adaptive"; value: number } {
  return { mode: "adaptive", value: colors };
}

function nibble(c: string): number {
  const v = Number.parseInt(c, 16);
  if (Number.isNaN(v)) throw new Error(c);
  return v;
}

/** Parse "#rgb", "#rrggbb" or "#rrggbbaa" (the # is optional) exactly like the CLI. */
export function hexToRgb(input: string): Rgb {
  const trimmed = input.trim();
  const hex = trimmed.startsWith("#") ? trimmed.slice(1) : trimmed;
  const valid = /^[0-9a-fA-F]+$/.test(hex) && (hex.length === 3 || hex.length === 6 || hex.length === 8);
  if (!valid) {
    throw new DithError(
      "INVALID_HEX_COLOR",
      "invalid hex color '" + trimmed + "'",
      "Use hex format like #111111, #f5efe6, or shorthand #000.",
    );
  }
  if (hex.length === 3) {
    const [r, g, b] = hex.split("").map((c) => nibble(c) * 17) as [number, number, number];
    return { r, g, b };
  }
  return { r: nibble(hex.slice(0, 2)), g: nibble(hex.slice(2, 4)), b: nibble(hex.slice(4, 6)) };
}

/** Format a color as lowercase "#rrggbb". */
export function rgbToHex(color: Rgb): HexColor {
  return ("#" + [color.r, color.g, color.b].map((v) => v.toString(16).padStart(2, "0")).join("")) as HexColor;
}

function tooFew(count: number): DithError<"PALETTE_TOO_FEW_COLORS"> {
  return new DithError(
    "PALETTE_TOO_FEW_COLORS",
    "palette needs at least 2 colors (got " + count + ")",
    "Provide at least 2 distinct colors.",
  );
}

function parseError(reason: string): DithError<"PALETTE_PARSE_ERROR"> {
  return new DithError(
    "PALETTE_PARSE_ERROR",
    "failed to parse palette: " + reason,
    "Supported palette files are .hex/.txt (hex colors), .gpl (GIMP Palette), and .json (array of hex strings).",
  );
}

/**
 * Parse palette file text with the CLI's --palette-file rules: .json (an array of hex strings or
 * {"colors": [...]}), .gpl (GIMP Palette), or hex lists (.hex/.txt, one or more per line, with
 * "//" and ";" comments). Pass the file name or extension to pick the format.
 */
export function parsePalette(text: string, fileName = "palette.hex"): HexColor[] {
  const ext = (fileName.includes(".") ? fileName.slice(fileName.lastIndexOf(".") + 1) : fileName).toLowerCase();
  let colors: Rgb[];
  if (ext === "json") {
    let parsed: unknown;
    try {
      parsed = JSON.parse(text);
    } catch (error) {
      throw parseError("invalid JSON palette: " + (error instanceof Error ? error.message : String(error)));
    }
    const list = Array.isArray(parsed) ? parsed : (parsed as { colors?: unknown } | null)?.colors;
    if (!Array.isArray(list) || !list.every((v) => typeof v === "string"))
      throw parseError("invalid JSON palette: expected an array of hex strings");
    try {
      colors = list.map((v: string) => hexToRgb(v));
    } catch (error) {
      throw parseError(error instanceof Error ? error.message : String(error));
    }
  } else if (ext === "gpl" || text.startsWith("GIMP Palette")) {
    colors = [];
    for (const line of text.split("\n")) {
      const t = line.trim();
      if (!t || t.startsWith("#") || t.startsWith("GIMP") || t.startsWith("Name:") || t.startsWith("Columns:"))
        continue;
      const parts = t.split(/\s+/);
      if (parts.length < 3) continue;
      const channel = (name: string, v: string): number => {
        const n = /^\+?\d+$/.test(v) ? Number(v) : Number.NaN;
        if (!(n >= 0 && n <= 255)) throw parseError("invalid " + name + " channel '" + v + "'");
        return n;
      };
      colors.push({
        r: channel("red", parts[0] ?? ""),
        g: channel("green", parts[1] ?? ""),
        b: channel("blue", parts[2] ?? ""),
      });
    }
    if (colors.length < 2) throw parseError("fewer than 2 RGB entries found in GPL file");
  } else {
    colors = text
      .split("\n")
      .map((line) => (line.split("//")[0] ?? "").trim())
      .filter((line) => line && !line.startsWith(";"))
      .flatMap((line) => line.split(/[,\s]/))
      .filter(Boolean)
      .map(hexToRgb);
  }
  if (colors.length < 2) throw tooFew(colors.length);
  return colors.map(rgbToHex);
}

/** Read and parse a palette file (File/Blob, or a URL to fetch). */
export async function readPalette(file: Blob | URL | string): Promise<HexColor[]> {
  if (typeof file === "string" || file instanceof URL) {
    const url = String(file);
    return parsePalette(await (await fetch(url)).text(), new URL(url, "file:///").pathname);
  }
  return parsePalette(await file.text(), "name" in file && typeof file.name === "string" ? file.name : "palette.hex");
}

/** Turn any {@link PaletteInput} into the recipe's PaletteSpec. */
export function toPaletteSpec(input: PaletteInput): PaletteSpec {
  if (typeof input === "string") return { mode: "builtin", value: input };
  if (Array.isArray(input)) {
    const colors = (input as readonly (HexColor | Rgb)[]).map((c) =>
      typeof c === "string" ? hexToRgb(c) : { r: c.r, g: c.g, b: c.b },
    );
    if (colors.length < 2) throw tooFew(colors.length);
    return { mode: "colors", value: colors };
  }
  return input as PaletteSpec;
}

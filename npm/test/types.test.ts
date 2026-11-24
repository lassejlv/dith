// Type tests: these assertions are checked by "vp check" (type-aware lint with type checking).
import { describe, expectTypeOf, it } from "vite-plus/test";

import {
  DithError,
  isDithError,
  render,
  resolveRecipe,
  type AnyDithError,
  type BitmapResult,
  type BlobResult,
  type BytesResult,
  type DrawResult,
  type PaletteId,
  type PixelsResult,
  type PresetId,
  type Recipe,
  type RenderOptions,
  type TextResult,
} from "../src/index.js";

declare const input: Uint8Array;
declare const canvas: OffscreenCanvas;

/** Type-checks the body without running it (the declared values above do not exist at runtime). */
const typeOnly = (_body: () => void): void => undefined;

describe("types", () => {
  it("picks the result type from the format", () =>
    typeOnly(() => {
      expectTypeOf(render(input)).resolves.toEqualTypeOf<BytesResult<"png">>();
      expectTypeOf(render(input, { format: "webp" })).resolves.toEqualTypeOf<BytesResult<"webp">>();
      expectTypeOf(render(input, { format: "webp" }))
        .resolves.toHaveProperty("mime")
        .toEqualTypeOf<"image/webp">();
      expectTypeOf(render(input, { format: "svg" })).resolves.toEqualTypeOf<TextResult<"svg">>();
      expectTypeOf(render(input, { format: "svg" }))
        .resolves.toHaveProperty("text")
        .toBeString();
      expectTypeOf(render(input, { format: "bitmap" })).resolves.toEqualTypeOf<BitmapResult>();
      expectTypeOf(render(input, { format: "rgba" })).resolves.toEqualTypeOf<PixelsResult>();
      expectTypeOf(render(input, { format: "png", output: "blob" })).resolves.toEqualTypeOf<BlobResult<"png">>();
      expectTypeOf(render(input, { target: canvas })).resolves.toEqualTypeOf<DrawResult>();
    }));

  it("types every recipe field with literal unions", () => {
    expectTypeOf<Recipe["preset"]>().toEqualTypeOf<PresetId | null>();
    expectTypeOf<Recipe["dither"]["algorithm"]>().toEqualTypeOf<
      | "floyd-steinberg"
      | "atkinson"
      | "jarvis-judice-ninke"
      | "stucki"
      | "burkes"
      | "sierra"
      | "sierra-two-row"
      | "sierra-lite"
      | "ostromoukhov"
      | "bayer2"
      | "bayer4"
      | "bayer8"
      | "bayer16"
      | "halftone"
      | "clustered-dot"
      | "diagonal-lines"
      | "horizontal-lines"
      | "vertical-lines"
      | "crosshatch"
      | "blue-noise"
      | "ign"
      | "riemersma"
      | "random"
      | "threshold"
    >();
    expectTypeOf<"gameboy">().toExtend<PaletteId>();
    expectTypeOf<"gray12">().toExtend<PaletteId>();
    expectTypeOf<RenderOptions["background"]>().toEqualTypeOf<`#${string}` | undefined>();
    expectTypeOf(resolveRecipe).returns.toEqualTypeOf<Recipe>();
  });

  it("rejects invalid options at compile time", () =>
    typeOnly(() => {
      // @ts-expect-error unknown preset
      void render(input, { preset: "nope" });
      // @ts-expect-error unknown algorithm
      void render(input, { algorithm: "floyd" });
      // @ts-expect-error colors must be hex strings starting with #
      void render(input, { palette: ["111111", "eeeeee"] });
      // @ts-expect-error kitty is not a render format
      void render(input, { format: "kitty" });
      // @ts-expect-error unknown generator
      void render({ pattern: "spiral" });
    }));

  it("narrows errors on their code", () => {
    const error: unknown = new DithError("UNKNOWN_PRESET", "x");
    if (isDithError(error)) {
      expectTypeOf(error).toEqualTypeOf<AnyDithError>();
      if (error.code === "UNKNOWN_PRESET") expectTypeOf(error.validValues).toEqualTypeOf<PresetId[]>();
      if (error.code === "INVALID_ARGUMENT") expectTypeOf(error.validValues).toEqualTypeOf<undefined>();
      // @ts-expect-error not an error code
      if (error.code === "NOPE") void error;
    }
  });
});

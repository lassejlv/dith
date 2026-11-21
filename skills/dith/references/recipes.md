# Dith recipes

These are copy-ready commands for common jobs. Every recipe was run against the release CLI. Swap in real paths, sizes, and brand colors. After each render, verify the JSON report as described in [SKILL.md](../SKILL.md#verify-the-result).

## Web and UI assets

### Hero background with no source image

```sh
dith generate mesh -o hero.webp \
  --preset vapor-mesh --canvas-width 1600 --canvas-height 900 \
  --stops "#0f172a,#3b82f6,#f43f5e,#f8fafc" --seed 7 --json
```

- Change `--seed` for a different mesh layout and `--angle` for the light direction.
- For a lighter, editorial feel, use `--preset film-stipple` or `--preset editorial-print`.

### Brand-colored gradient banner

```sh
dith generate linear-gradient -o banner.png \
  --algorithm bayer8 --colors "#0f172a,#2563eb,#f5efe6" \
  --canvas-width 1500 --canvas-height 500 --angle 90 --pattern-scale 2 --json
```

`--pattern-scale 2` doubles the Bayer cell size so the texture survives retina downscaling and reads as deliberate.

### Square avatar or profile photo (1-bit Mac look)

```sh
dith apply portrait.jpg -o avatar.png \
  --preset mac-classic --width 512 --height 512 --fit cover --sharpen 0.5 --json
```

`--fit cover` center-crops to the exact size. If the face goes too dark, add `--gamma 0.8` or `--bias 0.1`.

### Social / Open Graph card (1200×630) in brand colors

```sh
dith apply photo.jpg -o og.png \
  --width 1200 --height 630 --fit cover \
  --algorithm blue-noise --colors "#0f172a,#2563eb,#f5efe6" --auto-levels --json
```

### Logo or icon as a transparent vector SVG

```sh
# Halftone dots, background removed
dith apply logo.png -o logo.svg \
  --preset halftone-zine --max-dim 160 \
  --svg-shape circle --svg-skip-bg --alpha-mode preserve --json

# Crisp pixel-art SVG with a 1-bit alpha mask
dith apply logo.png -o logo-px.svg \
  --algorithm bayer4 --colors "#1e3cc8,#f5efe6" --max-dim 64 \
  --svg-shape rect --svg-skip-bg --alpha-mode threshold --json
```

- SVG size scales with pixel count, so keep `--max-dim` small.
- `rect` gives the smallest files.
- The SVG has a `viewBox` and `crispEdges` rendering, so you can size it freely with CSS `width` and it stays sharp.

### CSS sprite animation

```sh
dith animate rings -o rings-sheet.png \
  --anim-format spritesheet --sheet-cols 12 --frames 12 --fps 12 \
  --canvas-width 128 --canvas-height 128 --preset gameboy --json
```

Set `--sheet-cols` equal to `--frames` to get a single row, which keeps the CSS trivial. Read `frame_dimensions` (W×H) and `frame_count` (N) from the report, then:

```css
.sprite {
  width: 128px;                         /* W */
  height: 128px;                        /* H */
  background: url(rings-sheet.png) 0 0 / 1536px 128px;  /* N*W x H */
  image-rendering: pixelated;
  animation: play 1s steps(12) infinite; /* N frames, N / fps seconds */
}
@keyframes play { to { background-position: -1536px 0; } }  /* -N*W */
```

## Animated GIFs and loops

### Ink-wash dissolve reveal

```sh
dith animate photo.jpg -o reveal.gif \
  --preset mac-classic --max-dim 480 --frames 20 --fps 20 \
  --sweep-bias -0.45..0.0 --easing ease-out --hold-end 10 --temporal locked --json
```

`--hold-end 10` adds 10 frames, so `frame_count` will be 30. `--temporal locked` keeps the stipple from crawling.

### Mosaic focus-in, boomerang

```sh
dith animate photo.jpg -o focus.gif \
  --preset gameboy --max-dim 320 --frames 16 --fps 10 \
  --sweep-pixel-size 12..2 --ping-pong --json
```

### Dithered fade-in, full color

```sh
dith animate photo.jpg -o fade.apng \
  --preset film-stipple --max-dim 480 --frames 20 --fps 20 \
  --sweep-brightness -0.7..0.0 --easing smoothstep --json
```

### Film boil on a still photo (living print)

```sh
dith animate photo.jpg -o boil.gif \
  --preset risograph --max-dim 480 --temporal boil --boil-rate 2 --boil-amount 0.06 \
  --frames 12 --fps 10 --json
```

`--boil-rate 2` animates on twos, which looks hand-made. Keep `--boil-amount` between about 0.03 and 0.1; higher values get noisy.

### Seamless procedural loop

```sh
dith animate sphere -o orb.gif --preset risograph --duration 2 --fps 20 --easing ease-in-out --json
```

### Retro palette cycling with drifting screen

```sh
dith animate plasma -o crt.gif \
  --palette sunset-dusk --algorithm bayer4 --cycle-palette \
  --screen-drift 1,0 --frames 24 --fps 20 --json
```

### Check a pattern's motion before rendering it big

```sh
dith animate sphere -o motion-check.png \
  --anim-format spritesheet --sheet-cols 8 --frames 8 \
  --canvas-width 160 --canvas-height 120 --preset risograph --json
```

Look at the strip to see what moves and in which direction. Then render the real size.

### Fit an animation under a file-size budget

```sh
dith animate sphere -o orb.gif \
  --preset film-stipple --stops "#0f172a,#94a3b8,#f8fafc" \
  --frames 36 --fps 20 --canvas-width 800 --canvas-height 600 \
  --max-size 1MB --json
```

- Check `bytes_written`, `frame_dimensions`, and `size_fit_scale` in the report.
- If the scale is very small, lower the cost another way and retry: fewer `--frames`, a locked ordered algorithm (`--algorithm bayer4 --temporal locked`), or fewer palette colors.

### Re-dither an existing GIF/APNG/WebP

```sh
dith animate input.gif -o retro.apng --preset gameboy --temporal locked --json
```

- Add `--max-frames N` to cap long inputs.
- `--fps` overrides the source timing only when you pass it explicitly.

### Frames for an external encoder or for visual review

```sh
dith animate sphere --frames-dir frames --frames 24 --json   # frames/frame_0001.png ...
```

## Exploration and review

### Compare algorithms on the same palette

```sh
dith compare photo.jpg -o algos.png \
  --algorithms floyd-steinberg,atkinson,ostromoukhov,bayer8,blue-noise,halftone \
  --palettes newsprint --tile-max-dim 300 --json
```

### Compare palettes with one algorithm, on a light frame

```sh
dith compare photo.jpg -o palettes.png \
  --palettes bw,newsprint,risograph-blue,gameboy --algorithm atkinson --light --json
```

### Export each variant as its own file

```sh
dith compare photo.jpg --out-dir variants --presets mac-classic,risograph,film-stipple --json
```

This writes `variants/<label>.png`.

### Compare procedural art with no input

```sh
dith compare --pattern sphere -o presets.png \
  --presets mac-classic,halftone-zine,risograph,gameboy,crt-amber,woodcut --json
```

## Reuse and scale

### Save a brand look as a project preset, then use it everywhere

```sh
dith preset save brand-stipple \
  --algorithm blue-noise --colors "#0f172a,#2563eb,#f5efe6" \
  --contrast 0.12 --sharpen 0.3 --auto-levels \
  --description "Brand stipple" --tags brand,web --scope project --json

dith apply photo.jpg -o out.png --preset brand-stipple --json
```

The preset is saved to `.dith/presets/brand-stipple.json`. Commit it so the whole team and CI get the same look. Use `--scope user` for a personal default.

### Batch a folder

```sh
dith batch assets/*.png --out-dir dithered --preset brand-stipple --max-dim 1200 --suffix "" --json
```

- `batch` takes files, not directories, so use a shell glob.
- `--suffix ""` keeps the original file names. The default suffix is `-dithered`.
- With `--no-clobber`, files that already exist count as failed items: `status` becomes `partial_failure` and the exit code is 1. Read `items[].error` to tell skipped files from real failures.

### Freeze an exact pipeline as a recipe and replay it

```sh
dith apply photo.jpg -o look.png --preset gameboy --contrast 0.2 --emit-recipe --json \
  | jq '.recipe' > look.recipe.json
dith apply other.jpg -o other.png --recipe look.recipe.json --json
```

- Replaying a recipe on the same input gives byte-identical output.
- Any explicit flag overrides the matching recipe field. Precedence is `--recipe`, then `--preset`, then explicit flags.
- `--recipe -` reads the recipe from stdin.

### Pipe through stdin and stdout

```sh
curl -sL https://example.com/img.png | dith apply - -o - --preset mac-classic --format png > out.png
```

With `-o -` there is no extension to infer from, so output is PNG unless you pass `--format`.

### Text art for a README or terminal

```sh
dith apply photo.jpg -o art.txt --format braille --text-width 60 --preset mac-classic --json
```

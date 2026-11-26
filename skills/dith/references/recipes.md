# Dith recipes

These are copy-ready commands for common jobs. Every recipe was run against the release CLI. Swap in real paths, sizes, and brand colors. After each render, verify the JSON report as described in [SKILL.md](../SKILL.md#verify-the-result).

## Web and UI assets

### Hero background with no source image

```sh
dith generate mesh -o hero.png \
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

## Characters and icons

### Custom character icon

A soft, dithered mascot with exact ink line work, drawn from hand-written SVGs only (the repository's `examples/cat-icon/` is this recipe end to end).

1. Write the SVGs on one canvas (here `viewBox="-1 -1 2 2"`, `width`/`height` 512):
   - `body.svg`: the silhouette's parts as separate filled elements, e.g. a head ellipse and two ears as rounded triangles (a path with `stroke-linejoin="round"` and a stroke in the fill color).
   - `blush.svg`: soft paint, e.g. two semi-transparent black ellipses through a `feGaussianBlur` filter.
   - `face.svg`: ink that never changes: nose, mouth, and the outline (the silhouette drawn with a thicker stroke, masked by the silhouette itself so only the outside ring shows).
   - `eyes-open.svg`, `eyes-half.svg`, `eyes-closed.svg`: one file per eye state.
2. Render the icon:

```sh
dith generate inflate -o cat.png \
  --shape body.svg --blend 24 --paint blush.svg \
  --angle 135 --ground-shadow --ambient 0.5 --depth 0.6 --specular 0.3 \
  --stops "#171717,#F5F1EA" --canvas-width 512 --canvas-height 512 \
  --algorithm atkinson --colors "#F5F1EA,#171717" --pixel-size 2 --bias 0.12 --no-auto-levels \
  --overlay face.svg --overlay eyes-open.svg --json
```

3. Animate a blink (4.4 s loop at 20 fps steps). `blink.json` is `[4200, 50, 100, 50]`:

```sh
dith animate inflate -o cat-blink.gif \
  --shape body.svg --blend 24 --paint blush.svg \
  --angle 135 --ground-shadow --ambient 0.5 --depth 0.6 --specular 0.3 \
  --stops "#171717,#F5F1EA" --canvas-width 512 --canvas-height 512 \
  --algorithm atkinson --colors "#F5F1EA,#171717" --pixel-size 2 --bias 0.12 --no-auto-levels \
  --cycles 0 --frames 4 --fps 20 --timings blink.json --temporal locked-diff \
  --overlay face.svg --overlay-frames eyes-open.svg,eyes-half.svg,eyes-closed.svg,eyes-half.svg --json
```

4. Prove only the eyes move: `dith diff --frames cat-blink.gif --loop --json`. `union.changed_columns` should be two runs (one per eye) and `union.changed_rows` one run around the eyes.

- `--blend` melts head and ears into one body; 0 keeps a hard crease.
- Lighter, sparser dots: raise `--ambient` or lower `--depth`. More volume: the reverse.
- Overlays snap to the `--pixel-size` grid and to the palette, so the eyes' white highlights come out in the paper color.
- The first GIF frame is byte-identical to `cat.png`, and the GIF only stores the eyes after it (about 7 KB).

### Hold one frame for seconds in a sequence

```sh
dith animate open.png:4200ms half.png:50ms shut.png:100ms half.png:50ms -o blink.apng \
  --algorithm atkinson --colors "#F5F1EA,#171717" --temporal locked-diff --json
```

`:84f` means 84 frames at `--fps`. APNG keeps exact delays; GIF rounds to 10 ms and says so in `timing_note`. With error diffusion, `locked-diff` stops the change from rippling across the picture: compare `changed_pixels_total` against a `--temporal locked` render.

### Check that an edit only touched what it should

```sh
dith diff before.png after.png -o changes.png --json
```

Read `changed_pixels`, `bbox`, `changed_rows` and `changed_columns`; `changes.png` shows the changed pixels in red over a faded copy.

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

# Dith CLI Reference

## Install, Update, or Run from Source

```sh
# Install prebuilt binary + agent skill (macOS ARM & Linux x86_64)
curl -fsSL https://raw.githubusercontent.com/lassejlv/dith/main/install.sh | bash

# Update installed binary (and refresh skill) from GitHub Releases
dith update
dith update --check --json

# Or install from source
cargo install --git https://github.com/lassejlv/dith

# Or run inside the dith source checkout
mbx build --release
mbx run -- --help
```

## Discovery

```sh
dith capabilities
dith list presets --json
dith list algorithms --category error-diffusion --json
dith list palettes --tag print --json
dith list generators --json
dith inspect photo.png --colors 6 --json
```

## Apply to Existing Images

```sh
# Preset with custom brand colors, resize cap, edge sharpening, and JSON recipe
dith apply photo.png -o hero.png \
  --preset editorial-print --colors "#0f172a,#2563eb,#f5efe6" \
  --max-dim 1200 --sharpen 0.4 --emit-recipe --json

# Adaptive Oklab palette extracted from the photo itself
dith apply photo.png -o poster.png \
  --algorithm ostromoukhov --auto-palette 8 --pixel-size 2 --json

# Crisp transparent-background SVG halftone badge
dith apply logo.png -o logo.svg \
  --preset halftone-zine --max-dim 180 \
  --svg-shape circle --svg-skip-bg --alpha-mode preserve --json

# Stdin/stdout piping and dry-run validation
cat photo.png | dith apply - -o out.png --preset mac-classic --json
dith apply photo.png -o out.png --preset risograph --dry-run --no-clobber --json
```

Key `apply` / `generate` / `batch` knobs:
- `--color-space oklab|linear|srgb` (default `oklab`)
- `--strength 0.0..=1.5`, `--bias -0.5..=0.5`, `--noise 0.0..=0.5`, `--pattern-scale 1..=32`, `--seed <u64>`, `--no-serpentine`
- `--width`, `--height`, `--max-dim`, `--scale`, `--fit contain|cover|stretch`, `--pixel-size 1..=64`
- `--brightness -1.0..=1.0`, `--contrast -1.0..=1.0`, `--gamma 0.1..=5.0`, `--saturation 0.0..=3.0`, `--sharpen 0.0..=5.0`, `--blur 0.0..=20.0`, `--auto-levels`, `--grayscale`, `--invert`
- `--alpha-mode preserve|flatten|threshold|dither`, `--background "#rrggbb"`
- `--palette <builtin|grayN|rgb332|web216>`, `--colors "#a,#b,..."`, `--palette-file <path.(hex|gpl|json)>`, `--auto-palette 2..=64`
- `--recipe <path|->` to replay a saved JSON recipe verbatim

## Procedural Generation (No Input Image)

Patterns: `linear-gradient`, `radial-gradient`, `mesh`, `sphere`, `waves`, `noise`, `rings`, `plasma`, `checkerboard`.

```sh
dith generate sphere -o orb.png \
  --preset risograph --canvas-width 640 --canvas-height 480 --angle 135 --json

dith generate mesh -o hero-bg.webp \
  --preset vapor-mesh --canvas-width 1280 --canvas-height 720 \
  --stops "#0f172a,#3b82f6,#f43f5e,#f8fafc" --seed 7 --json
```

## Animated Dithers (`animate`)

Inputs: procedural pattern name (`sphere`, `waves`, `mesh`, ...), animated `.gif`/`.apng`/`.webp`, multiple image files (frame sequence), or a single static image (auto-boils).
Outputs: `.gif` (exact-palette lossless), `.apng` (32-bit RGBA), `.png` with `--anim-format spritesheet` (`--sheet-cols N`), or `--frames-dir <DIR>`.

```sh
# Seamless procedural loop
dith animate mesh -o aurora.gif \
  --preset vapor-mesh --duration 2.0 --fps 18 --easing ease-in-out --json

# Static photo with Risograph film boil "on twos"
dith animate photo.png -o zine.gif \
  --preset risograph --temporal boil --boil-rate 2 --boil-amount 0.06 \
  --frames 12 --fps 10 --json

# Parameter sweeps (dissolve, de-pixelate focus-in, pulse breathing)
dith animate photo.png -o reveal.gif \
  --preset mac-classic --sweep-bias -0.45..0.0 --easing ease-out --hold-end 8 --json
dith animate photo.png -o focus.gif \
  --preset gameboy --sweep-pixel-size 12..2 --ping-pong --json

# Classic palette cycling + scrolling screen drift
dith animate plasma -o crt.gif \
  --palette sunset-dusk --algorithm bayer4 --cycle-palette \
  --screen-drift 1,0 --frames 24 --fps 16 --json

# Re-dither an existing GIF/APNG/WebP with a shared palette
dith animate input.gif -o retro.apng \
  --preset gameboy --temporal locked --json

# CSS / game spritesheet
dith animate rings -o rings-sheet.png \
  --anim-format spritesheet --sheet-cols 6 --frames 12 --json
```

Available sweeps: `--sweep-bias`, `--sweep-contrast`, `--sweep-brightness`, `--sweep-gamma`, `--sweep-strength`, `--sweep-pixel-size` (`START..END`, shaped by `--sweep-mode linear|pulse` and `--easing linear|ease-in-out|ease-in|ease-out|smoothstep|bounce|steps`).

## Compare, Presets, Batch, and Previews

```sh
# Labeled contact sheet (or --out-dir for individual variant files)
dith compare photo.png -o sheet.png \
  --presets mac-classic,editorial-print,risograph,film-stipple --json
dith compare --pattern sphere -o algos.png \
  --algorithms floyd-steinberg,atkinson,ostromoukhov,halftone,blue-noise --json

# Save, inspect, list, and delete local presets
dith preset save brand-hero \
  --algorithm blue-noise --colors "#0f172a,#2563eb,#f5efe6" \
  --contrast 0.12 --sharpen 0.3 --auto-levels \
  --description "Brand hero stipple" --tags "brand,hero" \
  --scope project --json
dith preset list --source local --json
dith preset show brand-hero --json
dith preset delete brand-hero --scope project --json

# Parallel batch processing
dith batch assets/*.png --out-dir ./dithered --preset brand-hero --no-clobber --json

# Terminal previews (fullscreen on TTY by default; use --inline in non-interactive shells)
dith preview photo.png --preset risograph --inline --mode braille --cols 60
dith preview anim.gif --preset gameboy   # interactive TTY animation player
```

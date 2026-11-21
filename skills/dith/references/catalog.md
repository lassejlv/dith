# Dith catalog

This is a snapshot of the catalogs built into the release. For the authoritative live list, run `dith list <kind> --json` or `dith preset list --json`. `dith list presets` also includes saved local presets visible from the current directory.

## Presets

Pass a preset with `--preset <id>` (or `-P`). Any explicit flag overrides the matching preset field. To see the full resolved recipe of a preset, run `dith preset show <id> --json`. On procedural sources (`generate`, `animate <pattern>`, `--pattern`), the preset's auto-levels, contrast, and sharpen are skipped unless passed explicitly.

| id | Algorithm | Palette | Look |
| --- | --- | --- | --- |
| `mac-classic` | atkinson | mac-1bit | Crisp 1-bit on warm off-white CRT glass, edge sharpened |
| `editorial-print` | ostromoukhov | newsprint | Silky monochrome on archival newsprint |
| `film-stipple` | blue-noise | ink-cream | Artifact-free stipple in India ink and cream |
| `halftone-zine` | halftone | newsprint | Bold 45° clustered-dot halftone |
| `risograph` | stucki | risograph-blue | Soy cobalt blue on cream stock |
| `risograph-red` | stucki | risograph-red | High-impact red on warm uncoated stock |
| `risograph-duo` | floyd-steinberg | risograph-duo | Cobalt and fluorescent pink two-drum overprint |
| `blueprint` | atkinson | blueprint | Cyanotype Prussian blue and chalk white |
| `woodcut` | diagonal-lines | vermilion-paper | Engraving lines in sumi ink, vermilion, and washi |
| `gameboy` | bayer4 | gameboy | 4-shade olive LCD, chunky 3 px pixels |
| `pico8` | bayer4 | pico8 | Vibrant 16-color fantasy console, chunky 3 px pixels |
| `crt-amber` | bayer8 | amber-crt | Warm amber terminal, chunky 2 px pixels |
| `phosphor` | horizontal-lines | phosphor-green | Green phosphor scanlines |
| `vapor-mesh` | bayer4 | cyber-magenta | Neon crosshatch for hero gradients and dark UIs |
| `lo-fi-photo` | floyd-steinberg | auto (8) | Adaptive 8-color palette extracted from the image |

## Palettes

Pass a palette with `--palette <id>` (or `-p`). There are other ways to supply colors:
- `--colors "#a,#b,..."` for exact hex colors
- `--palette-file <path>` for a `.hex`/`.txt` file (one hex per line), a `.gpl` (GIMP) file, or a `.json` file
- `--auto-palette N` to extract N colors (2–64) from the image
- `grayN` for any N from 2 to 64, e.g. `gray3` or `gray12`

| id | Colors | Category | Character |
| --- | --- | --- | --- |
| `bw` | 2 | monochrome | Pure black and white (the default) |
| `mac-1bit` | 2 | monochrome | Warm CRT off-white and charcoal |
| `gray4`, `gray8`, `grayN` | N | monochrome | Even grayscale ramps |
| `newsprint` | 2 | editorial | Charcoal ink on warm newsprint |
| `ink-cream` | 3 | editorial | Obsidian, warm midtone, cream |
| `blueprint` | 2 | editorial | Prussian blue and chalk |
| `sepia` | 4 | editorial | Espresso to parchment darkroom print |
| `cobalt-sand` | 3 | editorial | Midnight ink, electric cobalt, desert sand |
| `vermilion-paper` | 3 | editorial | Sumi black, vermilion, washi |
| `forest-mint` | 4 | editorial | Pine, moss, mint |
| `sunset-dusk` | 5 | editorial | Plum to apricot twilight |
| `risograph-blue` | 2 | print | Soy blue on cream |
| `risograph-red` | 2 | print | Vermilion on uncoated paper |
| `risograph-duo` | 4 | print | Cobalt, fluorescent pink, plum, paper |
| `cmyk` | 5 | print | Process C, M, Y, K, and paper |
| `amber-crt` | 4 | retro | Amber phosphor levels |
| `phosphor-green` | 4 | retro | P1 green phosphor levels |
| `gameboy` | 4 | retro | Original olive Game Boy LCD |
| `gameboy-pocket` | 4 | retro | Silver-gray Game Boy Pocket |
| `cga` | 4 | retro | Black, cyan, magenta, white |
| `c64` | 16 | retro | Commodore 64 (Pepto) |
| `pico8` | 16 | retro | PICO-8 fantasy console |
| `cyber-magenta` | 5 | modern | Midnight violet, hot magenta, cyan, ice |
| `nord` | 14 | theme | Nord design system |
| `gruvbox` | 12 | theme | Warm earthy Gruvbox |
| `catppuccin` | 12 | theme | Catppuccin Mocha pastels |
| `rgb332` | 256 | cube | 8-bit 3-3-2 RGB cube |
| `web216` | 216 | cube | Web-safe 6×6×6 cube |

Palette tips:
- Two colors make the strongest dithered texture. With more colors, the result moves toward posterized color and the dither texture becomes subtler.
- Order does not matter. Dith matches every pixel to the nearest palette color in Oklab.
- For "brand color on paper", use a dark ink, a mid accent, and a light paper tone (3 colors). If there are only two, make sure their lightness differs enough.

## Algorithms

Pass an algorithm with `--algorithm <id>` (or `-a`).

### Error diffusion

Error diffusion gives organic, detailed results. Serpentine scanning is on by default. In animation, any change spreads to neighbouring pixels (`temporally_stable: false`), so moving content makes static areas shimmer and GIFs grow. Atkinson is the least affected.

| id | Best for |
| --- | --- |
| `floyd-steinberg` | General photography, crisp detail, multi-color palettes (the default) |
| `atkinson` | 1-bit editorial and portraits, the Mac look; high contrast with airy highlights |
| `jarvis-judice-ninke` | Smooth photographic gradations |
| `stucki` | Sharp editorial and Risograph posters |
| `burkes` | Clean illustrations, fast batches |
| `sierra` | Natural photos with smooth shadows |
| `sierra-two-row` | Portraits and UI graphics |
| `sierra-lite` | Light vintage stipple, small icons |
| `ostromoukhov` | Gallery monochrome and duotone, the smoothest gradients |

### Ordered screens

Ordered screens give a regular, graphic texture and respond to `--pattern-scale`. Each pixel depends only on itself (`temporally_stable: true`), so with `--temporal locked` static areas never flicker and GIFs stay small.

| id | Best for |
| --- | --- |
| `bayer2` | Chunky bold retro icons |
| `bayer4` | Game Boy and pixel art, web textures, animation-stable dithers |
| `bayer8` | Smooth retro displays, CRT and cyberpunk |
| `bayer16` | Subtle ordered ramps and backgrounds |
| `halftone` | Newspaper, comic, zine, and pop-art prints |
| `clustered-dot` | Bold halftones at small sizes |
| `diagonal-lines` | Woodcut, currency engraving |
| `horizontal-lines` | CRT scanlines, broadcast looks |
| `vertical-lines` | Architectural posters, fluted glass |
| `crosshatch` | Pen and ink, sketches, bookplates |

### Stochastic

Stochastic algorithms are noise-based and grid-free. They respond to `--seed`. `blue-noise`, `ign`, and `random` are per-pixel thresholds, so they are stable in locked animation. `riemersma` is error diffusion along a curve and is not.

| id | Best for |
| --- | --- |
| `blue-noise` | Filmic stipple, modern hero art; artifact-free and animation-friendly |
| `ign` | Clean modern digital grain without tiling |
| `riemersma` | Painterly Hilbert-curve stipple with no directional bias |
| `random` | Gritty photocopy and punk zine textures |

### Baseline

| id | Best for |
| --- | --- |
| `threshold` | Hard posterize with no dithering: stencils, silkscreen, and A/B baselines |

## Generators

Generators are procedural sources for `generate`, `animate`, `compare --pattern`, and `preview --pattern`. Shape them with:
- `--canvas-width` and `--canvas-height` (defaults: 640×480 for `generate`, 480×360 for `animate`)
- `--angle` in degrees (default 135)
- `--frequency` (default 1.0)
- `--stops "#a,#b,..."`: source colors, interpolated in Oklab, before the dither palette is applied

| id | Use for | When animated |
| --- | --- | --- |
| `linear-gradient` | Hero backgrounds, banners, cards, tonal smoothness tests | Gradient direction rotates 360° per loop |
| `radial-gradient` | Spotlight backdrops, glowing orbs, covers | Glow center orbits a small circle while its radius pulses |
| `mesh` | Modern SaaS/AI hero visuals, album art, abstract posters | Color anchors orbit small circles, so colors drift and blend |
| `sphere` | A shaded 3D orb that shows off Atkinson, halftone, and Risograph looks | Sphere stays still; the light orbits 360°, moving the highlight |
| `waves` | Editorial dividers, kinetic posters, woodcut and scanline looks | Ribbons flow one wavelength per loop |
| `noise` | fBm clouds, paper and mist textures | Clouds drift along a circle and billow |
| `rings` | Hypnotic zine covers, optical posters | Rings ripple outward |
| `plasma` | Retro demos, synthwave, palette cycling | Field cycles through its period |
| `checkerboard` | Architectural graphics, retro UI tiles | Grid scrolls diagonally while the vignette light orbits |

`--motion-amplitude` scales the motion (0 = still, 2 = dramatic). For rings, plasma, and checkerboard it sets the whole number of cycles per loop. `--cycles` repeats the whole motion several times per loop. `dith list generators --json` has the exact `motion` text.

`--stops` sets the colors of the source image. The dither palette (`--palette`, `--colors`, or the preset's palette) decides the final ink colors. To get the stops' hues in the output, pass the same hexes to `--colors`, or use `--auto-palette`.

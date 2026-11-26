//! The `dith capabilities` command: the self-describing JSON manifest.

use anyhow::Result;
use serde::Serialize;

use dith_core::color::ColorSpace;
use dith_core::dither::{Algorithm, AlgorithmInfo};
use dith_core::generate::{Pattern, PatternInfo};
use dith_core::output::OutputFormat;
use dith_core::palette::{BuiltinPaletteInfo, list_builtin_palettes};
use dith_core::preset::{PresetSummary, list_all_presets};
use dith_core::recipe::DitherRecipe;

pub(crate) fn handle_capabilities() -> Result<()> {
    let manifest = build_capabilities();
    println!("{}", serde_json::to_string_pretty(&manifest)?);
    Ok(())
}

/// Machine-readable JSON schema returned by `dith capabilities` for zero-shot agent onboarding.
#[derive(Debug, Clone, Serialize)]
pub struct CapabilitiesManifest {
    pub tool: &'static str,
    pub version: &'static str,
    pub description: &'static str,
    pub agent_workflow_tips: &'static [&'static str],
    pub commands: Vec<CommandCapability>,
    pub algorithms: Vec<AlgorithmInfo>,
    pub presets: Vec<PresetSummary>,
    pub palettes: Vec<BuiltinPaletteInfo>,
    pub generators: Vec<PatternInfo>,
    pub color_spaces: Vec<NamedDescription>,
    pub output_formats: Vec<NamedDescription>,
    pub parameter_ranges: Vec<ParameterRange>,
    pub default_recipe: DitherRecipe,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommandCapability {
    pub name: &'static str,
    pub summary: &'static str,
    pub example: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct NamedDescription {
    pub id: &'static str,
    pub description: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ParameterRange {
    pub flag: &'static str,
    pub value_type: &'static str,
    pub range: &'static str,
    pub default: &'static str,
    pub effect: &'static str,
}

/// Build the full self-describing capabilities manifest.
#[must_use]
pub fn build_capabilities() -> CapabilitiesManifest {
    CapabilitiesManifest {
        tool: "dith",
        version: env!("CARGO_PKG_VERSION"),
        description: "Agent-friendly dithering toolkit CLI for creating crisp 1-bit, duotone, Risograph, halftone, blue-noise, retro, animated, and vector SVG dithers.",
        agent_workflow_tips: &[
            "Pass --json on any command for stable, machine-parseable output and structured errors.",
            "Run `dith inspect <image> --json` first to measure dynamic range, transparency, dominant colors, and get ready-to-run preset recommendations.",
            "Use `dith compare <input> -o sheet.png --presets mac-classic,editorial-print,risograph,film-stipple` to visually compare multiple aesthetics in one grid.",
            "Use `dith generate sphere -o orb.png --preset editorial-print` to craft procedural dither artwork from scratch with zero input files.",
            "Use `dith animate sphere -o orb.gif --preset risograph --frames 24 --fps 16` (or `dith animate photo.png -o boil.gif --temporal boil`) to build seamless animated dithers, or `--view` for live fullscreen terminal playback.",
            "Use `--emit-recipe` and `--recipe <file|->` to save, mutate, and replay exact JSON dithering pipelines.",
            "Save reusable project-local (`.dith/presets/`) or user-global presets with `dith preset save <name> ...` or `--save-preset <name>` on apply/generate.",
            "Use `--dry-run --json` to validate arguments and preview planned dimensions without writing files.",
            "Use `--format svg --svg-shape circle --svg-skip-bg` for crisp transparent-ready web SVGs, or `dith preview` / `--view` for fullscreen Kitty graphics previews (pass `--inline` to print in scrollback instead).",
        ],
        commands: vec![
            CommandCapability {
                name: "apply",
                summary: "Dither an input image (or '-' stdin) with a preset, algorithm, custom/adaptive palette, and tonal controls.",
                example: "dith apply photo.png -o out.png --preset editorial-print --max-dim 960 --json",
            },
            CommandCapability {
                name: "generate",
                summary: "Procedurally generate gradients, 3D spheres, aurora meshes, waves, or fBm noise and dither in one step.",
                example: "dith generate sphere -o sphere.png --preset risograph --width 800 --height 600 --json",
            },
            CommandCapability {
                name: "animate",
                summary: "Create animated dithers (GIF, APNG, spritesheet, PNG frames, or live fullscreen --view) from procedural loops, GIFs, sequences, or boiled photos.",
                example: "dith animate sphere -o orb.gif --preset risograph --frames 24 --fps 16 --temporal locked --json",
            },
            CommandCapability {
                name: "preview",
                summary: "Open a fullscreen terminal preview (default on TTY, or --inline) via Kitty graphics protocol, Braille, or ASCII without writing a file.",
                example: "dith preview photo.png --preset risograph",
            },
            CommandCapability {
                name: "preset",
                summary: "Save, list, inspect, or delete persistent local presets in .dith/presets/ (project) or ~/.config/dith/presets/ (user).",
                example: "dith preset save brand-hero --algorithm blue-noise --colors \"#0f172a,#2563eb,#f5efe6\" --json",
            },
            CommandCapability {
                name: "compare",
                summary: "Render a labeled contact sheet (to -o, --out-dir, or fullscreen with --view) comparing algorithms, presets, or palettes.",
                example: "dith compare photo.png -o sheet.png --algorithms atkinson,ostromoukhov,bayer4,blue-noise --json",
            },
            CommandCapability {
                name: "batch",
                summary: "Dither many images in parallel into an output directory with structured per-file reporting.",
                example: "dith batch img1.png img2.png --out-dir ./dithered --preset mac-classic --json",
            },
            CommandCapability {
                name: "inspect",
                summary: "Analyze dimensions, luminance percentiles, transparency, dominant hex colors, and recommended dithering commands.",
                example: "dith inspect photo.png --json",
            },
            CommandCapability {
                name: "list",
                summary: "List available algorithms, palettes, presets (including saved local presets), generators, or formats.",
                example: "dith list palettes --tag risograph --json",
            },
            CommandCapability {
                name: "update",
                summary: "Update dith from GitHub Releases (macOS ARM & Linux x86_64) and silently refresh agent skills.",
                example: "dith update --check --json",
            },
            CommandCapability {
                name: "capabilities",
                summary: "Emit this complete JSON discovery manifest of all commands, options, ranges, presets, and palettes.",
                example: "dith capabilities",
            },
        ],
        algorithms: Algorithm::ALL.iter().map(|a| a.info()).collect(),
        presets: list_all_presets(),
        palettes: list_builtin_palettes(),
        generators: Pattern::ALL.iter().map(|p| p.info()).collect(),
        color_spaces: vec![
            NamedDescription {
                id: "oklab",
                description: ColorSpace::Oklab.description(),
            },
            NamedDescription {
                id: "linear",
                description: ColorSpace::Linear.description(),
            },
            NamedDescription {
                id: "srgb",
                description: ColorSpace::Srgb.description(),
            },
        ],
        output_formats: vec![
            NamedDescription {
                id: OutputFormat::Png.id(),
                description: "Lossless pixel-exact PNG (default)",
            },
            NamedDescription {
                id: OutputFormat::Webp.id(),
                description: "Compact web-ready WebP",
            },
            NamedDescription {
                id: OutputFormat::Jpeg.id(),
                description: "JPEG (flattens alpha)",
            },
            NamedDescription {
                id: OutputFormat::Gif.id(),
                description: "Indexed GIF",
            },
            NamedDescription {
                id: OutputFormat::Bmp.id(),
                description: "BMP (1/4/8-bit palette when opaque with <=256 colors, else 24/32-bit)",
            },
            NamedDescription {
                id: OutputFormat::Tiff.id(),
                description: "Deflate-compressed lossless TIFF",
            },
            NamedDescription {
                id: OutputFormat::Svg.id(),
                description: "Crisp RLE vector SVG or circular halftone dots",
            },
            NamedDescription {
                id: OutputFormat::Kitty.id(),
                description: "Pixel-exact inline terminal preview via Kitty graphics protocol (Kitty, Ghostty, WezTerm)",
            },
            NamedDescription {
                id: OutputFormat::Braille.id(),
                description: "High-density Unicode Braille text preview",
            },
            NamedDescription {
                id: OutputFormat::Ascii.id(),
                description: "Shaded ASCII text art",
            },
        ],
        parameter_ranges: vec![
            ParameterRange {
                flag: "--strength",
                value_type: "f32",
                range: "0.0..=1.5",
                default: "1.0",
                effect: "Scales error diffusion or ordered screen amplitude",
            },
            ParameterRange {
                flag: "--bias",
                value_type: "f32",
                range: "-0.5..=0.5",
                default: "0.0",
                effect: "Shifts dither threshold darker (<0) or lighter (>0)",
            },
            ParameterRange {
                flag: "--noise",
                value_type: "f32",
                range: "0.0..=0.5",
                default: "0.0",
                effect: "Injects seeded jitter to break regular patterns or banding",
            },
            ParameterRange {
                flag: "--pattern-scale",
                value_type: "u32",
                range: "1..=32",
                default: "1",
                effect: "Enlarges ordered/halftone/blue-noise screen cells",
            },
            ParameterRange {
                flag: "--pixel-size",
                value_type: "u32",
                range: "1..=64",
                default: "1",
                effect: "Downscales before dithering then nearest-upscales for crisp chunky pixels",
            },
            ParameterRange {
                flag: "--brightness",
                value_type: "f32",
                range: "-1.0..=1.0",
                default: "0.0",
                effect: "Linear brightness shift before dithering",
            },
            ParameterRange {
                flag: "--contrast",
                value_type: "f32",
                range: "-1.0..=1.0",
                default: "0.0",
                effect: "Contrast curve around midtones before dithering",
            },
            ParameterRange {
                flag: "--gamma",
                value_type: "f32",
                range: "0.1..=5.0",
                default: "1.0",
                effect: "Midtone gamma (<1 brightens, >1 darkens)",
            },
            ParameterRange {
                flag: "--saturation",
                value_type: "f32",
                range: "0.0..=3.0",
                default: "1.0",
                effect: "Color saturation multiplier in linear light",
            },
            ParameterRange {
                flag: "--sharpen",
                value_type: "f32",
                range: "0.0..=5.0",
                default: "0.0",
                effect: "Unsharp mask intensity before dithering (great for 1-bit edges)",
            },
            ParameterRange {
                flag: "--blur",
                value_type: "f32",
                range: "0.0..=20.0",
                default: "0.0",
                effect: "Pre-dither Gaussian blur sigma to calm sensor noise",
            },
            ParameterRange {
                flag: "--ink-below",
                value_type: "f32",
                range: "0.0..=1.0",
                default: "off",
                effect: "Tones darker than this luminance become solid ink (the darkest palette color) instead of dots",
            },
            ParameterRange {
                flag: "--paper-above",
                value_type: "f32",
                range: "0.0..=1.0",
                default: "off",
                effect: "Tones lighter than this luminance stay clean paper (the lightest palette color), so skies get no stray dots",
            },
            ParameterRange {
                flag: "--ink-lines",
                value_type: "f32",
                range: "0.0..=1.0",
                default: "off (0)",
                effect: "Finds the source's outlines and draws them as solid ink; higher values pick up fainter lines",
            },
            ParameterRange {
                flag: "--auto-palette",
                value_type: "usize",
                range: "2..=64",
                default: "none",
                effect: "Extracts N perceptual colors from the image using Oklab k-means++",
            },
            ParameterRange {
                flag: "--temporal",
                value_type: "enum",
                range: "locked | boil | grain",
                default: "locked (boil for static photos)",
                effect: "Controls how the dither pattern evolves across animation frames",
            },
            ParameterRange {
                flag: "--boil-rate",
                value_type: "u32",
                range: "1..=60",
                default: "1",
                effect: "Updates film boil every N frames (2 = animated on twos, 3 = on threes)",
            },
            ParameterRange {
                flag: "--boil-amount",
                value_type: "f32",
                range: "0.0..=0.5",
                default: "temporal default",
                effect: "Explicit boil/grain jitter intensity per animation frame",
            },
            ParameterRange {
                flag: "--screen-drift",
                value_type: "DX,DY",
                range: "integers",
                default: "none",
                effect: "Scrolls ordered/halftone/scanline screen cells smoothly across frames",
            },
            ParameterRange {
                flag: "--easing",
                value_type: "enum",
                range: "linear | ease-in-out | ease-in | ease-out | smoothstep | bounce | steps",
                default: "linear",
                effect: "Easing curve applied to procedural motion and --sweep-* animations",
            },
            ParameterRange {
                flag: "--sweep-bias / --sweep-contrast / --sweep-brightness / --sweep-gamma / --sweep-strength / --sweep-pixel-size",
                value_type: "START..END",
                range: "parameter domain",
                default: "none",
                effect: "Animates dithering or tonal knobs across frames (dissolves, mosaic focus-in, pulse breathing)",
            },
            ParameterRange {
                flag: "--cycle-palette",
                value_type: "bool",
                range: "true | false",
                default: "false",
                effect: "Classic Amiga/VGA palette color cycling across animation frames",
            },
            ParameterRange {
                flag: "--optimize",
                value_type: "bool",
                range: "true | false",
                default: "false",
                effect: "Smallest lossless PNG/APNG/spritesheet via palette-order and filter search plus Zopfli (encodes several times slower)",
            },
        ],
        default_recipe: DitherRecipe::default(),
    }
}

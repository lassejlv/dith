//! Browser and Node API compiled to WebAssembly (`--no-default-features --features wasm`).
//!
//! Recipes are the same JSON the CLI prints with `--emit-recipe`, so a recipe rendered here and
//! replayed with `dith apply <input> -o out.png --recipe recipe.json` produces identical bytes.
//! Errors are thrown as `Error` objects whose message is a JSON envelope with `code`, `message`,
//! `suggestion`, and optional `valid_values`, matching the CLI's `--json` error output.

use clap::ValueEnum;
use image::RgbaImage;
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

use crate::color::{ColorSpace, Rgb};
use crate::generate::{
    DEFAULT_SOURCE_STOPS, GenerateOptions, Pattern, generate_pattern, take_canvas_size,
};
use crate::output::{ColorUsage, OutputFormat, encode_image};
use crate::palette::Palette;
use crate::preprocess::{FitMode, PreprocessOptions, plan_dimensions, prepare_image};
use crate::recipe::{DitherRecipe, RecipeError, build_capabilities, run_pipeline};

#[derive(Serialize)]
struct ErrorEnvelope {
    status: &'static str,
    code: &'static str,
    message: String,
    suggestion: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    valid_values: Option<Vec<String>>,
}

fn recipe_error(err: &RecipeError) -> JsError {
    envelope_error(&ErrorEnvelope {
        status: "error",
        code: err.code(),
        message: err.to_string(),
        suggestion: err.suggestion(),
        valid_values: err.valid_values(),
    })
}

fn runtime_error(code: &'static str, message: String, suggestion: &str) -> JsError {
    envelope_error(&ErrorEnvelope {
        status: "error",
        code,
        message,
        suggestion: suggestion.to_string(),
        valid_values: None,
    })
}

fn envelope_error(env: &ErrorEnvelope) -> JsError {
    JsError::new(&serde_json::to_string(env).unwrap_or_else(|_| env.message.clone()))
}

fn parse_recipe(json: &str) -> Result<DitherRecipe, JsError> {
    let recipe = serde_json::from_str::<DitherRecipe>(json)
        .map_err(|e| recipe_error(&RecipeError::InvalidRecipeJson(e.to_string())))?;
    recipe.validate().map_err(|e| recipe_error(&e))?;
    Ok(recipe)
}

fn parse_format(id: &str) -> Result<OutputFormat, JsError> {
    OutputFormat::from_str(id, true).map_err(|_| {
        recipe_error(&RecipeError::Validation {
            field: "format",
            reason: format!("unknown output format '{id}'"),
        })
    })
}

/// Decode with the same decoders the CLI uses for each format. Only the browser formats are
/// linked in (TIFF decoding would add tens of kilobytes for a format browsers cannot show).
fn decode(bytes: &[u8]) -> Result<RgbaImage, JsError> {
    use image::codecs::{
        bmp::BmpDecoder, gif::GifDecoder, jpeg::JpegDecoder, png::PngDecoder, webp::WebPDecoder,
    };
    use image::{DynamicImage, ImageFormat, ImageResult};
    use std::io::Cursor;

    let decoded: ImageResult<DynamicImage> = match image::guess_format(bytes) {
        Ok(ImageFormat::Png) => {
            PngDecoder::new(Cursor::new(bytes)).and_then(DynamicImage::from_decoder)
        }
        Ok(ImageFormat::Jpeg) => {
            JpegDecoder::new(Cursor::new(bytes)).and_then(DynamicImage::from_decoder)
        }
        Ok(ImageFormat::WebP) => {
            WebPDecoder::new(Cursor::new(bytes)).and_then(DynamicImage::from_decoder)
        }
        Ok(ImageFormat::Gif) => {
            GifDecoder::new(Cursor::new(bytes)).and_then(DynamicImage::from_decoder)
        }
        Ok(ImageFormat::Bmp) => {
            BmpDecoder::new(Cursor::new(bytes)).and_then(DynamicImage::from_decoder)
        }
        Ok(other) => {
            return Err(runtime_error(
                "UNSUPPORTED_INPUT_FORMAT",
                format!("{other:?} input is not supported in the WebAssembly build"),
                "Convert the image to PNG, JPEG, WebP, GIF, or BMP.",
            ));
        }
        Err(e) => Err(e),
    };
    decoded.map(DynamicImage::into_rgba8).map_err(|e| {
        runtime_error(
            "DECODE_ERROR",
            format!("decoding input image: {e}"),
            "Pass PNG, JPEG, WebP, GIF, or BMP bytes.",
        )
    })
}

const fn mime_type(format: OutputFormat) -> &'static str {
    match format {
        OutputFormat::Png => "image/png",
        OutputFormat::Webp => "image/webp",
        OutputFormat::Jpeg => "image/jpeg",
        OutputFormat::Gif => "image/gif",
        OutputFormat::Bmp => "image/bmp",
        OutputFormat::Tiff => "image/tiff",
        OutputFormat::Svg => "image/svg+xml",
        _ => "text/plain;charset=utf-8",
    }
}

/// Procedural source settings, mirroring the `dith generate` arguments.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
struct GeneratorSpec {
    pattern: Pattern,
    canvas_width: u32,
    canvas_height: u32,
    angle: f32,
    frequency: f32,
    stops: Option<Vec<String>>,
}

impl Default for GeneratorSpec {
    fn default() -> Self {
        Self {
            pattern: Pattern::Sphere,
            canvas_width: 640,
            canvas_height: 480,
            angle: 135.0,
            frequency: 1.0,
            stops: None,
        }
    }
}

enum SourceKind {
    Image {
        image: RgbaImage,
        name: String,
    },
    Generator {
        spec: GeneratorSpec,
        stops: Vec<Rgb>,
    },
}

/// Geometry a preview proxy was resized for: width, height, max-dim, scale bits, fit, cap.
type ProxyKey = (
    Option<u32>,
    Option<u32>,
    Option<u32>,
    Option<u32>,
    FitMode,
    u32,
);

/// A decoded image or a procedural generator that recipes are rendered from.
///
/// Decoding happens once; every [`Source::render`] call reuses the decoded pixels, and previews
/// reuse a cached reduced copy until the recipe's geometry changes.
#[wasm_bindgen]
pub struct Source {
    kind: SourceKind,
    proxy: Option<(ProxyKey, RgbaImage)>,
}

impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (w, h) = self.dimensions();
        f.debug_struct("Source")
            .field("label", &self.label())
            .field("width", &w)
            .field("height", &h)
            .finish_non_exhaustive()
    }
}

/// Encoded render result plus the CLI-compatible JSON report.
#[wasm_bindgen]
#[derive(Debug)]
pub struct Rendered {
    bytes: Vec<u8>,
    mime: &'static str,
    format: &'static str,
    width: u32,
    height: u32,
    preview: bool,
    report: String,
}

#[wasm_bindgen]
impl Rendered {
    /// Encoded output bytes (or raw RGBA8 pixels for the `rgba` format). Moves the buffer out,
    /// so a second call returns an empty array.
    #[must_use]
    #[wasm_bindgen(js_name = takeBytes)]
    pub fn take_bytes(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.bytes)
    }

    /// MIME type of the bytes.
    #[must_use]
    #[wasm_bindgen(getter)]
    pub fn mime(&self) -> String {
        self.mime.to_string()
    }

    /// Output format id (`png`, `webp`, `svg`, ..., or `rgba`).
    #[must_use]
    #[wasm_bindgen(getter)]
    pub fn format(&self) -> String {
        self.format.to_string()
    }

    /// Output width in pixels.
    #[must_use]
    #[wasm_bindgen(getter)]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Output height in pixels.
    #[must_use]
    #[wasm_bindgen(getter)]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Whether this is a reduced-size preview rather than the exact recipe output.
    #[must_use]
    #[wasm_bindgen(getter)]
    pub fn preview(&self) -> bool {
        self.preview
    }

    /// JSON report with the same fields as `dith apply --json`, plus `preview`.
    #[must_use]
    #[wasm_bindgen(getter)]
    pub fn report(&self) -> String {
        self.report.clone()
    }
}

#[derive(Serialize)]
struct RenderReport<'a> {
    status: &'static str,
    dry_run: bool,
    source: String,
    output: Option<String>,
    format: &'static str,
    preset: Option<&'a str>,
    algorithm: &'static str,
    color_space: ColorSpace,
    palette_name: String,
    palette_colors: Option<Vec<String>>,
    source_dimensions: [u32; 2],
    working_dimensions: [u32; 2],
    output_dimensions: [u32; 2],
    bytes_written: usize,
    elapsed_ms: u128,
    color_usage: Vec<ColorUsage>,
    preview: bool,
    recipe: &'a DitherRecipe,
}

/// What the pipeline runs on for one render.
struct Prepared {
    label: String,
    source_dims: (u32, u32),
    image: RgbaImage,
    recipe: DitherRecipe,
    preview: bool,
}

/// Scale the resolution-dependent knobs of `recipe` by `ratio` for a reduced-size preview and
/// drop its geometry (the preview source already has the right size).
fn preview_recipe(recipe: &DitherRecipe, ratio: f32) -> DitherRecipe {
    let scaled = |v: u32| ((v as f32 * ratio).round() as u32).max(1);
    let mut preview = recipe.clone();
    preview.preprocess.width = None;
    preview.preprocess.height = None;
    preview.preprocess.max_dim = None;
    preview.preprocess.scale = None;
    preview.preprocess.fit = FitMode::Contain;
    preview.preprocess.pixel_size = scaled(recipe.preprocess.pixel_size);
    preview.preprocess.blur = recipe.preprocess.blur * ratio;
    preview.dither.pattern_scale = scaled(recipe.dither.pattern_scale);
    preview
}

#[wasm_bindgen]
impl Source {
    /// Decode an encoded image (PNG, JPEG, WebP, GIF, BMP, or TIFF). `name` labels the report's
    /// `source` field, like the input path does on the CLI.
    ///
    /// # Errors
    /// Throws when the bytes are not a supported image.
    #[wasm_bindgen(js_name = fromBytes)]
    pub fn from_bytes(bytes: &[u8], name: Option<String>) -> Result<Self, JsError> {
        Ok(Self::image(decode(bytes)?, name))
    }

    /// Wrap raw RGBA8 pixels (for example from a canvas) as a source.
    ///
    /// # Errors
    /// Throws when `pixels.len() != width * height * 4`.
    #[wasm_bindgen(js_name = fromRgba)]
    pub fn from_rgba(
        pixels: Vec<u8>,
        width: u32,
        height: u32,
        name: Option<String>,
    ) -> Result<Self, JsError> {
        let image = RgbaImage::from_raw(width, height, pixels).ok_or_else(|| {
            runtime_error(
                "INVALID_PIXELS",
                format!("expected {width}x{height}x4 RGBA bytes"),
                "Pass width * height * 4 bytes of RGBA8 pixel data.",
            )
        })?;
        Ok(Self::image(image, name))
    }

    /// A procedural generator source configured like `dith generate`: a JSON object with
    /// `pattern`, `canvas_width`, `canvas_height`, `angle`, `frequency`, and `stops` (hex strings).
    ///
    /// # Errors
    /// Throws on unknown patterns or invalid stop colors.
    #[wasm_bindgen(js_name = fromGenerator)]
    pub fn from_generator(spec_json: &str) -> Result<Self, JsError> {
        let spec: GeneratorSpec = serde_json::from_str(spec_json).map_err(|e| {
            recipe_error(&RecipeError::Validation {
                field: "generator",
                reason: e.to_string(),
            })
        })?;
        let stops = match &spec.stops {
            Some(list) => Palette::from_hex_list(&list.join(","))
                .map_err(|e| recipe_error(&RecipeError::from(e)))?
                .colors()
                .to_vec(),
            None => DEFAULT_SOURCE_STOPS.to_vec(),
        };
        Ok(Self {
            kind: SourceKind::Generator { spec, stops },
            proxy: None,
        })
    }

    /// Source width in pixels (the canvas width for generators).
    #[must_use]
    #[wasm_bindgen(getter)]
    pub fn width(&self) -> u32 {
        self.dimensions().0
    }

    /// Source height in pixels (the canvas height for generators).
    #[must_use]
    #[wasm_bindgen(getter)]
    pub fn height(&self) -> u32 {
        self.dimensions().1
    }

    /// Whether this source is procedural (`dith generate`) rather than an image (`dith apply`).
    #[must_use]
    #[wasm_bindgen(getter)]
    pub fn procedural(&self) -> bool {
        matches!(self.kind, SourceKind::Generator { .. })
    }

    /// Output dimensions `[width, height]` the recipe would produce, without rendering.
    ///
    /// # Errors
    /// Throws on invalid recipe JSON.
    #[wasm_bindgen(js_name = planDimensions)]
    pub fn plan(&self, recipe_json: &str) -> Result<Vec<u32>, JsError> {
        let mut recipe = parse_recipe(recipe_json)?;
        let (w, h) = self.canvas(&mut recipe);
        let (out_w, out_h) = plan_dimensions(w, h, &recipe.preprocess);
        Ok(vec![out_w, out_h])
    }

    /// The undithered source as RGBA8 at the size [`Source::render`] would use for the same
    /// arguments (resized to the recipe's geometry, or the generated canvas), for before/after views.
    ///
    /// # Errors
    /// Throws on invalid recipe JSON.
    pub fn original(
        &mut self,
        recipe_json: &str,
        preview_max_dim: Option<u32>,
    ) -> Result<Rendered, JsError> {
        let recipe = parse_recipe(recipe_json)?;
        let prepared = self.prepare(&recipe, preview_max_dim);
        let geometry = PreprocessOptions {
            width: prepared.recipe.preprocess.width,
            height: prepared.recipe.preprocess.height,
            max_dim: prepared.recipe.preprocess.max_dim,
            scale: prepared.recipe.preprocess.scale,
            fit: prepared.recipe.preprocess.fit,
            ..PreprocessOptions::default()
        };
        let image = prepare_image(&prepared.image, &geometry).working;
        Ok(Rendered {
            width: image.width(),
            height: image.height(),
            bytes: image.into_raw(),
            mime: "application/octet-stream",
            format: "rgba",
            preview: prepared.preview,
            report: String::new(),
        })
    }

    /// Render `recipe_json` and encode it as `format` (defaults to the recipe's export format,
    /// else PNG; `rgba` returns raw pixels). With `preview_max_dim`, outputs whose longest side
    /// exceeds it are rendered approximately at that size for interactive feedback.
    ///
    /// # Errors
    /// Throws a JSON error envelope on invalid recipes, palettes, or encoder failures.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "wasm-bindgen passes optional strings by value"
    )]
    pub fn render(
        &mut self,
        recipe_json: &str,
        format: Option<String>,
        preview_max_dim: Option<u32>,
    ) -> Result<Rendered, JsError> {
        let recipe = parse_recipe(recipe_json)?;
        let format = match format.as_deref() {
            Some("rgba") => None,
            Some(id) => Some(parse_format(id)?),
            None => Some(recipe.export.format.unwrap_or_default()),
        };

        let prepared = self.prepare(&recipe, preview_max_dim);
        let output =
            run_pipeline(&prepared.image, &prepared.recipe).map_err(|e| recipe_error(&e))?;
        let (bytes, format_id, mime) = match format {
            Some(f) => (
                encode_image(&output.image, f, &recipe.export).map_err(|e| {
                    runtime_error(
                        "ENCODE_ERROR",
                        format!("{e:#}"),
                        "Try another output format.",
                    )
                })?,
                f.id(),
                mime_type(f),
            ),
            None => (output.image.into_raw(), "rgba", "application/octet-stream"),
        };

        let report = RenderReport {
            status: "ok",
            dry_run: false,
            source: prepared.label,
            output: None,
            format: format_id,
            preset: recipe.preset.as_deref(),
            algorithm: recipe.dither.algorithm.id(),
            color_space: recipe.dither.color_space,
            palette_name: output.palette.name.clone(),
            palette_colors: Some(output.palette.hex_colors()),
            source_dimensions: [prepared.source_dims.0, prepared.source_dims.1],
            working_dimensions: [output.working_width, output.working_height],
            output_dimensions: [output.output_width, output.output_height],
            bytes_written: bytes.len(),
            elapsed_ms: output.elapsed_ms,
            color_usage: output.histogram,
            preview: prepared.preview,
            recipe: &recipe,
        };
        Ok(Rendered {
            report: serde_json::to_string(&report).unwrap_or_default(),
            bytes,
            mime,
            format: format_id,
            width: output.output_width,
            height: output.output_height,
            preview: prepared.preview,
        })
    }
}

impl Source {
    fn image(image: RgbaImage, name: Option<String>) -> Self {
        Self {
            kind: SourceKind::Image {
                image,
                name: name.unwrap_or_else(|| "input".to_string()),
            },
            proxy: None,
        }
    }

    fn dimensions(&self) -> (u32, u32) {
        match &self.kind {
            SourceKind::Image { image, .. } => image.dimensions(),
            SourceKind::Generator { spec, .. } => (spec.canvas_width, spec.canvas_height),
        }
    }

    fn label(&self) -> String {
        match &self.kind {
            SourceKind::Image { name, .. } => name.clone(),
            SourceKind::Generator { spec, .. } => format!("generate:{}", spec.pattern.id()),
        }
    }

    /// Source size the recipe's geometry applies to; for generators this mirrors
    /// `dith generate`, where an explicit recipe width/height replaces the canvas size.
    fn canvas(&self, recipe: &mut DitherRecipe) -> (u32, u32) {
        match &self.kind {
            SourceKind::Image { image, .. } => image.dimensions(),
            SourceKind::Generator { spec, .. } => take_canvas_size(
                &mut recipe.preprocess,
                spec.canvas_width,
                spec.canvas_height,
            ),
        }
    }

    /// The exact source and recipe, or a reduced proxy plus scaled recipe when a preview cap
    /// applies.
    fn prepare(&mut self, recipe: &DitherRecipe, preview_max_dim: Option<u32>) -> Prepared {
        let mut exact = recipe.clone();
        let source_dims = self.canvas(&mut exact);
        let (out_w, out_h) = plan_dimensions(source_dims.0, source_dims.1, &exact.preprocess);
        let cap = preview_max_dim.filter(|cap| out_w.max(out_h) > (*cap).max(1));
        let label = self.label();
        match &self.kind {
            SourceKind::Generator { spec, stops } => {
                let ratio = cap.map(|cap| cap as f32 / out_w.max(out_h) as f32);
                let (width, height, recipe) = match ratio {
                    Some(r) => (
                        ((out_w as f32 * r).round() as u32).max(1),
                        ((out_h as f32 * r).round() as u32).max(1),
                        preview_recipe(&exact, r),
                    ),
                    None => (source_dims.0, source_dims.1, exact),
                };
                let opts = GenerateOptions {
                    pattern: spec.pattern,
                    width,
                    height,
                    angle: spec.angle,
                    frequency: spec.frequency,
                    stops: stops.clone(),
                    seed: recipe.dither.seed,
                    ..GenerateOptions::default()
                };
                Prepared {
                    label,
                    source_dims,
                    image: generate_pattern(&opts),
                    recipe,
                    preview: ratio.is_some(),
                }
            }
            SourceKind::Image { image, .. } => {
                let Some(cap) = cap else {
                    return Prepared {
                        label,
                        source_dims,
                        image: image.clone(),
                        recipe: exact,
                        preview: false,
                    };
                };
                let p = &recipe.preprocess;
                let key = (
                    p.width,
                    p.height,
                    p.max_dim,
                    p.scale.map(f32::to_bits),
                    p.fit,
                    cap,
                );
                let proxy = match self.proxy.take() {
                    Some((cached_key, proxy)) if cached_key == key => proxy,
                    _ => {
                        let geometry = PreprocessOptions {
                            width: p.width,
                            height: p.height,
                            max_dim: Some(p.max_dim.map_or(cap, |m| m.min(cap))),
                            scale: p.scale,
                            fit: p.fit,
                            ..PreprocessOptions::default()
                        };
                        prepare_image(image, &geometry).working
                    }
                };
                let ratio = proxy.width().max(proxy.height()) as f32 / out_w.max(out_h) as f32;
                let prepared = Prepared {
                    label,
                    source_dims,
                    image: proxy.clone(),
                    recipe: preview_recipe(recipe, ratio),
                    preview: true,
                };
                self.proxy = Some((key, proxy));
                prepared
            }
        }
    }
}

/// Every accepted value of the enum-typed recipe fields, by their JSON (kebab-case) ids.
#[derive(Serialize)]
struct EnumValues {
    algorithms: Vec<String>,
    color_spaces: Vec<String>,
    fit_modes: Vec<String>,
    alpha_modes: Vec<String>,
    svg_shapes: Vec<String>,
    output_formats: Vec<String>,
    patterns: Vec<String>,
}

fn value_names<T: ValueEnum>() -> Vec<String> {
    T::value_variants()
        .iter()
        .filter_map(ValueEnum::to_possible_value)
        .map(|v| v.get_name().to_string())
        .collect()
}

/// The capabilities manifest plus the value lists of every enum-typed recipe field, as JSON.
/// The npm package generates its TypeScript types from this at build time.
#[must_use]
#[wasm_bindgen]
pub fn schema() -> String {
    #[derive(Serialize)]
    struct Schema {
        capabilities: crate::recipe::CapabilitiesManifest,
        enums: EnumValues,
    }
    let schema = Schema {
        capabilities: build_capabilities(),
        enums: EnumValues {
            algorithms: value_names::<crate::dither::Algorithm>(),
            color_spaces: value_names::<ColorSpace>(),
            fit_modes: value_names::<FitMode>(),
            alpha_modes: value_names::<crate::preprocess::AlphaMode>(),
            svg_shapes: value_names::<crate::output::SvgShape>(),
            output_formats: value_names::<OutputFormat>(),
            patterns: value_names::<Pattern>(),
        },
    };
    serde_json::to_string(&schema).unwrap_or_default()
}

/// The full `dith capabilities` manifest as JSON (algorithms, presets, palettes, generators, ...).
#[must_use]
#[wasm_bindgen]
pub fn capabilities() -> String {
    serde_json::to_string(&build_capabilities()).unwrap_or_default()
}

/// The resolved recipe JSON for a built-in preset. With `procedural`, photo preparation is
/// dropped exactly as `dith generate --preset` does.
///
/// # Errors
/// Throws `UNKNOWN_PRESET` for unknown names.
#[wasm_bindgen(js_name = presetRecipe)]
pub fn preset_recipe(name: &str, procedural: bool) -> Result<String, JsError> {
    let mut recipe = DitherRecipe::from_preset(name).map_err(|e| recipe_error(&e))?;
    if procedural {
        recipe.strip_photo_prep();
    }
    Ok(serde_json::to_string(&recipe).unwrap_or_default())
}

/// The default recipe JSON (`bw` palette, Floyd-Steinberg).
#[must_use]
#[wasm_bindgen(js_name = defaultRecipe)]
pub fn default_recipe() -> String {
    serde_json::to_string(&DitherRecipe::default()).unwrap_or_default()
}

/// Crate version.
#[must_use]
#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

//! The `dith mcp` command: a Model Context Protocol server over stdio.
//!
//! Speaks newline-delimited JSON-RPC 2.0 and serves both protocol eras. Legacy clients open with an
//! `initialize` handshake (revisions 2025-11-25 and earlier); modern clients (2026-07-28) carry the
//! protocol version in every request's `_meta` and may probe with `server/discover`. Tool schemas
//! are derived from the clap definitions, and tool calls parse their arguments with the same clap
//! parser and run the CLI handlers in-process under [`capture`], so results match the CLI exactly.

use std::any::TypeId;
use std::ffi::OsString;
use std::io::{self, BufRead, Write};
use std::sync::OnceLock;

use anyhow::Result;
use clap::{Arg, ArgAction, CommandFactory, Parser};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::cli::{Cli, Command};
use crate::output::{base64_encode, encode_fast_png_bytes};
use crate::recipe::{DitherRecipe, RecipeError, build_capabilities};

use super::animate::handle_animate;
use super::apply::handle_apply;
use super::compare::handle_compare;
use super::generate::handle_generate;
use super::inspect::handle_inspect;
use super::list::handle_list;
use super::shared::{PREVIEW_MAX_DIM, capture};

/// Stateless protocol revision, negotiated per request through `_meta`.
const MODERN_VERSION: &str = "2026-07-28";
/// Handshake-era revisions accepted by `initialize`, newest first.
const LEGACY_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const META_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;

/// Freshness hint for cacheable results: the tool list only changes with the binary.
const CACHE_TTL_MS: u64 = 3_600_000;

/// CLI flags left out of tool schemas: terminal-only display, writes outside the requested paths
/// (`--save-preset`), and `--recipe`, which tools expose as `recipe_file` plus an inline `recipe`.
const HIDDEN_FLAGS: [&str; 9] = [
    "view",
    "inline",
    "fullscreen",
    "view_mode",
    "view_cols",
    "view_rows",
    "save_preset",
    "preset_scope",
    "recipe",
];

/// MCP-only properties of the rendering tools, which take the shared recipe flags.
const RENDER_PROPERTIES: [&str; 3] = ["include_image", "recipe", "recipe_file"];

/// One MCP tool backed by a dith subcommand.
#[derive(Debug)]
struct Tool {
    name: &'static str,
    title: &'static str,
    subcommand: &'static str,
    description: &'static str,
    read_only: bool,
}

/// Every tool, in the deterministic order `tools/list` returns them.
const TOOLS: [Tool; 7] = [
    Tool {
        name: "dith_animate",
        title: "Animate dither",
        subcommand: "animate",
        description: "Create an animated dither (GIF, APNG, PNG spritesheet, or a folder of PNG frames) from a procedural loop (`pattern`, or a pattern name in `inputs`), an input GIF to re-dither, a frame sequence, or a still photo with Risograph film boil. Use for looping hero art, loaders, dissolve and fade reveals (`sweep_*`), palette cycling, and boiling print textures. `output` (.gif, .apng, or spritesheet .png) or `frames_dir` is required unless `dry_run` is set. Pair `temporal: \"locked\"` with ordered or blue-noise algorithms for flicker-free loops; `max_size` (e.g. \"2MB\") shrinks frames until the file fits. Returns the JSON animation report plus a PNG preview of the middle frame.",
        read_only: false,
    },
    Tool {
        name: "dith_apply",
        title: "Dither an image",
        subcommand: "apply",
        description: "Dither an existing image file into PNG, WebP, JPEG, GIF, BMP, TIFF, vector SVG, or Braille/ASCII text. Use when the user has a photo, logo, screenshot, or asset to restyle as 1-bit, duotone, Risograph, halftone, newsprint, Game Boy, PICO-8, and similar looks. Start from a `preset` (see dith_list kind=presets), then override `algorithm`, `palette`/`colors`/`auto_palette`, sizing (`max_dim`, `width`, `pixel_size`), and tone (`contrast`, `gamma`, `bias`); or pass a full `recipe` object (from `emit_recipe`) to replay exact settings. The format is inferred from the `output` extension unless `format` is set. Run dith_inspect first for tuned recommendations, and dith_compare to choose between looks. Returns the JSON render report plus a PNG preview of the result.",
        read_only: false,
    },
    Tool {
        name: "dith_capabilities",
        title: "dith capabilities manifest",
        subcommand: "capabilities",
        description: "Return dith's complete self-describing manifest: commands, algorithms, presets, palettes, generators, color spaces, output formats, parameter ranges, and the default recipe. Use once for zero-shot onboarding, or pass `section` to fetch a single part (dith_list is cheaper for filtered catalogs).",
        read_only: true,
    },
    Tool {
        name: "dith_compare",
        title: "Compare dither variants",
        subcommand: "compare",
        description: "Render a labeled contact sheet comparing several algorithms, presets, or palettes on one input image (or a procedural `pattern`), and/or write each variant into `out_dir`. Use before committing to a look: pass 3 to 9 candidates in `presets`, `algorithms`, or `palettes`, look at the returned sheet, then call dith_apply with the winner. `output` (sheet .png or .webp) or `out_dir` is required unless `dry_run` is set. Returns the JSON comparison report plus a PNG preview of the sheet.",
        read_only: false,
    },
    Tool {
        name: "dith_generate",
        title: "Generate procedural dither art",
        subcommand: "generate",
        description: "Procedurally generate artwork with no input image (gradients, lit 3D spheres, aurora meshes, waves, rings, fBm noise, and more; see dith_list kind=generators) and dither it in one step. Use for hero backgrounds, orbs, textures, and placeholder art. Size the canvas with `canvas_width`/`canvas_height`, set source colors with `stops`, and style it with `preset`, `algorithm`, and `palette`/`colors`. Returns the JSON render report plus a PNG preview of the result.",
        read_only: false,
    },
    Tool {
        name: "dith_inspect",
        title: "Inspect an image",
        subcommand: "inspect",
        description: "Analyze an image file (dimensions, luminance percentiles, dynamic range, transparency, dominant colors) and get ready-to-run dithering recommendations. Use first on any user-supplied image to pick presets, palettes, and tonal settings for dith_apply or dith_compare.",
        read_only: true,
    },
    Tool {
        name: "dith_list",
        title: "List dith catalogs",
        subcommand: "list",
        description: "List one catalog: algorithms, palettes, presets (built-in plus saved local presets), generators (procedural patterns), or output formats, optionally filtered by `tag` or `category`. Use to discover valid values for `preset`, `algorithm`, `palette`, and `pattern` before rendering.",
        read_only: true,
    },
];

/// Run the MCP server on stdin/stdout until the client closes stdin.
pub(crate) fn handle_mcp() -> Result<()> {
    let mut input = io::stdin().lock();
    let mut stdout = io::stdout().lock();
    let mut line = Vec::new();
    loop {
        line.clear();
        if input.read_until(b'\n', &mut line)? == 0 {
            return Ok(());
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let Some(response) = handle_message(&line) else {
            continue;
        };
        if writeln!(stdout, "{response}")
            .and_then(|()| stdout.flush())
            .is_err()
        {
            return Ok(());
        }
    }
}

/// Answer one JSON-RPC message; notifications and stray responses get no reply.
fn handle_message(raw: &[u8]) -> Option<String> {
    let Ok(message) = serde_json::from_slice::<Value>(raw) else {
        return Some(error_response(
            &Value::Null,
            PARSE_ERROR,
            "Parse error",
            None,
        ));
    };
    let Value::Object(message) = message else {
        return Some(error_response(
            &Value::Null,
            INVALID_REQUEST,
            "Invalid Request: expected one JSON-RPC object per line (batches are not supported)",
            None,
        ));
    };
    let id = message.get("id")?;
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        if message.contains_key("result") || message.contains_key("error") {
            return None;
        }
        return Some(error_response(
            id,
            INVALID_REQUEST,
            "Invalid Request: missing method",
            None,
        ));
    };
    let params = message.get("params").and_then(Value::as_object);
    let requested = params
        .and_then(|p| p.get("_meta"))
        .and_then(|meta| meta.get(META_PROTOCOL_VERSION))
        .and_then(Value::as_str);
    if let Some(version) = requested
        && version != MODERN_VERSION
        && !LEGACY_VERSIONS.contains(&version)
    {
        return Some(error_response(
            id,
            UNSUPPORTED_PROTOCOL_VERSION,
            "Unsupported protocol version",
            Some(json!({ "supported": supported_versions(), "requested": version })),
        ));
    }
    let modern = requested == Some(MODERN_VERSION);

    let mut result = match method {
        "initialize" => initialize(params),
        "ping" => json!({}),
        "server/discover" => json!({
            "supportedVersions": supported_versions(),
            "capabilities": { "tools": {} },
            "instructions": instructions(),
            "ttlMs": CACHE_TTL_MS,
            "cacheScope": "public",
        }),
        "tools/list" => return Some(tools_list_response(id, modern)),
        "tools/call" => match call_tool(params) {
            Ok(result) => result,
            Err(message) => return Some(error_response(id, INVALID_PARAMS, &message, None)),
        },
        _ => {
            return Some(error_response(
                id,
                METHOD_NOT_FOUND,
                &format!("Method not found: {method}"),
                None,
            ));
        }
    };
    if modern && let Value::Object(fields) = &mut result {
        fields.insert("resultType".to_string(), json!("complete"));
        fields.insert(
            "_meta".to_string(),
            json!({ META_SERVER_INFO: server_info() }),
        );
    }
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string())
}

fn error_response(id: &Value, code: i64, message: &str, data: Option<Value>) -> String {
    let mut error = json!({ "code": code, "message": message });
    if let Some(data) = data {
        error["data"] = data;
    }
    json!({ "jsonrpc": "2.0", "id": id, "error": error }).to_string()
}

fn supported_versions() -> Vec<&'static str> {
    std::iter::once(MODERN_VERSION)
        .chain(LEGACY_VERSIONS)
        .collect()
}

fn server_info() -> Value {
    json!({ "name": "dith", "title": "dith", "version": env!("CARGO_PKG_VERSION") })
}

fn instructions() -> String {
    let cwd = std::env::current_dir().map_or_else(|_| ".".into(), |dir| dir.display().to_string());
    format!(
        "dith crafts deterministic dithered images, vector SVGs, procedural art, and looping animations. \
         For a user image: dith_inspect it, dith_compare a few presets or algorithms, then dith_apply the winner. \
         With no input image use dith_generate (stills) or dith_animate (GIF/APNG loops). \
         dith_list and dith_capabilities return the valid presets, algorithms, palettes, and patterns. \
         Rendering tools return a JSON report plus a PNG preview (at most {PREVIEW_MAX_DIM}px); set include_image=false to skip it. \
         Relative paths resolve against the server's working directory: {cwd}"
    )
}

fn initialize(params: Option<&Map<String, Value>>) -> Value {
    let requested = params
        .and_then(|p| p.get("protocolVersion"))
        .and_then(Value::as_str);
    let version = requested
        .and_then(|v| LEGACY_VERSIONS.into_iter().find(|known| *known == v))
        .unwrap_or(LEGACY_VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": server_info(),
        "instructions": instructions(),
    })
}

/// The clap command tree, built once and shared by schema generation and argument lookup.
fn cli_command() -> &'static clap::Command {
    static COMMAND: OnceLock<clap::Command> = OnceLock::new();
    COMMAND.get_or_init(Cli::command)
}

/// Every tool definition serialized once as a JSON array, in [`TOOLS`] order.
fn tools_json() -> &'static str {
    static TOOLS_JSON: OnceLock<String> = OnceLock::new();
    TOOLS_JSON.get_or_init(|| {
        let tools: Vec<Value> = TOOLS
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "title": tool.title,
                    "description": tool.description,
                    "inputSchema": input_schema(tool),
                    "annotations": {
                        "title": tool.title,
                        "readOnlyHint": tool.read_only,
                        "destructiveHint": !tool.read_only,
                        "idempotentHint": true,
                        "openWorldHint": false,
                    },
                })
            })
            .collect();
        Value::Array(tools).to_string()
    })
}

fn tools_list_response(id: &Value, modern: bool) -> String {
    let tools = tools_json();
    if modern {
        let meta = json!({ META_SERVER_INFO: server_info() });
        format!(
            r#"{{"jsonrpc":"2.0","id":{id},"result":{{"tools":{tools},"ttlMs":{CACHE_TTL_MS},"cacheScope":"public","resultType":"complete","_meta":{meta}}}}}"#
        )
    } else {
        format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{"tools":{tools}}}}}"#)
    }
}

fn subcommand(tool: &Tool) -> &'static clap::Command {
    cli_command()
        .find_subcommand(tool.subcommand)
        .expect("every MCP tool maps to a dith subcommand")
}

/// Whether the tool renders through the shared recipe flags (and so returns a preview image).
fn renders(tool: &Tool) -> bool {
    subcommand(tool)
        .get_arguments()
        .any(|arg| arg.get_id() == "recipe")
}

/// The clap arguments a tool exposes as schema properties under their own ids.
fn tool_args(tool: &Tool) -> impl Iterator<Item = &'static Arg> {
    subcommand(tool).get_arguments().filter(|arg| {
        let id = arg.get_id().as_str();
        !arg.is_hide_set() && id != "help" && !HIDDEN_FLAGS.contains(&id)
    })
}

fn input_schema(tool: &Tool) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    if tool.subcommand == "capabilities" {
        properties.insert(
            "section".to_string(),
            json!({
                "type": "string",
                "enum": capabilities_sections(),
                "description": "Return only this top-level manifest section instead of the whole manifest.",
            }),
        );
    }
    for arg in tool_args(tool) {
        let id = arg.get_id().as_str();
        properties.insert(id.to_string(), Value::Object(arg_schema(arg)));
        if arg.is_required_set() {
            required.push(id);
        }
    }
    if renders(tool) {
        properties.insert(
            "include_image".to_string(),
            json!({
                "type": "boolean",
                "default": true,
                "description": format!("Return a PNG preview of the result (box-averaged by a whole factor to at most {PREVIEW_MAX_DIM}px) alongside the JSON report."),
            }),
        );
        properties.insert(
            "recipe".to_string(),
            json!({
                "type": "object",
                "description": "Full dith recipe object to start from (the `recipe` field of a report made with emit_recipe=true); `preset` and explicit options override it. Omitted keys keep their defaults.",
                "properties": {
                    "preset": { "type": ["string", "null"] },
                    "palette": {
                        "type": "object",
                        "description": "{\"mode\": \"builtin\", \"value\": \"gameboy\"}, {\"mode\": \"colors\", \"value\": [\"#111111\", \"#f5efe6\"]}, {\"mode\": \"file\", \"value\": \"brand.gpl\"}, or {\"mode\": \"adaptive\", \"value\": 6}.",
                        "properties": {
                            "mode": { "type": "string", "enum": ["builtin", "colors", "file", "adaptive"] },
                            "value": {},
                        },
                        "required": ["mode", "value"],
                    },
                    "dither": { "type": "object", "description": "algorithm, color_space, strength, serpentine, bias, noise, pattern_scale, seed." },
                    "preprocess": { "type": "object", "description": "Resize, tone, and alpha settings (width, height, max_dim, pixel_size, contrast, gamma, ...)." },
                    "export": { "type": "object", "description": "format, svg_shape, svg_skip_bg, text_width, optimize." },
                },
            }),
        );
        properties.insert(
            "recipe_file".to_string(),
            json!({
                "type": "string",
                "description": "Path to a JSON recipe file to start from (alternative to the inline `recipe` object).",
            }),
        );
    }
    let mut schema = json!({
        "type": "object",
        "properties": properties,
        "additionalProperties": false,
    });
    if !required.is_empty() {
        schema["required"] = json!(required);
    }
    schema
}

/// JSON Schema for one clap argument: type, enum, default, and description from its help text.
fn arg_schema(arg: &Arg) -> Map<String, Value> {
    let mut schema = Map::new();
    if matches!(arg.get_action(), ArgAction::SetTrue) {
        schema.insert("type".to_string(), json!("boolean"));
    } else {
        let kind = scalar_kind(arg);
        let values: Vec<String> = arg
            .get_possible_values()
            .iter()
            .filter(|value| !value.is_hide_set())
            .map(|value| value.get_name().to_string())
            .collect();
        let mut item = Map::new();
        item.insert("type".to_string(), json!(kind.json_type()));
        if !values.is_empty() {
            item.insert("enum".to_string(), json!(values));
        } else if kind == ScalarKind::Unsigned {
            item.insert("minimum".to_string(), json!(0));
        }
        if matches!(arg.get_action(), ArgAction::Append) {
            schema.insert("type".to_string(), json!("array"));
            schema.insert("items".to_string(), Value::Object(item));
        } else {
            schema = item;
            if let [default] = arg.get_default_values() {
                let text = default.to_string_lossy();
                let value = match kind {
                    ScalarKind::Unsigned | ScalarKind::Integer => text
                        .parse::<i64>()
                        .map_or_else(|_| json!(text), Value::from),
                    ScalarKind::Number => text
                        .parse::<f64>()
                        .map_or_else(|_| json!(text), Value::from),
                    ScalarKind::String => json!(text),
                };
                schema.insert("default".to_string(), value);
            }
        }
    }
    if let Some(help) = arg.get_long_help().or_else(|| arg.get_help()) {
        let help = help.to_string();
        let description = match arg.get_id().as_str() {
            "input" if help.contains("'-'") => {
                "Input image path, relative to the server's working directory.".to_string()
            }
            "output" if help.contains("'-'") => {
                "Output file path, relative to the server's working directory; the format is inferred from the extension unless `format` is set. Optional only for braille/ascii text formats, whose text is returned in the report.".to_string()
            }
            _ => help.replace('\n', " "),
        };
        schema.insert("description".to_string(), json!(description));
    }
    schema
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScalarKind {
    Unsigned,
    Integer,
    Number,
    String,
}

impl ScalarKind {
    const fn json_type(self) -> &'static str {
        match self {
            Self::Unsigned | Self::Integer => "integer",
            Self::Number => "number",
            Self::String => "string",
        }
    }
}

fn scalar_kind(arg: &Arg) -> ScalarKind {
    // `--max-size` parses human sizes such as "2MB" into a byte count.
    if arg.get_id() == "max_size" {
        return ScalarKind::String;
    }
    let parsed = arg.get_value_parser().type_id();
    let any_of = |types: &[TypeId]| types.iter().any(|ty| parsed == *ty);
    if any_of(&[
        TypeId::of::<u8>(),
        TypeId::of::<u16>(),
        TypeId::of::<u32>(),
        TypeId::of::<u64>(),
        TypeId::of::<usize>(),
    ]) {
        ScalarKind::Unsigned
    } else if any_of(&[TypeId::of::<i32>(), TypeId::of::<i64>()]) {
        ScalarKind::Integer
    } else if any_of(&[TypeId::of::<f32>(), TypeId::of::<f64>()]) {
        ScalarKind::Number
    } else {
        ScalarKind::String
    }
}

/// The capabilities manifest, built once per server process.
fn capabilities_manifest() -> &'static Value {
    static MANIFEST: OnceLock<Value> = OnceLock::new();
    MANIFEST.get_or_init(|| serde_json::to_value(build_capabilities()).unwrap_or_default())
}

fn capabilities_sections() -> Vec<String> {
    capabilities_manifest()
        .as_object()
        .map_or_else(Vec::new, |fields| fields.keys().cloned().collect())
}

/// A failed tool call, reported as an `isError` result carrying the CLI's JSON error envelope.
#[derive(Debug)]
struct ToolError {
    code: &'static str,
    message: String,
    suggestion: String,
    valid_values: Option<Vec<String>>,
}

impl ToolError {
    fn invalid(message: String, valid_values: Option<Vec<String>>) -> Self {
        Self {
            code: "INVALID_ARGUMENT",
            message,
            suggestion: "Match the tool's inputSchema from tools/list; property names are the snake_case form of dith's CLI flags (--max-dim is max_dim).".to_string(),
            valid_values,
        }
    }

    fn from_recipe(err: &RecipeError) -> Self {
        Self {
            code: err.code(),
            message: err.to_string(),
            suggestion: err.suggestion(),
            valid_values: err.valid_values(),
        }
    }

    fn from_anyhow(err: &anyhow::Error) -> Self {
        if let Some(recipe_err) = err.downcast_ref::<RecipeError>() {
            return Self::from_recipe(recipe_err);
        }
        Self {
            code: "RUNTIME_ERROR",
            message: format!("{err:#}"),
            suggestion: "Check that input paths exist relative to the server's working directory and outputs are writable, or retry with dry_run=true.".to_string(),
            valid_values: None,
        }
    }

    fn into_result(self) -> Value {
        let mut envelope = json!({
            "status": "error",
            "code": self.code,
            "message": self.message,
            "suggestion": self.suggestion,
        });
        if let Some(values) = self.valid_values {
            envelope["valid_values"] = json!(values);
        }
        json!({
            "content": [{ "type": "text", "text": envelope.to_string() }],
            "isError": true,
        })
    }
}

/// Run `tools/call`; `Err` is a protocol error (unknown tool or malformed params).
fn call_tool(params: Option<&Map<String, Value>>) -> Result<Value, String> {
    let name = params
        .and_then(|p| p.get("name"))
        .and_then(Value::as_str)
        .ok_or("Invalid params: tools/call needs a string `name`")?;
    let tool = TOOLS
        .iter()
        .find(|tool| tool.name == name)
        .ok_or_else(|| format!("Unknown tool: {name}"))?;
    let empty = Map::new();
    let arguments = match params.and_then(|p| p.get("arguments")) {
        None | Some(Value::Null) => &empty,
        Some(Value::Object(arguments)) => arguments,
        Some(_) => return Err("Invalid params: `arguments` must be an object".to_string()),
    };
    Ok(run_tool(tool, arguments).unwrap_or_else(ToolError::into_result))
}

fn run_tool(tool: &Tool, arguments: &Map<String, Value>) -> Result<Value, ToolError> {
    if tool.subcommand == "capabilities" {
        return capabilities_result(arguments);
    }
    let call = parse_call(tool, arguments)?;
    let Cli { command, .. } = Cli::try_parse_from(&call.argv)
        .map_err(|err| ToolError::invalid(clap_message(&err), None))?;
    let inline_recipe = call.inline_recipe;
    let (outcome, captured) = match command {
        Command::Apply(mut args) => {
            args.flags.inline_recipe = inline_recipe;
            capture(|| handle_apply(&args, true))
        }
        Command::Generate(mut args) => {
            args.flags.inline_recipe = inline_recipe;
            capture(|| handle_generate(&args, true))
        }
        Command::Animate(mut args) => {
            args.flags.inline_recipe = inline_recipe;
            capture(|| handle_animate(&args, true))
        }
        Command::Compare(mut args) => {
            args.flags.inline_recipe = inline_recipe;
            capture(|| handle_compare(&args, true))
        }
        Command::Inspect(args) => capture(|| handle_inspect(&args, true)),
        Command::List(args) => capture(|| handle_list(&args, true)),
        _ => unreachable!("MCP tools only parse their own subcommand"),
    };
    outcome.map_err(|err| ToolError::from_anyhow(&err))?;

    let mut content = vec![json!({
        "type": "text",
        "text": captured.report.unwrap_or_else(|| r#"{"status":"ok"}"#.to_string()),
    })];
    if call.include_image
        && let Some(preview) = captured.preview
    {
        let png = encode_fast_png_bytes(&preview).map_err(|err| ToolError::from_anyhow(&err))?;
        content.push(json!({
            "type": "image",
            "data": base64_encode(&png),
            "mimeType": "image/png",
        }));
    }
    Ok(json!({ "content": content, "isError": false }))
}

fn capabilities_result(arguments: &Map<String, Value>) -> Result<Value, ToolError> {
    static FULL_TEXT: OnceLock<String> = OnceLock::new();
    if let Some(unknown) = arguments.keys().find(|key| *key != "section") {
        return Err(ToolError::invalid(
            format!("unknown argument '{unknown}' for dith_capabilities"),
            Some(vec!["section".to_string()]),
        ));
    }
    let manifest = capabilities_manifest();
    let text = match arguments.get("section") {
        None | Some(Value::Null) => FULL_TEXT.get_or_init(|| manifest.to_string()).clone(),
        Some(Value::String(section)) if manifest.get(section).is_some() => {
            manifest[section.as_str()].to_string()
        }
        Some(other) => {
            return Err(ToolError::invalid(
                format!("unknown capabilities section {other}"),
                Some(capabilities_sections()),
            ));
        }
    };
    Ok(json!({ "content": [{ "type": "text", "text": text }], "isError": false }))
}

/// Tool arguments translated into a clap argv plus the MCP-only options.
#[derive(Debug)]
struct ParsedCall {
    argv: Vec<OsString>,
    inline_recipe: Option<DitherRecipe>,
    include_image: bool,
}

fn parse_call(tool: &Tool, arguments: &Map<String, Value>) -> Result<ParsedCall, ToolError> {
    let has_recipe = renders(tool);
    let mut call = ParsedCall {
        argv: vec!["dith".into(), tool.subcommand.into()],
        inline_recipe: None,
        include_image: true,
    };
    let mut positionals: Vec<(usize, Vec<String>)> = Vec::new();
    for (key, value) in arguments {
        if value.is_null() {
            continue;
        }
        if has_recipe && key == "recipe" {
            if !value.is_object() {
                return Err(ToolError::invalid(
                    "'recipe' must be a JSON object (use 'recipe_file' for a path)".to_string(),
                    None,
                ));
            }
            let recipe = DitherRecipe::deserialize(value).map_err(|err| {
                ToolError::from_recipe(&RecipeError::InvalidRecipeJson(err.to_string()))
            })?;
            call.inline_recipe = Some(recipe);
            continue;
        }
        if has_recipe && key == "include_image" {
            call.include_image = value.as_bool().ok_or_else(|| {
                ToolError::invalid("'include_image' must be a boolean".to_string(), None)
            })?;
            continue;
        }
        let arg = if has_recipe && key == "recipe_file" {
            subcommand(tool)
                .get_arguments()
                .find(|arg| arg.get_id() == "recipe")
        } else {
            tool_args(tool).find(|arg| arg.get_id() == key.as_str())
        }
        .ok_or_else(|| {
            ToolError::invalid(
                format!("unknown argument '{key}' for {}", tool.name),
                Some(property_names(tool)),
            )
        })?;
        if matches!(arg.get_action(), ArgAction::SetTrue) {
            match value {
                Value::Bool(true) => call.argv.push(format!("--{}", long_name(arg)).into()),
                Value::Bool(false) => {}
                _ => {
                    return Err(ToolError::invalid(
                        format!("'{key}' must be a boolean"),
                        None,
                    ));
                }
            }
            continue;
        }
        let values = match value {
            Value::Array(items) if matches!(arg.get_action(), ArgAction::Append) => items
                .iter()
                .map(|item| scalar_text(key, item))
                .collect::<Result<Vec<_>, _>>()?,
            other => vec![scalar_text(key, other)?],
        };
        if arg.is_positional() {
            positionals.push((arg.get_index().unwrap_or(usize::MAX), values));
        } else {
            let long = long_name(arg);
            call.argv.extend(
                values
                    .iter()
                    .map(|v| OsString::from(format!("--{long}={v}"))),
            );
        }
    }
    if !positionals.is_empty() {
        positionals.sort_by_key(|(index, _)| *index);
        call.argv.push("--".into());
        call.argv.extend(
            positionals
                .into_iter()
                .flat_map(|(_, values)| values)
                .map(OsString::from),
        );
    }
    Ok(call)
}

fn long_name(arg: &Arg) -> &str {
    arg.get_long().unwrap_or_else(|| arg.get_id().as_str())
}

fn property_names(tool: &Tool) -> Vec<String> {
    let mut names: Vec<String> = tool_args(tool)
        .map(|arg| arg.get_id().to_string())
        .collect();
    if renders(tool) {
        names.extend(RENDER_PROPERTIES.map(String::from));
    }
    names
}

/// Render a JSON scalar as a CLI value; `-` (stdin/stdout) is refused because stdio carries MCP.
fn scalar_text(key: &str, value: &Value) -> Result<String, ToolError> {
    match value {
        Value::String(text) if text == "-" => Err(ToolError::invalid(
            format!(
                "'{key}' cannot be '-': stdin and stdout carry the MCP protocol, so pass a file path"
            ),
            None,
        )),
        Value::String(text) => Ok(text.clone()),
        Value::Number(number) => Ok(number.to_string()),
        _ => Err(ToolError::invalid(
            format!("'{key}' must be a string or number"),
            None,
        )),
    }
}

/// Clap's error without the usage footer, which names CLI flags rather than tool properties.
fn clap_message(err: &clap::Error) -> String {
    err.to_string()
        .lines()
        .take_while(|line| !line.starts_with("Usage:") && !line.starts_with("For more information"))
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .trim_start_matches("error: ")
        .to_string()
}

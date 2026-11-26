//! End-to-end integration tests for the `dith` CLI binary.

use std::process::Command;

use image::AnimationDecoder;
use serde_json::Value;
use tempfile::TempDir;

fn dith_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_dith"))
}

#[test]
fn capabilities_outputs_valid_self_describing_json() {
    let out = dith_bin().arg("capabilities").output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["tool"], "dith");
    assert!(json["algorithms"].as_array().unwrap().len() >= 20);
    assert!(json["presets"].as_array().unwrap().len() >= 10);
    assert!(json["palettes"].as_array().unwrap().len() >= 20);
    assert!(json["generators"].as_array().unwrap().len() >= 8);
}

#[test]
fn generate_apply_inspect_compare_and_batch_work_end_to_end() {
    let dir = TempDir::new().unwrap();
    let orb_png = dir.path().join("orb.png");
    let orb_svg = dir.path().join("orb.svg");
    let recipe_path = dir.path().join("recipe.json");
    let applied_png = dir.path().join("applied.png");
    let sheet_png = dir.path().join("sheet.png");
    let batch_dir = dir.path().join("batch_out");

    // 1. Generate a dithered 3D sphere with preset + JSON output + emit-recipe.
    let gen_out = dith_bin()
        .args([
            "generate",
            "sphere",
            "-o",
            orb_png.to_str().unwrap(),
            "--preset",
            "risograph",
            "--canvas-width",
            "160",
            "--canvas-height",
            "120",
            "--emit-recipe",
            "--preview",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        gen_out.status.success(),
        "generate failed: {}",
        String::from_utf8_lossy(&gen_out.stderr)
    );
    let gen_json: Value = serde_json::from_slice(&gen_out.stdout).unwrap();
    assert_eq!(gen_json["status"], "ok");
    assert_eq!(gen_json["output_dimensions"], serde_json::json!([160, 120]));
    assert!(gen_json["preview_braille"].as_str().unwrap().len() > 10);
    assert!(orb_png.exists());

    // Save emitted recipe JSON to replay it on `apply`.
    std::fs::write(
        &recipe_path,
        serde_json::to_string_pretty(&gen_json["recipe"]).unwrap(),
    )
    .unwrap();

    // 2. Inspect generated image and confirm structured diagnostics + recommendations.
    let insp = dith_bin()
        .args(["inspect", orb_png.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert!(insp.status.success());
    let insp_json: Value = serde_json::from_slice(&insp.stdout).unwrap();
    assert_eq!(insp_json["width"], 160);
    assert_eq!(insp_json["height"], 120);
    assert_ne!(
        insp_json["recommendations"].as_array().unwrap().len(),
        0,
        "expected recommendations"
    );

    // 3. Apply saved recipe + override palette with custom hex colors + export SVG dots.
    let app_svg = dith_bin()
        .args([
            "apply",
            orb_png.to_str().unwrap(),
            "-o",
            orb_svg.to_str().unwrap(),
            "--recipe",
            recipe_path.to_str().unwrap(),
            "--colors",
            "#111111,#f5efe6,#d93829",
            "--max-dim",
            "64",
            "--svg-shape",
            "circle",
            "--svg-skip-bg",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        app_svg.status.success(),
        "apply svg failed: {}",
        String::from_utf8_lossy(&app_svg.stderr)
    );
    let svg_text = std::fs::read_to_string(&orb_svg).unwrap();
    assert!(svg_text.starts_with("<svg "));
    assert!(svg_text.contains("a.48.48 0 1 0 .96 0 .48.48 0 1 0-.96 0"));

    // 4. Dry-run apply validates and plans without writing.
    let dry = dith_bin()
        .args([
            "apply",
            orb_png.to_str().unwrap(),
            "-o",
            applied_png.to_str().unwrap(),
            "--preset",
            "gameboy",
            "--dry-run",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(dry.status.success());
    let dry_json: Value = serde_json::from_slice(&dry.stdout).unwrap();
    assert_eq!(dry_json["dry_run"], true);
    assert!(!applied_png.exists());

    // 5. Compare contact sheet across multiple presets.
    let cmp = dith_bin()
        .args([
            "compare",
            orb_png.to_str().unwrap(),
            "-o",
            sheet_png.to_str().unwrap(),
            "--presets",
            "mac-classic,editorial-print,gameboy,film-stipple",
            "--tile-max-dim",
            "100",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        cmp.status.success(),
        "compare failed: {}",
        String::from_utf8_lossy(&cmp.stderr)
    );
    let cmp_json: Value = serde_json::from_slice(&cmp.stdout).unwrap();
    assert_eq!(cmp_json["variants"].as_array().unwrap().len(), 4);
    assert!(sheet_png.exists());

    // 6. Batch process multiple files into out-dir.
    let batch = dith_bin()
        .args([
            "batch",
            orb_png.to_str().unwrap(),
            sheet_png.to_str().unwrap(),
            "--out-dir",
            batch_dir.to_str().unwrap(),
            "--preset",
            "blueprint",
            "--max-dim",
            "80",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        batch.status.success(),
        "batch failed: {}",
        String::from_utf8_lossy(&batch.stderr)
    );
    let batch_json: Value = serde_json::from_slice(&batch.stdout).unwrap();
    assert_eq!(batch_json["succeeded"], 2);
    assert_eq!(batch_json["failed"], 0);
}

#[test]
fn invalid_palette_returns_structured_json_error_with_valid_values() {
    let out = dith_bin()
        .args([
            "generate",
            "linear-gradient",
            "--palette",
            "nonexistent-palette",
            "--format",
            "braille",
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err_json: Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(err_json["status"], "error");
    assert_eq!(err_json["code"], "UNKNOWN_PALETTE");
    assert!(err_json["valid_values"].as_array().unwrap().len() >= 15);
}

#[test]
fn braille_text_output_works_without_output_file() {
    let out = dith_bin()
        .args([
            "generate",
            "radial-gradient",
            "--canvas-width",
            "40",
            "--canvas-height",
            "32",
            "--algorithm",
            "bayer4",
            "--format",
            "braille",
            "--text-width",
            "20",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.lines().count() >= 4);
}

#[test]
fn preview_and_view_flag_emit_valid_kitty_graphics_protocol() {
    // 1. Dedicated `preview` subcommand defaults to Kitty graphics protocol.
    let prev = dith_bin()
        .args([
            "preview",
            "--pattern",
            "sphere",
            "--preset",
            "risograph",
            "--canvas-width",
            "64",
            "--canvas-height",
            "48",
            "--cols",
            "36",
        ])
        .output()
        .unwrap();
    assert!(
        prev.status.success(),
        "preview failed: {}",
        String::from_utf8_lossy(&prev.stderr)
    );
    let seq = String::from_utf8(prev.stdout).unwrap();
    assert!(seq.starts_with("\x1b_Ga=T,f=100,t=d,q=2,c=36,m="));
    assert!(seq.ends_with("\x1b\\\n"));

    // 2. `generate --view` works without `-o` and renders via Kitty graphics protocol.
    let view_gen = dith_bin()
        .args([
            "generate",
            "linear-gradient",
            "--canvas-width",
            "48",
            "--canvas-height",
            "32",
            "--preset",
            "mac-classic",
            "--view",
        ])
        .output()
        .unwrap();
    assert!(view_gen.status.success());
    let gen_seq = String::from_utf8(view_gen.stdout).unwrap();
    assert!(gen_seq.starts_with("\x1b_Ga=T,f=100,t=d,q=2,m="));

    // 3. `compare --view` works without `-o` and renders contact sheet via Kitty graphics protocol.
    let view_cmp = dith_bin()
        .args([
            "compare",
            "--pattern",
            "sphere",
            "--presets",
            "mac-classic,gameboy",
            "--tile-max-dim",
            "64",
            "--view",
        ])
        .output()
        .unwrap();
    assert!(view_cmp.status.success());
    let cmp_seq = String::from_utf8(view_cmp.stdout).unwrap();
    assert!(cmp_seq.starts_with("\x1b_Ga=T,f=100,t=d,q=2,m="));

    // 4. `preview --json` reports fullscreen=true by default and never hangs when non-TTY.
    let prev_json = dith_bin()
        .args([
            "preview",
            "--pattern",
            "rings",
            "--canvas-width",
            "48",
            "--canvas-height",
            "48",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(prev_json.status.success());
    let parsed: Value = serde_json::from_slice(&prev_json.stdout).unwrap();
    assert_eq!(parsed["status"], "ok");
    assert_eq!(parsed["mode"], "kitty");
    assert_eq!(parsed["fullscreen"], true);
    assert!(
        parsed["preview_payload"]
            .as_str()
            .unwrap()
            .starts_with("\x1b_Ga=T,f=100,t=d,q=2,m=")
    );

    // 5. `preview --inline --json` switches fullscreen off cleanly.
    let inline_json = dith_bin()
        .args([
            "preview",
            "--pattern",
            "rings",
            "--canvas-width",
            "32",
            "--canvas-height",
            "32",
            "--inline",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(inline_json.status.success());
    let parsed_inline: Value = serde_json::from_slice(&inline_json.stdout).unwrap();
    assert_eq!(parsed_inline["fullscreen"], false);
}

#[test]
fn agents_can_save_list_show_apply_and_delete_local_presets() {
    let workspace = TempDir::new().unwrap();
    let presets_dir = workspace.path().join(".dith/presets");
    let out_png = workspace.path().join("branded.png");
    let out_inline = workspace.path().join("inline-saved.png");

    // 1. Save a custom local preset via `dith preset save`.
    let save = dith_bin()
        .current_dir(workspace.path())
        .args([
            "preset",
            "save",
            "brand-hero",
            "--algorithm",
            "blue-noise",
            "--colors",
            "#0f172a,#2563eb,#f5efe6",
            "--contrast",
            "0.12",
            "--sharpen",
            "0.3",
            "--auto-levels",
            "--description",
            "Cobalt stipple hero look",
            "--tags",
            "brand,hero,web",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        save.status.success(),
        "preset save failed: {}",
        String::from_utf8_lossy(&save.stderr)
    );
    let save_json: Value = serde_json::from_slice(&save.stdout).unwrap();
    assert_eq!(save_json["status"], "ok");
    assert_eq!(save_json["id"], "brand-hero");
    assert_eq!(save_json["source"], "project");
    assert!(presets_dir.join("brand-hero.json").exists());

    // 2. Local preset shows up in `dith preset list --source local` AND `dith list presets`.
    let list_local = dith_bin()
        .current_dir(workspace.path())
        .args(["preset", "list", "--source", "local", "--json"])
        .output()
        .unwrap();
    assert!(list_local.status.success());
    let local_items: Value = serde_json::from_slice(&list_local.stdout).unwrap();
    assert!(
        local_items
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == "brand-hero" && p["source"] == "project")
    );

    // 3. `preset show` returns full definition.
    let show = dith_bin()
        .current_dir(workspace.path())
        .args(["preset", "show", "brand-hero", "--json"])
        .output()
        .unwrap();
    assert!(show.status.success());
    let show_json: Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(show_json["algorithm"], "blue-noise");
    assert_eq!(show_json["source"], "project");

    // 4. Use the saved local preset in `generate --preset brand-hero` (and also test `--save-preset`).
    let gen_out = dith_bin()
        .current_dir(workspace.path())
        .args([
            "generate",
            "sphere",
            "-o",
            out_png.to_str().unwrap(),
            "--preset",
            "brand-hero",
            "--canvas-width",
            "80",
            "--canvas-height",
            "60",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        gen_out.status.success(),
        "generate with local preset failed: {}",
        String::from_utf8_lossy(&gen_out.stderr)
    );
    let gen_json: Value = serde_json::from_slice(&gen_out.stdout).unwrap();
    assert_eq!(gen_json["algorithm"], "blue-noise");
    assert_eq!(
        gen_json["palette_colors"],
        serde_json::json!(["#0f172a", "#2563eb", "#f5efe6"])
    );
    assert!(out_png.exists());

    // 5. Save a preset directly during `generate --save-preset quick-look`.
    let quick = dith_bin()
        .current_dir(workspace.path())
        .args([
            "generate",
            "waves",
            "-o",
            out_inline.to_str().unwrap(),
            "--algorithm",
            "atkinson",
            "--palette",
            "blueprint",
            "--canvas-width",
            "64",
            "--canvas-height",
            "48",
            "--save-preset",
            "quick-look",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(quick.status.success());
    let quick_json: Value = serde_json::from_slice(&quick.stdout).unwrap();
    assert!(
        quick_json["saved_preset_path"]
            .as_str()
            .unwrap()
            .contains("quick-look.json")
    );
    assert!(presets_dir.join("quick-look.json").exists());

    // 6. Delete the saved local preset cleanly.
    let del = dith_bin()
        .current_dir(workspace.path())
        .args(["preset", "delete", "brand-hero", "--json"])
        .output()
        .unwrap();
    assert!(del.status.success());
    assert!(!presets_dir.join("brand-hero.json").exists());
}

#[test]
fn animated_dithers_support_procedural_gif_apng_spritesheet_and_photo_boil() {
    let dir = TempDir::new().unwrap();
    let gif_path = dir.path().join("orb.gif");
    let apng_path = dir.path().join("waves.apng");
    let sheet_path = dir.path().join("rings-sheet.png");
    let frames_dir = dir.path().join("frames");
    let redithered_gif = dir.path().join("redithered.gif");
    let still_png = dir.path().join("still.png");
    let boiled_gif = dir.path().join("boiled.gif");

    // 1. Procedural seamless looping GIF with exact Game Boy palette.
    let anim_gif = dith_bin()
        .args([
            "animate",
            "sphere",
            "-o",
            gif_path.to_str().unwrap(),
            "--preset",
            "gameboy",
            "--canvas-width",
            "64",
            "--canvas-height",
            "48",
            "--frames",
            "8",
            "--fps",
            "16",
            "--temporal",
            "locked",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        anim_gif.status.success(),
        "animate gif failed: {}",
        String::from_utf8_lossy(&anim_gif.stderr)
    );
    let gif_json: Value = serde_json::from_slice(&anim_gif.stdout).unwrap();
    assert_eq!(gif_json["status"], "ok");
    assert_eq!(gif_json["frame_count"], 8);
    assert_eq!(gif_json["anim_format"], "gif");
    assert_eq!(gif_json["temporal"], "locked");
    let gif_bytes = std::fs::read(&gif_path).unwrap();
    assert!(gif_bytes.starts_with(b"GIF89a"));

    // 2. Re-dither an existing animated GIF with shared adaptive palette + ping-pong!
    let re_gif = dith_bin()
        .args([
            "animate",
            gif_path.to_str().unwrap(),
            "-o",
            redithered_gif.to_str().unwrap(),
            "--auto-palette",
            "4",
            "--algorithm",
            "blue-noise",
            "--ping-pong",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        re_gif.status.success(),
        "re-dither gif failed: {}",
        String::from_utf8_lossy(&re_gif.stderr)
    );
    let re_json: Value = serde_json::from_slice(&re_gif.stdout).unwrap();
    assert_eq!(re_json["frame_count"], 14); // 8 + 6 interior reverse frames

    // 3. Procedural APNG + frames directory export.
    let anim_apng = dith_bin()
        .args([
            "animate",
            "waves",
            "-o",
            apng_path.to_str().unwrap(),
            "--frames-dir",
            frames_dir.to_str().unwrap(),
            "--preset",
            "risograph",
            "--canvas-width",
            "48",
            "--canvas-height",
            "36",
            "--frames",
            "6",
            "--fps",
            "12",
            "--temporal",
            "boil",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        anim_apng.status.success(),
        "animate apng failed: {}",
        String::from_utf8_lossy(&anim_apng.stderr)
    );
    let apng_bytes = std::fs::read(&apng_path).unwrap();
    assert!(apng_bytes.windows(4).any(|w| w == b"acTL"));
    assert!(frames_dir.join("frame_0001.png").exists());
    assert!(frames_dir.join("frame_0006.png").exists());

    // 4. Spritesheet export.
    let anim_sheet = dith_bin()
        .args([
            "animate",
            "rings",
            "-o",
            sheet_path.to_str().unwrap(),
            "--anim-format",
            "spritesheet",
            "--sheet-cols",
            "3",
            "--canvas-width",
            "32",
            "--canvas-height",
            "32",
            "--frames",
            "6",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(anim_sheet.status.success());
    let sheet_json: Value = serde_json::from_slice(&anim_sheet.stdout).unwrap();
    assert_eq!(sheet_json["spritesheet_grid"], serde_json::json!([3, 2]));
    assert_eq!(image::image_dimensions(&sheet_path).unwrap(), (96, 64));

    // 5. Static photo -> animated Risograph boil GIF (defaults temporal to boil!).
    let gen_still = dith_bin()
        .args([
            "generate",
            "mesh",
            "-o",
            still_png.to_str().unwrap(),
            "--canvas-width",
            "48",
            "--canvas-height",
            "48",
        ])
        .output()
        .unwrap();
    assert!(gen_still.status.success());

    let boil = dith_bin()
        .args([
            "animate",
            still_png.to_str().unwrap(),
            "-o",
            boiled_gif.to_str().unwrap(),
            "--preset",
            "risograph",
            "--frames",
            "5",
            "--fps",
            "10",
            "--boil-rate",
            "2",
            "--boil-amount",
            "0.07",
            "--hold-end",
            "2",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(boil.status.success());
    let boil_json: Value = serde_json::from_slice(&boil.stdout).unwrap();
    assert_eq!(boil_json["temporal"], "boil");
    assert_eq!(boil_json["frame_count"], 7); // 5 + 2 hold_end
    assert!(boiled_gif.exists());

    // 6. Ink-wash bias dissolve + mosaic pixel-size reveal + palette cycling + duration!
    let custom_gif = dir.path().join("custom-sweep.gif");
    let custom = dith_bin()
        .args([
            "animate",
            "plasma",
            "-o",
            custom_gif.to_str().unwrap(),
            "--palette",
            "sunset-dusk",
            "--algorithm",
            "bayer4",
            "--canvas-width",
            "48",
            "--canvas-height",
            "32",
            "--duration",
            "0.5",
            "--fps",
            "12",
            "--easing",
            "ease-in-out",
            "--sweep-mode",
            "pulse",
            "--sweep-bias",
            "-0.3..0.3",
            "--sweep-pixel-size",
            "6..1",
            "--screen-drift",
            "1,-1",
            "--cycle-palette",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        custom.status.success(),
        "custom sweep animate failed: {}",
        String::from_utf8_lossy(&custom.stderr)
    );
    let custom_json: Value = serde_json::from_slice(&custom.stdout).unwrap();
    assert_eq!(custom_json["frame_count"], 6); // 0.5s * 12fps
    assert!(custom_gif.exists());
}

fn run_json(args: &[&str]) -> (bool, Value, Value) {
    let out = dith_bin().args(args).output().unwrap();
    let stdout = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    let stderr = serde_json::from_slice(&out.stderr).unwrap_or(Value::Null);
    (out.status.success(), stdout, stderr)
}

#[test]
fn catalogs_describe_generator_motion_and_algorithm_temporal_stability() {
    let (ok, gens, _) = run_json(&["list", "generators", "--json"]);
    assert!(ok);
    for g in gens.as_array().unwrap() {
        assert!(
            g["motion"].as_str().is_some_and(|m| !m.is_empty()),
            "generator {} lacks a motion description",
            g["id"]
        );
    }

    let (ok, algos, _) = run_json(&["list", "algorithms", "--json"]);
    assert!(ok);
    let stable = |id: &str| {
        algos
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == id)
            .unwrap()["temporally_stable"]
            .as_bool()
            .unwrap()
    };
    assert!(stable("bayer4"));
    assert!(stable("blue-noise"));
    assert!(!stable("atkinson"));
    assert!(!stable("riemersma"));
}

#[test]
fn animate_max_size_shrinks_frames_until_the_file_fits() {
    let dir = TempDir::new().unwrap();
    let out = dir.path().join("fit.gif");
    let out_s = out.to_str().unwrap();
    let base = [
        "animate",
        "sphere",
        "-o",
        out_s,
        "--algorithm",
        "blue-noise",
        "--temporal",
        "grain",
        "--frames",
        "12",
        "--canvas-width",
        "320",
        "--canvas-height",
        "240",
        "--json",
    ];

    let (ok, full, _) = run_json(&base);
    assert!(ok);
    let full_bytes = full["bytes_written"].as_u64().unwrap();

    let limit = full_bytes / 3;
    let limit_arg = limit.to_string();
    let mut args = base.to_vec();
    args.extend(["--max-size", &limit_arg]);
    let (ok, fit, err) = run_json(&args);
    assert!(ok, "max-size failed: {err}");
    let written = fit["bytes_written"].as_u64().unwrap();
    assert!(written <= limit, "{written} > {limit}");
    assert_eq!(std::fs::metadata(&out).unwrap().len(), written);
    assert_eq!(fit["frame_count"], 12, "frame count must be preserved");
    assert_eq!(fit["max_size_bytes"].as_u64(), Some(limit));
    let scale = fit["size_fit_scale"].as_f64().unwrap();
    assert!(scale < 1.0 && scale > 0.2, "scale {scale}");
    let dims = fit["frame_dimensions"].as_array().unwrap();
    assert!(dims[0].as_u64().unwrap() < 320);

    // An impossible budget fails with a structured, actionable error.
    let mut args = base.to_vec();
    args.extend(["--max-size", "50B"]);
    let (ok, _, err) = run_json(&args);
    assert!(!ok);
    assert_eq!(err["code"], "INVALID_ARGUMENT");
    assert!(err["message"].as_str().unwrap().contains("max-size"));

    // --max-size without an encoded output is rejected.
    let frames_dir = dir.path().join("frames");
    let (ok, _, err) = run_json(&[
        "animate",
        "sphere",
        "--frames-dir",
        frames_dir.to_str().unwrap(),
        "--max-size",
        "1MB",
        "--json",
    ]);
    assert!(!ok);
    assert_eq!(err["code"], "INVALID_ARGUMENT");
}

#[test]
fn procedural_sources_skip_preset_photo_prep_unless_flags_are_explicit() {
    let prep = |args: &[&str]| {
        let (ok, report, err) = run_json(args);
        assert!(ok, "{err}");
        report["recipe"]["preprocess"].clone()
    };

    let generated = prep(&[
        "generate",
        "sphere",
        "-o",
        "x.png",
        "--preset",
        "risograph",
        "--dry-run",
        "--json",
    ]);
    assert_eq!(generated["auto_levels"], false);
    assert_eq!(generated["contrast"], 0.0);
    assert_eq!(generated["sharpen"], 0.0);

    let explicit = prep(&[
        "generate",
        "sphere",
        "-o",
        "x.png",
        "--preset",
        "risograph",
        "--auto-levels",
        "--contrast",
        "0.2",
        "--dry-run",
        "--json",
    ]);
    assert_eq!(explicit["auto_levels"], true);
    assert!((explicit["contrast"].as_f64().unwrap() - 0.2).abs() < 1e-6);

    let animated = prep(&[
        "animate",
        "sphere",
        "-o",
        "x.gif",
        "--preset",
        "risograph",
        "--dry-run",
        "--json",
    ]);
    assert_eq!(animated["auto_levels"], false);

    // Photos keep the preset's full preparation.
    let dir = TempDir::new().unwrap();
    let photo = dir.path().join("photo.png");
    let (ok, _, err) = run_json(&[
        "generate",
        "noise",
        "-o",
        photo.to_str().unwrap(),
        "--canvas-width",
        "32",
        "--canvas-height",
        "32",
        "--json",
    ]);
    assert!(ok, "{err}");
    let applied = prep(&[
        "apply",
        photo.to_str().unwrap(),
        "-o",
        "y.png",
        "--preset",
        "risograph",
        "--dry-run",
        "--json",
    ]);
    assert_eq!(applied["auto_levels"], true);
    assert!(applied["sharpen"].as_f64().unwrap() > 0.0);
}

#[test]
fn optimize_flag_shrinks_png_losslessly_and_round_trips_through_recipes() {
    let dir = TempDir::new().unwrap();
    let path = |name: &str| dir.path().join(name).to_str().unwrap().to_string();
    let generate = |extra: &[&str], out: &str| {
        let result = dith_bin()
            .args([
                "generate",
                "sphere",
                "--canvas-width",
                "96",
                "--canvas-height",
                "64",
            ])
            .args(["--json", "-o", out])
            .args(extra)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "generate failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        serde_json::from_slice::<Value>(&result.stdout).unwrap()
    };

    generate(&["--preset", "risograph"], &path("default.png"));
    let report = generate(
        &["--preset", "risograph", "--optimize", "--emit-recipe"],
        &path("optimized.png"),
    );
    assert_eq!(report["recipe"]["export"]["optimize"], true);

    let default_bytes = std::fs::read(path("default.png")).unwrap();
    let optimized_bytes = std::fs::read(path("optimized.png")).unwrap();
    assert!(optimized_bytes.len() < default_bytes.len());
    assert_eq!(
        image::load_from_memory(&optimized_bytes)
            .unwrap()
            .to_rgba8(),
        image::load_from_memory(&default_bytes).unwrap().to_rgba8()
    );

    let mut recipe = report["recipe"].clone();
    std::fs::write(path("optimize.json"), recipe.to_string()).unwrap();
    generate(&["--recipe", &path("optimize.json")], &path("replayed.png"));
    assert_eq!(
        std::fs::read(path("replayed.png")).unwrap(),
        optimized_bytes
    );

    recipe["export"].as_object_mut().unwrap().remove("optimize");
    std::fs::write(path("legacy.json"), recipe.to_string()).unwrap();
    generate(&["--recipe", &path("legacy.json")], &path("legacy.png"));
    assert_eq!(std::fs::read(path("legacy.png")).unwrap(), default_bytes);
}

#[test]
fn solid_ink_flags_validate_reach_catalogs_and_round_trip_through_recipes() {
    let dir = TempDir::new().unwrap();
    let path = |name: &str| dir.path().join(name).to_str().unwrap().to_string();
    let (ok, _, err) = run_json(&[
        "generate",
        "sphere",
        "--canvas-width",
        "120",
        "--canvas-height",
        "90",
        "-o",
        &path("src.png"),
        "--json",
    ]);
    assert!(ok, "{err}");

    for (flag, value) in [
        ("--ink-below", "1.2"),
        ("--paper-above", "-0.1"),
        ("--ink-lines", "3"),
    ] {
        let (ok, _, err) = run_json(&[
            "apply",
            &path("src.png"),
            "-o",
            &path("bad.png"),
            flag,
            value,
            "--dry-run",
            "--json",
        ]);
        assert!(!ok);
        assert_eq!(err["code"], "INVALID_ARGUMENT", "{flag}");
        assert!(
            err["message"].as_str().unwrap().contains("0.0..=1.0"),
            "{err}"
        );
    }

    let (ok, presets, _) = run_json(&["list", "presets", "--json"]);
    assert!(ok);
    let manga = presets
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "manga")
        .expect("manga preset is listed");
    assert_eq!(manga["recipe"]["dither"]["algorithm"], "halftone");
    assert!(manga["recipe"]["dither"]["ink_lines"].as_f64().unwrap() > 0.0);

    let (ok, caps, _) = run_json(&["capabilities"]);
    assert!(ok);
    let flags: Vec<&str> = caps["parameter_ranges"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["flag"].as_str())
        .collect();
    for flag in ["--ink-below", "--paper-above", "--ink-lines"] {
        assert!(flags.contains(&flag), "capabilities lacks {flag}");
    }

    let (ok, report, err) = run_json(&[
        "apply",
        &path("src.png"),
        "-o",
        &path("manga.png"),
        "--preset",
        "manga",
        "--ink-lines",
        "0.8",
        "--emit-recipe",
        "--json",
    ]);
    assert!(ok, "{err}");
    assert_eq!(
        report["recipe"]["dither"]["ink_below"].as_f64().unwrap() as f32,
        0.22
    );
    assert_eq!(
        report["recipe"]["dither"]["ink_lines"].as_f64().unwrap() as f32,
        0.8
    );
    std::fs::write(path("manga.json"), report["recipe"].to_string()).unwrap();
    let (ok, _, err) = run_json(&[
        "apply",
        &path("src.png"),
        "-o",
        &path("replayed.png"),
        "--recipe",
        &path("manga.json"),
        "--json",
    ]);
    assert!(ok, "{err}");
    assert_eq!(
        std::fs::read(path("replayed.png")).unwrap(),
        std::fs::read(path("manga.png")).unwrap()
    );
}

#[test]
fn diff_counts_boxes_and_highlights_changes_between_images_and_frames() {
    let dir = TempDir::new().unwrap();
    let path = |name: &str| dir.path().join(name).to_str().unwrap().to_string();
    let sphere = |out: &str, angle: &str| {
        let (ok, _, err) = run_json(&[
            "generate",
            "sphere",
            "-o",
            &path(out),
            "--canvas-width",
            "64",
            "--canvas-height",
            "48",
            "--angle",
            angle,
            "--algorithm",
            "bayer4",
            "--json",
        ]);
        assert!(ok, "{err}");
    };
    sphere("a.png", "135");
    sphere("b.png", "100");

    let (ok, same, err) = run_json(&["diff", &path("a.png"), &path("a.png"), "--json"]);
    assert!(ok, "{err}");
    assert_eq!(same["changed_pixels"], 0);
    assert_eq!(same["bbox"], Value::Null);
    assert_eq!(same["total_pixels"], 64 * 48);

    let (ok, d, err) = run_json(&[
        "diff",
        &path("a.png"),
        &path("b.png"),
        "-o",
        &path("hl.png"),
        "--json",
    ]);
    assert!(ok, "{err}");
    let changed = d["changed_pixels"].as_u64().unwrap();
    assert!(changed > 0);
    let bbox = &d["bbox"];
    assert!(bbox["width"].as_u64().unwrap() <= 64);
    let rows: u64 = d["changed_rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r[1].as_u64().unwrap() - r[0].as_u64().unwrap() + 1)
        .sum();
    assert!(rows <= bbox["height"].as_u64().unwrap());
    let hl = image::open(path("hl.png")).unwrap().to_rgba8();
    let red = hl.pixels().filter(|p| p.0 == [230, 40, 70, 255]).count() as u64;
    assert_eq!(red, changed);

    let (ok, _, err) = run_json(&[
        "diff",
        &path("a.png"),
        &path("b.png"),
        "-o",
        &path("never.png"),
        "--dry-run",
        "--json",
    ]);
    assert!(ok, "{err}");
    assert!(!std::path::Path::new(&path("never.png")).exists());

    let tiny = image::RgbaImage::new(8, 8);
    tiny.save(path("tiny.png")).unwrap();
    let (ok, _, err) = run_json(&["diff", &path("a.png"), &path("tiny.png"), "--json"]);
    assert!(!ok);
    assert_eq!(err["code"], "INVALID_ARGUMENT");
    let status = dith_bin()
        .args(["diff", &path("a.png"), &path("tiny.png")])
        .output()
        .unwrap()
        .status;
    assert_eq!(status.code(), Some(2));

    let (ok, _, err) = run_json(&[
        "animate",
        "sphere",
        "-o",
        &path("s.gif"),
        "--frames",
        "4",
        "--canvas-width",
        "64",
        "--canvas-height",
        "48",
        "--algorithm",
        "bayer4",
        "--json",
    ]);
    assert!(ok, "{err}");
    let (ok, frames, err) = run_json(&["diff", "--frames", &path("s.gif"), "--loop", "--json"]);
    assert!(ok, "{err}");
    assert_eq!(frames["frame_count"], 4);
    let list = frames["frames"].as_array().unwrap();
    assert_eq!(list.len(), 4, "three consecutive pairs plus the loop seam");
    assert_eq!(
        (&list[3]["index"], &list[3]["previous"]),
        (&Value::from(0), &Value::from(3))
    );
    let total: u64 = list
        .iter()
        .map(|f| f["changed_pixels"].as_u64().unwrap())
        .sum();
    assert_eq!(frames["changed_pixels_total"].as_u64().unwrap(), total);
    assert!(frames["union"]["changed_pixels"].as_u64().unwrap() <= total);

    let (ok, _, err) = run_json(&["diff", "--frames", &path("a.png"), "--json"]);
    assert!(!ok);
    assert_eq!(err["code"], "INVALID_ARGUMENT");
}

#[test]
fn animate_sequences_take_per_frame_times_from_inputs_or_a_timings_file() {
    let dir = TempDir::new().unwrap();
    let path = |name: &str| dir.path().join(name).to_str().unwrap().to_string();
    for (name, shade) in [("a.png", 0u8), ("b.png", 255u8)] {
        image::RgbaImage::from_pixel(24, 16, image::Rgba([shade, shade, shade, 255]))
            .save(path(name))
            .unwrap();
    }
    let timed = |name: &str, spec: &str| format!("{}:{spec}", path(name));

    let (ok, apng, err) = run_json(&[
        "animate",
        &timed("a.png", "4200ms"),
        &timed("b.png", "1f"),
        &timed("a.png", "105ms"),
        "-o",
        &path("blink.apng"),
        "--fps",
        "20",
        "--json",
    ]);
    assert!(ok, "{err}");
    let delays: Vec<u64> = apng["frames"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["delay_ms"].as_u64().unwrap())
        .collect();
    assert_eq!(delays, vec![4200, 50, 105]);
    assert_eq!(apng["total_duration_ms"], 4355);
    assert_eq!(apng["timing_note"], Value::Null, "APNG keeps exact delays");
    let decoded: Vec<u32> = image::codecs::png::PngDecoder::new(std::io::BufReader::new(
        std::fs::File::open(path("blink.apng")).unwrap(),
    ))
    .unwrap()
    .apng()
    .unwrap()
    .into_frames()
    .map(|f| {
        let (n, d) = f.unwrap().delay().numer_denom_ms();
        n / d
    })
    .collect();
    assert_eq!(decoded, vec![4200, 50, 105]);

    let (ok, gif, err) = run_json(&[
        "animate",
        &timed("a.png", "4200ms"),
        &timed("b.png", "1f"),
        &timed("a.png", "105ms"),
        "-o",
        &path("blink.gif"),
        "--fps",
        "20",
        "--json",
    ]);
    assert!(ok, "{err}");
    assert_eq!(gif["frames"][2]["encoded_delay_ms"], 110);
    assert!(
        gif["timing_note"]
            .as_str()
            .unwrap()
            .contains("105 ms -> 110 ms")
    );
    assert_eq!(gif["frames"][1]["changed_pixels"], 24 * 16);
    assert_eq!(gif["changed_pixels_total"], 2 * 24 * 16);

    std::fs::write(path("t.json"), r#"[4200, "50ms", "2f"]"#).unwrap();
    let three = [path("a.png"), path("b.png"), path("a.png")];
    let (ok, from_file, err) = run_json(&[
        "animate",
        &three[0],
        &three[1],
        &three[2],
        "-o",
        &path("t.gif"),
        "--timings",
        &path("t.json"),
        "--fps",
        "20",
        "--hold-end",
        "1",
        "--json",
    ]);
    assert!(ok, "{err}");
    let delays: Vec<u64> = from_file["frames"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["delay_ms"].as_u64().unwrap())
        .collect();
    assert_eq!(
        delays,
        vec![4200, 50, 100, 100],
        "hold-end repeats the last frame at its delay"
    );

    let (ok, _, err) = run_json(&[
        "animate",
        &three[0],
        &three[1],
        "-o",
        &path("x.gif"),
        "--timings",
        &path("t.json"),
        "--json",
    ]);
    assert!(!ok);
    assert!(
        err["message"].as_str().unwrap().contains("3 frame times"),
        "{err}"
    );
    let (ok, _, err) = run_json(&[
        "animate",
        &timed("a.png", "50ms"),
        &three[1],
        &three[2],
        "-o",
        &path("x.gif"),
        "--timings",
        &path("t.json"),
        "--json",
    ]);
    assert!(!ok);
    assert_eq!(err["code"], "INVALID_ARGUMENT");
    let (ok, _, err) = run_json(&[
        "animate",
        &timed("a.png", "5ms"),
        &three[1],
        "-o",
        &path("x.gif"),
        "--json",
    ]);
    assert!(!ok);
    assert!(
        err["message"].as_str().unwrap().contains("10..=65535"),
        "{err}"
    );

    let (ok, dry, err) = run_json(&[
        "animate",
        &timed("a.png", "4200ms"),
        &timed("b.png", "50ms"),
        "-o",
        &path("dry.gif"),
        "--dry-run",
        "--json",
    ]);
    assert!(ok, "{err}");
    assert_eq!(dry["total_duration_ms"], 4250);
    assert!(!std::path::Path::new(&path("dry.gif")).exists());
}

#[test]
fn animate_summary_and_report_only_claim_an_fps_for_uniform_timing() {
    let dir = TempDir::new().unwrap();
    let path = |name: &str| dir.path().join(name).to_str().unwrap().to_string();
    for (name, shade) in [("a.png", 0u8), ("b.png", 255u8)] {
        image::RgbaImage::from_pixel(24, 16, image::Rgba([shade, shade, shade, 255]))
            .save(path(name))
            .unwrap();
    }
    let timed = |name: &str, spec: &str| format!("{}:{spec}", path(name));
    let summary = |args: &[&str]| {
        let out = dith_bin().args(args).output().unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap()
    };

    let uniform = [
        "animate",
        &path("a.png"),
        &path("b.png"),
        "-o",
        &path("uniform.apng"),
        "--fps",
        "10",
    ];
    assert!(summary(&uniform).contains("(2 frames @ 10fps, 24x16,"));
    let (ok, report, err) = run_json(&[&uniform[..], &["--json"]].concat());
    assert!(ok, "{err}");
    assert_eq!(report["fps"], 10);

    let variable = [
        "animate",
        &timed("a.png", "4200ms"),
        &timed("b.png", "50ms"),
        &timed("a.png", "100ms"),
        &timed("b.png", "50ms"),
        "-o",
        &path("blink.apng"),
        "--fps",
        "20",
    ];
    let text = summary(&variable);
    assert!(
        text.contains("(4 frames, 4.40 s, variable timing, 24x16,"),
        "{text}"
    );
    assert!(!text.contains("fps"), "{text}");
    let (ok, report, err) = run_json(&[&variable[..], &["--json"]].concat());
    assert!(ok, "{err}");
    assert_eq!(report["fps"], Value::Null);
    assert_eq!(report["total_duration_ms"], 4400);

    let text = summary(&[&variable[..], &["--dry-run"]].concat());
    assert!(
        text.contains("(4 frames, 4.40 s, variable timing, 24x16,"),
        "{text}"
    );
    let (ok, report, err) = run_json(&[&variable[..], &["--dry-run", "--json"]].concat());
    assert!(ok, "{err}");
    assert_eq!(report["fps"], Value::Null);

    std::fs::write(path("slow.json"), "[2000, 2000]").unwrap();
    let text = summary(&[
        "animate",
        &path("a.png"),
        &path("b.png"),
        "-o",
        &path("slow.apng"),
        "--timings",
        &path("slow.json"),
    ]);
    assert!(text.contains("(2 frames, 4.00 s, 24x16,"), "{text}");
}

#[test]
fn ink_overlays_are_exact_snap_to_the_pixel_grid_and_change_per_frame() {
    let dir = TempDir::new().unwrap();
    let path = |name: &str| dir.path().join(name).to_str().unwrap().to_string();
    std::fs::write(
        path("eye.svg"),
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 48"><ellipse cx="20" cy="20" rx="5" ry="7" fill="#000"/><circle cx="19" cy="17" r="2" fill="#fff"/></svg>"##,
    )
    .unwrap();
    std::fs::write(
        path("shut.svg"),
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 48"><path d="M15 21 Q20 26 25 21" stroke="#000" stroke-width="2" fill="none"/></svg>"##,
    )
    .unwrap();
    let common = [
        "--canvas-width",
        "64",
        "--canvas-height",
        "48",
        "--algorithm",
        "atkinson",
        "--colors",
        "#F5F1EA,#171717",
        "--pixel-size",
        "2",
    ];
    let mut args = vec!["generate", "sphere", "-o"];
    let out = path("eyes.png");
    args.push(&out);
    args.extend(common);
    let eye = path("eye.svg");
    args.extend(["--overlay", eye.as_str(), "--emit-recipe", "--json"]);
    let (ok, report, err) = run_json(&args);
    assert!(ok, "{err}");
    assert_eq!(report["recipe"]["overlay"]["layers"][0], eye.as_str());
    let img = image::open(&out).unwrap().to_rgba8();
    assert_eq!(
        img.get_pixel(20, 23).0,
        [0x17, 0x17, 0x17, 255],
        "eye is solid ink"
    );
    assert_eq!(
        img.get_pixel(19, 17).0,
        [0xF5, 0xF1, 0xEA, 255],
        "highlight snaps to paper"
    );
    for by in 0..24 {
        for bx in 0..32 {
            let p = img.get_pixel(bx * 2, by * 2);
            for (dx, dy) in [(1, 0), (0, 1), (1, 1)] {
                assert_eq!(
                    img.get_pixel(bx * 2 + dx, by * 2 + dy),
                    p,
                    "block {bx},{by}"
                );
            }
        }
    }

    std::fs::write(path("r.json"), report["recipe"].to_string()).unwrap();
    let (ok, _, err) = run_json(&[
        "apply",
        &path("eyes.png"),
        "-o",
        &path("noop.png"),
        "--recipe",
        &path("r.json"),
        "--json",
    ]);
    assert!(ok, "{err}");

    let (ok, red, err) = run_json(&[
        "generate",
        "sphere",
        "-o",
        &path("red.png"),
        "--canvas-width",
        "64",
        "--canvas-height",
        "48",
        "--overlay",
        &eye,
        "--overlay-color",
        "#ff0000",
        "--json",
    ]);
    assert!(ok, "{err}");
    assert_eq!(red["status"], "ok");
    let red = image::open(path("red.png")).unwrap().to_rgba8();
    assert_eq!(red.get_pixel(19, 17).0, [255, 0, 0, 255]);

    let (ok, _, err) = run_json(&[
        "generate",
        "sphere",
        "-o",
        &path("x.png"),
        "--overlay",
        &path("missing.svg"),
        "--json",
    ]);
    assert!(!ok);
    assert_eq!(err["code"], "INVALID_ASSET");

    let still = path("eyes.png");
    let frames = format!("{eye},{}", path("shut.svg"));
    let blink = path("blink.gif");
    let mut anim = vec![
        "animate",
        still.as_str(),
        still.as_str(),
        "-o",
        blink.as_str(),
        "--overlay-frames",
        frames.as_str(),
        "--json",
    ];
    anim.extend(common.iter().skip(4));
    let (ok, report, err) = run_json(&anim);
    assert!(ok, "{err}");
    assert!(report["frames"][1]["changed_pixels"].as_u64().unwrap() > 0);
    let (ok, d, err) = run_json(&["diff", "--frames", &blink, "--json"]);
    assert!(ok, "{err}");
    let bbox = &d["union"]["bbox"];
    let (x, y) = (bbox["x"].as_u64().unwrap(), bbox["y"].as_u64().unwrap());
    let (w, h) = (
        bbox["width"].as_u64().unwrap(),
        bbox["height"].as_u64().unwrap(),
    );
    assert!(
        x >= 14 && x + w <= 26 && y >= 12 && y + h <= 28,
        "changes stay on the eye: {bbox}"
    );

    let one = path("shut.svg");
    let (ok, _, err) = run_json(&[
        "animate",
        &still,
        &still,
        "-o",
        &path("x.gif"),
        "--overlay-frames",
        &one,
        "--json",
    ]);
    assert!(!ok);
    assert!(
        err["message"].as_str().unwrap().contains("1 overlays"),
        "{err}"
    );
}

#[test]
fn inflate_generator_puffs_svg_shapes_and_validates_its_flags() {
    let dir = TempDir::new().unwrap();
    let path = |name: &str| dir.path().join(name).to_str().unwrap().to_string();
    std::fs::write(
        path("blob.svg"),
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 96 64"><circle cx="34" cy="32" r="18"/><circle cx="62" cy="32" r="18"/></svg>"#,
    )
    .unwrap();
    std::fs::write(
        path("paint.svg"),
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 96 64"><circle cx="62" cy="32" r="10" fill="#000"/></svg>"##,
    )
    .unwrap();
    let blob = path("blob.svg");
    let render = |out: &str, extra: &[&str]| {
        let mut args = vec![
            "generate",
            "inflate",
            "-o",
            out,
            "--shape",
            &blob,
            "--canvas-width",
            "96",
            "--canvas-height",
            "64",
            "--stops",
            "#000000,#ffffff",
            "--algorithm",
            "threshold",
            "--json",
        ];
        args.extend(extra);
        run_json(&args)
    };
    let (ok, _, err) = render(&path("hard.png"), &[]);
    assert!(ok, "{err}");
    let (ok, _, err) = render(
        &path("soft.png"),
        &[
            "--blend",
            "16",
            "--ground-shadow",
            "0.9",
            "--paint",
            &path("paint.svg"),
        ],
    );
    assert!(ok, "{err}");
    let hard = image::open(path("hard.png")).unwrap().to_rgba8();
    let soft = image::open(path("soft.png")).unwrap().to_rgba8();
    assert_eq!(
        hard.get_pixel(1, 1).0,
        [255, 255, 255, 255],
        "background is the light stop"
    );
    assert_eq!(
        hard.get_pixel(48, 15).0,
        [255, 255, 255, 255],
        "hard union leaves the waist open"
    );
    assert_eq!(
        soft.get_pixel(62, 32).0,
        [0, 0, 0, 255],
        "paint darkens the surface"
    );
    assert_eq!(hard.get_pixel(62, 32).0, [255, 255, 255, 255]);
    let (ok, again, err) = render(
        &path("soft2.png"),
        &[
            "--blend",
            "16",
            "--ground-shadow",
            "0.9",
            "--paint",
            &path("paint.svg"),
        ],
    );
    assert!(ok, "{err}");
    assert_eq!(again["status"], "ok");
    assert_eq!(
        std::fs::read(path("soft.png")).unwrap(),
        std::fs::read(path("soft2.png")).unwrap()
    );

    let (ok, _, err) = run_json(&["generate", "inflate", "-o", &path("x.png"), "--json"]);
    assert!(!ok);
    assert_eq!(err["code"], "INVALID_ARGUMENT");
    assert!(
        err["message"].as_str().unwrap().contains("--shape"),
        "{err}"
    );
    let (ok, _, err) = run_json(&[
        "generate",
        "sphere",
        "-o",
        &path("x.png"),
        "--shape",
        &path("blob.svg"),
        "--json",
    ]);
    assert!(!ok);
    assert!(
        err["message"]
            .as_str()
            .unwrap()
            .contains("only apply to inflate"),
        "{err}"
    );
    let (ok, _, err) = render(&path("x.png"), &["--depth", "9"]);
    assert!(!ok);
    assert!(err["message"].as_str().unwrap().contains("depth"), "{err}");
    let (ok, _, err) = run_json(&[
        "generate",
        "inflate",
        "-o",
        &path("x.png"),
        "--shape",
        &path("nope.svg"),
        "--json",
    ]);
    assert!(!ok);
    assert_eq!(err["code"], "INVALID_ASSET");

    let (ok, anim, err) = run_json(&[
        "animate",
        "inflate",
        "-o",
        &path("orbit.gif"),
        "--shape",
        &path("blob.svg"),
        "--frames",
        "4",
        "--canvas-width",
        "96",
        "--canvas-height",
        "64",
        "--json",
    ]);
    assert!(ok, "{err}");
    assert!(
        anim["changed_pixels_total"].as_u64().unwrap() > 0,
        "the light orbits"
    );
    let (ok, still, err) = run_json(&[
        "animate",
        "inflate",
        "-o",
        &path("still.gif"),
        "--shape",
        &path("blob.svg"),
        "--frames",
        "4",
        "--cycles",
        "0",
        "--canvas-width",
        "96",
        "--canvas-height",
        "64",
        "--json",
    ]);
    assert!(ok, "{err}");
    assert_eq!(
        still["changed_pixels_total"], 0,
        "--cycles 0 keeps the light still"
    );
}

#[test]
fn locked_diff_keeps_error_diffusion_still_outside_what_moves() {
    let dir = TempDir::new().unwrap();
    let path = |name: &str| dir.path().join(name).to_str().unwrap().to_string();
    let face = |name: &str, eye_ry: f32| {
        std::fs::write(
            path(name),
            format!(
                r##"<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="-1 -1 2 2"><defs><radialGradient id="g" cx="0.35" cy="0.3" r="0.8"><stop offset="0" stop-color="#eee"/><stop offset="1" stop-color="#555"/></radialGradient></defs><rect x="-1" y="-1" width="2" height="2" fill="#F5F1EA"/><ellipse cx="0" cy="0.1" rx="0.66" ry="0.54" fill="url(#g)"/><ellipse cx="-0.25" cy="0.04" rx="0.075" ry="{eye_ry}" fill="#171717"/><ellipse cx="0.25" cy="0.04" rx="0.075" ry="{eye_ry}" fill="#171717"/></svg>"##
            ),
        )
        .unwrap();
    };
    face("open.svg", 0.10);
    face("shut.svg", 0.012);
    let run = |temporal: &str, out: &str| {
        let (ok, report, err) = run_json(&[
            "animate",
            &format!("{}:400ms", path("open.svg")),
            &format!("{}:100ms", path("shut.svg")),
            "-o",
            &path(out),
            "--temporal",
            temporal,
            "--algorithm",
            "atkinson",
            "--colors",
            "#F5F1EA,#171717",
            "--json",
        ]);
        assert!(ok, "{err}");
        let (ok, diff, err) = run_json(&["diff", "--frames", &path(out), "--json"]);
        assert!(ok, "{err}");
        (report, diff)
    };
    let (locked, locked_frames) = run("locked", "locked.gif");
    let (fixed, fixed_frames) = run("locked-diff", "fixed.gif");
    let changed = |r: &Value| r["frames"][1]["changed_pixels"].as_u64().unwrap();
    assert!(
        changed(&fixed) * 3 < changed(&locked),
        "locked-diff {} vs locked {}",
        changed(&fixed),
        changed(&locked)
    );
    assert_eq!(
        changed(&fixed),
        fixed_frames["frames"][0]["changed_pixels"]
            .as_u64()
            .unwrap()
    );
    // Eyes span x 42..54 and 74..86, y 61..72 at 128px; the default 4px margin may add a little.
    let bbox = &fixed_frames["union"]["bbox"];
    let (x, y) = (bbox["x"].as_u64().unwrap(), bbox["y"].as_u64().unwrap());
    let (w, h) = (
        bbox["width"].as_u64().unwrap(),
        bbox["height"].as_u64().unwrap(),
    );
    assert!(x >= 37 && x + w <= 91 && y >= 56 && y + h <= 78, "{bbox}");
    assert!(locked_frames["union"]["bbox"]["height"].as_u64().unwrap() > h);
    assert!(
        std::fs::metadata(path("fixed.gif")).unwrap().len()
            < std::fs::metadata(path("locked.gif")).unwrap().len()
    );

    let (ok, _, err) = run_json(&[
        "animate",
        &path("open.svg"),
        &path("shut.svg"),
        "-o",
        &path("x.gif"),
        "--diff-margin",
        "2",
        "--json",
    ]);
    assert!(!ok);
    assert!(
        err["message"].as_str().unwrap().contains("locked-diff"),
        "{err}"
    );
    let (ok, _, err) = run_json(&[
        "animate",
        &path("open.svg"),
        &path("shut.svg"),
        "-o",
        &path("x.gif"),
        "--temporal",
        "locked-diff",
        "--diff-threshold",
        "2",
        "--json",
    ]);
    assert!(!ok);
    assert_eq!(err["code"], "INVALID_ARGUMENT");
}

#[test]
fn catalogs_list_the_new_command_generator_and_flags() {
    let (ok, caps, _) = run_json(&["capabilities"]);
    assert!(ok);
    let has = |list: &str, key: &str, value: &str| {
        caps[list]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e[key].as_str().is_some_and(|v| v.contains(value)))
    };
    assert!(has("commands", "name", "diff"));
    assert!(has("generators", "id", "inflate"));
    for flag in [
        "--diff-margin",
        "--overlay-alpha",
        "--blend",
        "--ground-shadow",
        "--paint",
        "--timings",
    ] {
        assert!(
            has("parameter_ranges", "flag", flag),
            "capabilities lacks {flag}"
        );
    }
    assert!(has("parameter_ranges", "range", "locked-diff"));
    let (ok, gens, _) = run_json(&["list", "generators", "--json"]);
    assert!(ok);
    assert!(
        gens.as_array()
            .unwrap()
            .iter()
            .any(|g| g["id"] == "inflate")
    );
}

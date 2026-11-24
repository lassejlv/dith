//! End-to-end integration tests for the `dith` CLI binary.

use std::process::Command;

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

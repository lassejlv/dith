//! End-to-end tests for `dith plot`: plotter SVG structure, G-code validity, and determinism.

use std::path::Path;
use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;

fn dith(args: &[&str]) -> (bool, Value, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_dith"))
        .args(args)
        .arg("--json")
        .output()
        .unwrap();
    let stdout: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    (
        out.status.success(),
        stdout,
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn plot(args: &[&str]) -> Value {
    let mut full = vec!["plot"];
    full.extend_from_slice(args);
    let (ok, report, stderr) = dith(&full);
    assert!(ok, "plot failed: {stderr}");
    assert_eq!(report["status"], "ok");
    report
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

/// Minimal G-code simulator: tracks pen state and position, rejects anything a pen plotter
/// would choke on, and returns (dots drawn, pen-up travel mm, final position).
fn simulate(gcode: &str, pen_up: &str, pen_down: &str, max_xy: [f64; 2]) -> (usize, f64, [f64; 2]) {
    let mut pen_is_down = false;
    let mut units = false;
    let mut absolute = false;
    let mut pos = [0.0_f64, 0.0];
    let mut dots = 0;
    let mut travel = 0.0;
    let mut ended = false;
    for raw in gcode.lines() {
        assert!(!ended, "commands after M2: {raw}");
        let line = raw.split(';').next().unwrap().trim();
        if line.is_empty() {
            continue;
        }
        if line == pen_down {
            assert!(!pen_is_down, "pen lowered twice");
            pen_is_down = true;
            dots += 1;
            continue;
        }
        if line == pen_up {
            pen_is_down = false;
            continue;
        }
        let mut words = line.split_whitespace();
        let cmd = words.next().unwrap();
        let mut target = pos;
        let mut feed = None;
        let mut ij = [None, None];
        for w in words {
            let (letter, value) = w.split_at(1);
            let v: f64 = value
                .parse()
                .unwrap_or_else(|_| panic!("bad number in '{raw}'"));
            match letter {
                "X" => target[0] = v,
                "Y" => target[1] = v,
                "F" => feed = Some(v),
                "I" => ij[0] = Some(v),
                "J" => ij[1] = Some(v),
                "P" => assert!(v >= 0.0),
                other => panic!("unexpected word {other} in '{raw}'"),
            }
        }
        match cmd {
            "G21" => units = true,
            "G90" => absolute = true,
            "G4" => {}
            "M2" => {
                assert!(!pen_is_down, "program ends with the pen down");
                ended = true;
            }
            "G0" | "G1" | "G2" | "G3" => {
                assert!(units && absolute, "motion before G21/G90: {raw}");
                for k in 0..2 {
                    assert!(
                        (-0.001..=max_xy[k] + 0.001).contains(&target[k]),
                        "{raw} leaves the sheet"
                    );
                }
                if cmd == "G0" {
                    assert!(!pen_is_down, "rapid move with the pen down: {raw}");
                    travel += ((target[0] - pos[0]).powi(2) + (target[1] - pos[1]).powi(2)).sqrt();
                } else {
                    assert!(pen_is_down, "drawing move with the pen up: {raw}");
                    assert!(
                        feed.is_some_and(|f| f > 0.0),
                        "draw move without feed: {raw}"
                    );
                    if cmd != "G1" {
                        assert!(ij[0].is_some() && ij[1].is_some(), "arc without I/J: {raw}");
                    }
                }
                pos = target;
            }
            other => panic!("unexpected command {other} in '{raw}'"),
        }
    }
    assert!(ended, "missing M2");
    (dots, travel, pos)
}

#[test]
fn plot_writes_physical_svg_with_one_path_per_pen_in_tour_order() {
    let dir = TempDir::new().unwrap();
    let svg = dir.path().join("sphere.svg");
    let report = plot(&["sphere", "-o", s(&svg), "-n", "600", "--paper", "a5"]);
    assert_eq!(report["points"], 600);
    assert_eq!(report["points_requested"], 600);
    assert_eq!(report["format"], "svg");
    assert_eq!(report["layout"]["orientation"], "landscape");
    assert!(report["iterations"].as_u64().unwrap() >= 1);
    let nearest = report["travel_nearest_mm"].as_f64().unwrap();
    let optimised = report["travel_mm"].as_f64().unwrap();
    assert!(optimised < nearest, "{optimised} vs {nearest}");
    assert!(nearest < report["travel_unordered_mm"].as_f64().unwrap());
    assert!(report["estimated_plot_seconds"].as_f64().unwrap() > 0.0);

    let text = std::fs::read_to_string(&svg).unwrap();
    assert!(text.contains(r#"width="210mm" height="148mm" viewBox="0 0 210 148""#));
    assert_eq!(text.matches("<path ").count(), 1);
    assert_eq!(text.matches(r#"inkscape:groupmode="layer""#).count(), 1);
    assert_eq!(text.matches("h0").count(), 600);
}

#[test]
fn plot_gcode_is_valid_and_matches_the_report() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("gradient.png");
    let (ok, _, stderr) = dith(&[
        "generate",
        "linear-gradient",
        "-o",
        s(&src),
        "--palette",
        "gray64",
        "--canvas-width",
        "240",
        "--canvas-height",
        "160",
    ]);
    assert!(ok, "{stderr}");
    let gcode = dir.path().join("gradient.gcode");
    let report = plot(&[
        s(&src),
        "-o",
        s(&gcode),
        "-n",
        "500",
        "--pen-up",
        "M5",
        "--pen-down",
        "M3 S30",
        "--dwell",
        "0.05",
        "--paper",
        "letter",
    ]);
    assert_eq!(report["format"], "gcode");
    assert_eq!(report["paper"]["name"], "letter");
    let text = std::fs::read_to_string(&gcode).unwrap();
    assert!(text.starts_with("; dith "));
    assert!(text.contains("; units mm, origin bottom-left, pen-up \"M5\", pen-down \"M3 S30\""));
    assert_eq!(text.matches("G4 P0.05").count(), 2 * 500 + 1);
    let layout = &report["layout"];
    let size = [
        layout["width_mm"].as_f64().unwrap(),
        layout["height_mm"].as_f64().unwrap(),
    ];
    let (dots, travel, end) = simulate(&text, "M5", "M3 S30", size);
    assert_eq!(dots, 500);
    assert_eq!(end, [0.0, 0.0]);
    let reported = report["travel_mm"].as_f64().unwrap();
    assert!(
        (travel - reported).abs() < 1.0,
        "simulated {travel} vs reported {reported}"
    );

    // Circles become full G2 arcs; strokes become G1 dashes.
    let circles = dir.path().join("circles.gcode");
    plot(&[
        s(&src),
        "-o",
        s(&circles),
        "-n",
        "200",
        "--dot",
        "circle",
        "--min-dot",
        "0.8",
        "--max-dot",
        "1.6",
        "--origin",
        "top-left",
    ]);
    let text = std::fs::read_to_string(&circles).unwrap();
    assert_eq!(text.matches("\nG2 ").count(), 200);
    simulate(&text, "G0 Z2", "G1 Z0 F1000", [297.0, 210.0]);
    let strokes = dir.path().join("strokes.nc");
    plot(&[
        s(&src),
        "-o",
        s(&strokes),
        "-n",
        "200",
        "--dot",
        "stroke",
        "--origin",
        "center",
    ]);
    let text = std::fs::read_to_string(&strokes).unwrap();
    assert_eq!(text.matches("\nG1 X").count(), 200);
}

#[test]
fn plot_output_is_byte_deterministic() {
    let dir = TempDir::new().unwrap();
    let a = dir.path().join("a.gcode");
    let b = dir.path().join("b.gcode");
    let args = ["mesh", "-n", "1500", "--seed", "9", "--dot", "circle"];
    plot(&[&args[..], &["-o", s(&a)]].concat());
    plot(&[&args[..], &["-o", s(&b)]].concat());
    assert_eq!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap());
    let c = dir.path().join("c.gcode");
    plot(&[
        "mesh",
        "-n",
        "1500",
        "--seed",
        "10",
        "--dot",
        "circle",
        "-o",
        s(&c),
    ]);
    assert_ne!(std::fs::read(&a).unwrap(), std::fs::read(&c).unwrap());
}

#[test]
fn plot_splits_multi_color_palettes_into_pen_layers() {
    let dir = TempDir::new().unwrap();
    let svg = dir.path().join("pens.svg");
    let report = plot(&[
        "mesh",
        "-o",
        s(&svg),
        "-n",
        "2000",
        "--colors",
        "#111111,#e11d48,#2563eb,#ffffff",
        "--stops",
        "#111111,#e11d48,#2563eb,#ffffff",
    ]);
    assert_eq!(report["paper_color"], "#ffffff");
    let pens = report["pens"].as_array().unwrap();
    assert!(pens.len() >= 2, "{pens:?}");
    let total: u64 = pens.iter().map(|p| p["points"].as_u64().unwrap()).sum();
    assert_eq!(total, report["points"].as_u64().unwrap());
    let text = std::fs::read_to_string(&svg).unwrap();
    assert_eq!(text.matches("<path ").count(), pens.len());
    for pen in pens {
        let color = pen["color"].as_str().unwrap();
        assert!(text.contains(&format!(r#"stroke="{color}""#)));
    }
}

#[test]
fn plot_dry_run_density_and_errors_are_structured() {
    let (ok, report, _) = dith(&[
        "plot",
        "sphere",
        "--dry-run",
        "--density",
        "2",
        "--paper",
        "a3",
    ]);
    assert!(ok);
    assert_eq!(report["dry_run"], true);
    assert_eq!(report["bytes_written"], 0);
    assert!(report["points_requested"].as_u64().unwrap() > 100);
    assert!(report["timings_ms"].is_null());

    let (ok, _, stderr) = dith(&["plot", "sphere", "-o", "x.svg", "--paper", "a9"]);
    assert!(!ok);
    let err: Value = serde_json::from_str(&stderr).unwrap();
    assert_eq!(err["code"], "INVALID_ARGUMENT");
    assert!(err["message"].as_str().unwrap().contains("paper"));

    let (ok, _, stderr) = dith(&["plot", "sphere", "-o", "x.png"]);
    assert!(!ok);
    let err: Value = serde_json::from_str(&stderr).unwrap();
    assert!(err["message"].as_str().unwrap().contains("format"));
}

#[test]
fn plot_is_discoverable_in_capabilities_and_list() {
    let (ok, caps, _) = dith(&["capabilities"]);
    assert!(ok);
    assert!(
        caps["commands"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == "plot")
    );
    assert_eq!(caps["plot_formats"][0]["id"], "gcode");
    assert!(
        caps["paper_sizes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == "a4")
    );
    let (ok, formats, _) = dith(&["list", "formats"]);
    assert!(ok);
    assert!(
        formats
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["id"] == "gcode")
    );
    let (ok, papers, _) = dith(&["list", "papers"]);
    assert!(ok);
    assert_eq!(papers.as_array().unwrap().len(), 8);
}

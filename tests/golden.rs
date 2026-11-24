//! Cross-platform golden-hash determinism tests.
//!
//! Runs a fixed corpus of `dith` invocations (every built-in preset, algorithm, generator, and
//! output format, plus animations, auto-palette, preprocessing, and compare contact sheets) against
//! small committed fixtures, hashes every output with SHA-256, and compares the hashes with
//! `tests/golden/hashes.json`. CI runs this on `x86_64` Linux, aarch64 Linux, and aarch64 macOS,
//! so a passing run on all three proves the same inputs and flags produce byte-identical files.
//!
//! Regenerate the golden file after an intentional output change with:
//!
//! ```text
//! DITH_UPDATE_GOLDEN=1 cargo test --test golden
//! ```
//!
//! Set `DITH_GOLDEN_BIN=/path/to/dith` to check another build (for example a release binary or an
//! older version) against the same golden file.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::{Deserialize, Serialize};
use tempfile::TempDir;

const PHOTO: &str = "tests/golden/fixtures/photo.png";
const ALPHA: &str = "tests/golden/fixtures/alpha.png";
const GOLDEN: &str = "tests/golden/hashes.json";
const UPDATE_ENV: &str = "DITH_UPDATE_GOLDEN";
const BIN_ENV: &str = "DITH_GOLDEN_BIN";

const PRESETS: &[&str] = &[
    "blueprint",
    "crt-amber",
    "editorial-print",
    "film-stipple",
    "gameboy",
    "halftone-zine",
    "lo-fi-photo",
    "mac-classic",
    "phosphor",
    "pico8",
    "risograph",
    "risograph-duo",
    "risograph-red",
    "vapor-mesh",
    "woodcut",
];

const ALGORITHMS: &[&str] = &[
    "floyd-steinberg",
    "atkinson",
    "jarvis-judice-ninke",
    "stucki",
    "burkes",
    "sierra",
    "sierra-two-row",
    "sierra-lite",
    "ostromoukhov",
    "bayer2",
    "bayer4",
    "bayer8",
    "bayer16",
    "halftone",
    "clustered-dot",
    "diagonal-lines",
    "horizontal-lines",
    "vertical-lines",
    "crosshatch",
    "blue-noise",
    "ign",
    "riemersma",
    "random",
    "threshold",
];

const GENERATORS: &[&str] = &[
    "linear-gradient",
    "radial-gradient",
    "mesh",
    "sphere",
    "waves",
    "noise",
    "rings",
    "plasma",
    "checkerboard",
];

/// One corpus entry: `dith` arguments with `{out}` standing for the output path. Cases with an
/// empty `ext` write to stdout, which is hashed instead of a file.
struct Case {
    name: String,
    args: Vec<String>,
    ext: &'static str,
}

impl Case {
    fn new(name: impl Into<String>, ext: &'static str, args: &str) -> Self {
        Self {
            name: name.into(),
            args: args.split_whitespace().map(str::to_string).collect(),
            ext,
        }
    }

    fn output_name(&self) -> String {
        format!("{}.{}", self.name.replace('/', "-"), self.ext)
    }

    /// Shell command that reproduces this case from the repository root.
    fn command(&self) -> String {
        let out = self.output_name();
        let args: Vec<&str> = self
            .args
            .iter()
            .map(|a| {
                if a == "{out}" {
                    out.as_str()
                } else {
                    a.as_str()
                }
            })
            .collect();
        format!("dith {}", args.join(" "))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    command: String,
    bytes: usize,
    sha256: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct GoldenFile {
    #[serde(rename = "_regenerate")]
    regenerate: String,
    cases: BTreeMap<String, Entry>,
}

fn corpus() -> Vec<Case> {
    let mut cases = Vec::new();

    for preset in PRESETS {
        cases.push(Case::new(
            format!("preset/{preset}"),
            "png",
            &format!("apply {PHOTO} -o {{out}} --preset {preset}"),
        ));
    }
    // Larger renders of the libm-sensitive paths (Lanczos upscale, grayscale/saturation powf,
    // blue noise, k-means, Ostromoukhov) to raise the odds of catching a platform difference.
    for preset in [
        "editorial-print",
        "film-stipple",
        "lo-fi-photo",
        "risograph",
    ] {
        cases.push(Case::new(
            format!("preset-large/{preset}"),
            "png",
            &format!("apply {PHOTO} -o {{out}} --preset {preset} --scale 4"),
        ));
    }
    for algorithm in ALGORITHMS {
        cases.push(Case::new(
            format!("algorithm/{algorithm}"),
            "png",
            &format!("apply {PHOTO} -o {{out}} -a {algorithm} -p cobalt-sand"),
        ));
    }
    for space in ["oklab", "linear", "srgb"] {
        cases.push(Case::new(
            format!("color-space/{space}"),
            "png",
            &format!("apply {PHOTO} -o {{out}} -a floyd-steinberg -p pico8 --color-space {space}"),
        ));
    }
    for generator in GENERATORS {
        cases.push(Case::new(
            format!("generate/{generator}"),
            "png",
            &format!("generate {generator} -o {{out}} --canvas-width 160 --canvas-height 120"),
        ));
    }
    cases.push(Case::new(
        "generate/sphere-styled",
        "png",
        "generate sphere -o {out} --canvas-width 80 --canvas-height 80 --preset risograph-duo \
         --angle 60 --frequency 1.7 --stops #0f172a,#38bdf8,#f8fafc",
    ));

    let format = |name: &str, ext: &'static str, flags: &str| {
        Case::new(
            format!("format/{name}"),
            ext,
            &format!("apply {PHOTO} -o {{out}} --preset risograph-duo {flags}"),
        )
    };
    cases.extend([
        format("png", "png", ""),
        format("png-optimize", "png", "--optimize"),
        format("webp", "webp", ""),
        format("svg-rect", "svg", "--svg-shape rect"),
        format("svg-circle", "svg", "--svg-shape circle"),
        format("svg-dot", "svg", "--svg-shape dot --svg-skip-bg"),
        format("gif", "gif", ""),
        format("bmp", "bmp", ""),
        format("tiff", "tiff", ""),
        format("jpeg", "jpg", ""),
        Case::new(
            "format/png-optimize-pico8",
            "png",
            &format!("apply {PHOTO} -o {{out}} --preset pico8 --optimize"),
        ),
        Case::new(
            "format/webp-many-colors",
            "webp",
            &format!("apply {PHOTO} -o {{out}} --auto-palette 12 -a atkinson"),
        ),
        Case::new(
            "format/braille",
            "",
            &format!("apply {PHOTO} --preset mac-classic -f braille --text-width 40"),
        ),
        Case::new(
            "format/ascii",
            "",
            &format!("apply {PHOTO} --preset mac-classic -f ascii --text-width 48"),
        ),
    ]);

    for n in [2, 6, 16] {
        cases.push(Case::new(
            format!("auto-palette/{n}"),
            "png",
            &format!("apply {PHOTO} -o {{out}} --auto-palette {n} --seed 7"),
        ));
    }

    let prep = |name: &str, input: &str, flags: &str| {
        Case::new(
            format!("preprocess/{name}"),
            "png",
            &format!("apply {input} -o {{out}} -a floyd-steinberg -p gray8 {flags}"),
        )
    };
    cases.extend([
        prep(
            "tonal",
            PHOTO,
            "--gamma 0.8 --contrast 0.2 --brightness -0.1 --saturation 1.4 --auto-levels --invert",
        ),
        prep("blur", PHOTO, "--blur 1.5"),
        prep("sharpen", PHOTO, "--sharpen 1.2 --grayscale"),
        prep("resize-contain", PHOTO, "--width 61 --height 40"),
        prep("resize-cover", PHOTO, "--width 50 --height 50 --fit cover"),
        prep("scale-up", PHOTO, "--scale 1.5"),
        prep("pixel-size", PHOTO, "--pixel-size 4"),
        prep(
            "noise-bias",
            PHOTO,
            "--noise 0.2 --bias 0.1 --seed 3 --strength 0.8",
        ),
        prep("alpha-preserve", ALPHA, "--alpha-mode preserve"),
        prep(
            "alpha-flatten",
            ALPHA,
            "--alpha-mode flatten --background #ffcc00",
        ),
        prep("alpha-threshold", ALPHA, "--alpha-mode threshold"),
        prep("alpha-dither", ALPHA, "--alpha-mode dither"),
    ]);

    let anim = |name: &str, ext: &'static str, flags: &str| {
        Case::new(
            format!("animate/{name}"),
            ext,
            &format!("animate -o {{out}} --canvas-width 64 --canvas-height 48 --frames 6 {flags}"),
        )
    };
    cases.extend([
        anim("sphere-gif", "gif", "--pattern sphere --preset risograph"),
        anim(
            "waves-apng",
            "apng",
            "--pattern waves -a bayer4 -p gameboy --easing ease-in-out",
        ),
        anim(
            "plasma-spritesheet",
            "png",
            "--pattern plasma --preset vapor-mesh --anim-format spritesheet --sheet-cols 3",
        ),
        anim(
            "mesh-gif-boil",
            "gif",
            "--pattern mesh -a blue-noise -p cobalt-sand --temporal boil --screen-drift 1,0",
        ),
        anim(
            "rings-apng-cycle",
            "apng",
            "--pattern rings -a bayer8 -p pico8 --cycle-palette --ping-pong",
        ),
        anim(
            "noise-gif-sweep",
            "gif",
            "--pattern noise -a atkinson -p bw --sweep-bias -0.4..0.0 --sweep-mode pulse",
        ),
        anim(
            "checkerboard-apng-optimize",
            "apng",
            "--pattern checkerboard -a ign -p cga --temporal grain --optimize",
        ),
        Case::new(
            "animate/photo-boil-gif",
            "gif",
            &format!("animate {PHOTO} -o {{out}} --frames 4 --temporal boil --preset film-stipple"),
        ),
        Case::new(
            "animate/photo-boil-apng",
            "apng",
            &format!("animate {PHOTO} -o {{out}} --frames 4 --temporal boil -a stucki -p bw"),
        ),
        Case::new(
            "animate/photo-boil-webp",
            "webp",
            &format!(
                "animate {PHOTO} -o {{out}} --frames 4 --temporal boil -a atkinson -p risograph-duo"
            ),
        ),
        anim("sphere-webp", "webp", "--pattern sphere --preset risograph"),
        anim(
            "rings-webp-cycle",
            "webp",
            "--pattern rings -a bayer8 -p pico8 --cycle-palette --ping-pong",
        ),
    ]);

    cases.extend([
        Case::new(
            "plot/photo-svg",
            "svg",
            &format!("plot {PHOTO} -o {{out}} -n 600 --paper a6 --edges 0.8"),
        ),
        Case::new(
            "plot/sphere-gcode",
            "gcode",
            "plot sphere -o {out} -n 400 --paper a6 --dot circle",
        ),
        Case::new(
            "plot/mesh-multipen-svg",
            "svg",
            "plot mesh -o {out} -n 500 --paper a6 --palette risograph-duo",
        ),
    ]);

    cases.extend([
        Case::new(
            "compare/algorithms",
            "png",
            &format!(
                "compare {PHOTO} -o {{out}} --algorithms floyd-steinberg,atkinson,bayer4,blue-noise \
                 -p bw --tile-max-dim 96"
            ),
        ),
        Case::new(
            "compare/presets",
            "png",
            &format!(
                "compare {PHOTO} -o {{out}} --presets gameboy,risograph,lo-fi-photo --tile-max-dim 80"
            ),
        ),
        Case::new(
            "compare/palettes-pattern",
            "png",
            "compare --pattern sphere -o {out} --palettes bw,gameboy,pico8 -a bayer4 \
             --tile-max-dim 64 --light",
        ),
    ]);

    cases
}

/// Run one case with `bin` in `dir` and return the bytes it produced.
fn run_case(case: &Case, bin: &OsStr, root: &Path, dir: &Path) -> Result<Vec<u8>, String> {
    let out = dir.join(case.output_name());
    let args = case.args.iter().map(|a| {
        if a == "{out}" {
            out.clone().into_os_string()
        } else if a.starts_with("tests/golden/") {
            root.join(a).into_os_string()
        } else {
            a.into()
        }
    });
    let result = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("XDG_CONFIG_HOME", dir)
        .env_remove("DITH_PRESETS_DIR")
        .env_remove("TMUX")
        .output()
        .map_err(|e| format!("spawning dith: {e}"))?;
    if !result.status.success() {
        return Err(format!(
            "exited with {}: {}",
            result.status,
            String::from_utf8_lossy(&result.stderr).trim()
        ));
    }
    if case.ext.is_empty() {
        Ok(result.stdout)
    } else {
        std::fs::read(&out).map_err(|e| format!("reading {}: {e}", out.display()))
    }
}

/// Run the whole corpus on all cores and return each case's entry (or error) by name.
fn run_corpus(cases: &[Case]) -> BTreeMap<String, Result<Entry, String>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let bin = std::env::var_os(BIN_ENV).unwrap_or_else(|| env!("CARGO_BIN_EXE_dith").into());
    let dir = TempDir::new().expect("creating temp dir");
    let next = AtomicUsize::new(0);
    let results = Mutex::new(BTreeMap::new());
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get().min(16));
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                while let Some(case) = cases.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let entry = run_case(case, &bin, &root, dir.path()).map(|bytes| Entry {
                        command: case.command(),
                        bytes: bytes.len(),
                        sha256: sha256_hex(&bytes),
                    });
                    results.lock().unwrap().insert(case.name.clone(), entry);
                }
            });
        }
    });
    results.into_inner().unwrap()
}

#[test]
fn outputs_match_golden_hashes() {
    let cases = corpus();
    let mut names: Vec<&str> = cases.iter().map(|c| c.name.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), cases.len(), "duplicate case names in corpus");

    let results = run_corpus(&cases);
    let failures: Vec<String> = results
        .iter()
        .filter_map(|(name, r)| r.as_ref().err().map(|e| format!("  {name}: {e}")))
        .collect();
    assert!(
        failures.is_empty(),
        "{} golden case(s) failed to run:\n{}",
        failures.len(),
        failures.join("\n")
    );
    let actual: BTreeMap<String, Entry> = results
        .into_iter()
        .map(|(name, r)| (name, r.unwrap()))
        .collect();

    let golden_path = Path::new(env!("CARGO_MANIFEST_DIR")).join(GOLDEN);
    if std::env::var_os(UPDATE_ENV).is_some_and(|v| !v.is_empty() && v != "0") {
        let file = GoldenFile {
            regenerate: format!("{UPDATE_ENV}=1 cargo test --test golden"),
            cases: actual,
        };
        let mut text = serde_json::to_string_pretty(&file).unwrap();
        text.push('\n');
        std::fs::write(&golden_path, text).expect("writing golden file");
        eprintln!("wrote {} golden hashes to {GOLDEN}", file.cases.len());
        return;
    }

    let text = std::fs::read_to_string(&golden_path).unwrap_or_else(|e| {
        panic!("reading {GOLDEN}: {e}; create it with `{UPDATE_ENV}=1 cargo test --test golden`")
    });
    let golden: GoldenFile = serde_json::from_str(&text).expect("parsing golden file");

    let mut report = String::new();
    let mut mismatches = 0;
    for (name, entry) in &actual {
        match golden.cases.get(name) {
            Some(expected) if expected.sha256 == entry.sha256 => {}
            Some(expected) => {
                mismatches += 1;
                let _ = writeln!(
                    report,
                    "  CHANGED {name}\n    expected {} ({} bytes)\n    actual   {} ({} bytes)\n    \
                     reproduce: {}",
                    expected.sha256, expected.bytes, entry.sha256, entry.bytes, entry.command
                );
            }
            None => {
                mismatches += 1;
                let _ = writeln!(report, "  NEW     {name}\n    reproduce: {}", entry.command);
            }
        }
    }
    for name in golden.cases.keys().filter(|n| !actual.contains_key(*n)) {
        mismatches += 1;
        let _ = writeln!(
            report,
            "  REMOVED {name} (in {GOLDEN} but not in the corpus)"
        );
    }
    assert!(
        mismatches == 0,
        "{mismatches} of {} golden case(s) differ on {}-{}:\n{report}\nIf the change is \
         intentional, regenerate with `{UPDATE_ENV}=1 cargo test --test golden`. If only one \
         platform differs, that platform's output is not deterministic.",
        actual.len(),
        std::env::consts::ARCH,
        std::env::consts::OS,
    );
}

/// Minimal SHA-256 (FIPS 180-4), so the test needs no extra dependency.
fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (i, word) in block.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*word);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (state, v) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *state = state.wrapping_add(v);
        }
    }
    h.iter().fold(String::with_capacity(64), |mut s, v| {
        let _ = write!(s, "{v:08x}");
        s
    })
}

#[test]
fn sha256_matches_known_vectors() {
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        sha256_hex(&[b'a'; 1000]),
        "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
    );
}

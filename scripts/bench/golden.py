#!/usr/bin/env python3
"""Golden byte comparison for the dith CLI.

    scripts/bench/golden.py render --bin target/release/dith --corpus bench-corpus --out /tmp/golden-new
    scripts/bench/golden.py render --bin /path/to/dith-main   --corpus bench-corpus --out /tmp/golden-main
    scripts/bench/golden.py diff /tmp/golden-main /tmp/golden-new

`render` runs every built-in preset into every output format, the algorithm x palette x color
space matrix, the preprocess and dither flags, the generators, animations, batch and compare, and
writes `manifest.json` mapping every output file to its SHA-256. `diff` exits non-zero when two
manifests differ in any byte. Run `render` under RAYON_NUM_THREADS=1 and under the default thread
count; all four manifests must be identical.
"""

import argparse
import concurrent.futures
import hashlib
import json
import os
import shutil
import subprocess
import sys

FORMATS = {"png": "png", "webp": "webp", "jpeg": "jpg", "gif": "gif", "bmp": "bmp", "tiff": "tiff",
           "svg": "svg", "kitty": "kitty", "braille": "txt", "ascii": "txt"}
ALGORITHMS = ["floyd-steinberg", "atkinson", "jarvis-judice-ninke", "stucki", "burkes", "sierra",
              "sierra-two-row", "sierra-lite", "ostromoukhov", "bayer2", "bayer4", "bayer8", "bayer16",
              "halftone", "clustered-dot", "diagonal-lines", "horizontal-lines", "vertical-lines",
              "crosshatch", "blue-noise", "ign", "riemersma", "random", "threshold"]
CORE_ALGORITHMS = ["floyd-steinberg", "atkinson", "ostromoukhov", "bayer8", "halftone", "blue-noise", "riemersma"]
PALETTES = {"bw": ["-p", "bw"], "gameboy": ["-p", "gameboy"], "pico8": ["-p", "pico8"],
            "nord": ["-p", "nord"], "web216": ["-p", "web216"], "gray16": ["-p", "gray16"],
            "auto8": ["--auto-palette", "8"], "auto32": ["--auto-palette", "32"]}
SPACES = ["oklab", "linear", "srgb"]
PATTERNS = ["linear-gradient", "radial-gradient", "mesh", "sphere", "waves", "noise", "rings", "plasma", "checkerboard"]
FLAG_VARIANTS = {
    "no-serpentine": ["--no-serpentine"], "strength-05": ["--strength", "0.5"], "strength-15": ["--strength", "1.5"],
    "bias-neg": ["--bias=-0.2"], "bias-pos": ["--bias", "0.2"], "noise": ["--noise", "0.2"],
    "noise-seed": ["--noise", "0.35", "--seed", "7"], "pattern-scale": ["--pattern-scale", "3"], "seed": ["--seed", "1234567"],
    "pixel-size": ["--pixel-size", "3"], "width": ["--width", "500"], "height": ["--height", "333"],
    "contain": ["--width", "640", "--height", "640", "--fit", "contain"], "cover": ["--width", "640", "--height", "640", "--fit", "cover"],
    "stretch": ["--width", "640", "--height", "640", "--fit", "stretch"], "scale-down": ["--scale", "0.5"], "scale-up": ["--scale", "1.3"],
    "max-dim": ["--max-dim", "640"], "brightness": ["--brightness", "0.15"], "contrast": ["--contrast", "0.3"],
    "contrast-neg": ["--contrast=-0.4"], "gamma": ["--gamma", "0.8"], "saturation": ["--saturation", "1.6"],
    "desaturate": ["--saturation", "0.3"], "sharpen": ["--sharpen", "1.2"], "blur": ["--blur", "1.8"],
    "auto-levels": ["--auto-levels"], "grayscale": ["--grayscale"], "invert": ["--invert"],
    "tone-stack": ["--brightness=-0.05", "--contrast", "0.2", "--gamma", "1.1", "--saturation", "1.2", "--sharpen", "0.6", "--auto-levels"],
    "ink-below": ["--ink-below", "0.2"], "paper-above": ["--paper-above", "0.8"], "ink-lines": ["--ink-lines", "0.6"],
    "ink-all": ["--ink-below", "0.2", "--paper-above", "0.85", "--ink-lines", "0.5"],
    "colors": ["--colors", "#0f172a,#2563eb,#f8fafc,#f59e0b"],
}
ALPHA_VARIANTS = {
    "alpha-preserve": ["--alpha-mode", "preserve"], "alpha-flatten": ["--alpha-mode", "flatten", "--background", "#203040"],
    "alpha-threshold": ["--alpha-mode", "threshold"], "alpha-dither": ["--alpha-mode", "dither"],
    "alpha-ink": ["--ink-below", "0.25", "--ink-lines", "0.5"], "alpha-blur-sharpen": ["--blur", "1.2", "--sharpen", "0.8"],
    "alpha-resize": ["--width", "400", "--pixel-size", "2"],
}


def build_cases(binary, corpus):
    presets = [p["id"] for p in json.loads(subprocess.check_output([binary, "list", "presets", "--json"]))]
    c = lambda name: os.path.join(corpus, name)
    cases = []

    def add(name, argv):
        cases.append((name, argv))

    # 1. every preset into every output format
    for preset in presets:
        for src in ("migrant-800.jpg", "alpha-1024.png"):
            for fmt, ext in FORMATS.items():
                add(f"preset/{preset}/{src}/{fmt}", ["apply", c(src), "-P", preset, "-f", fmt, "-o", "{O}/out." + ext])
        add(f"preset/{preset}/generate", ["generate", "sphere", "-P", preset, "-o", "{O}/out.png"])
        add(f"preset/{preset}/big", ["apply", c("wheatfield-3000.jpg"), "-P", preset, "-o", "{O}/out.bmp"])
    add("preset/optimize/png", ["apply", c("migrant-800.jpg"), "-P", "mac-classic", "--optimize", "-o", "{O}/out.png"])
    add("preset/svg-skip-bg", ["apply", c("migrant-800.jpg"), "-P", "halftone-zine", "--width", "200", "--svg-skip-bg", "-o", "{O}/out.svg"])

    # 2. algorithm x palette x color space
    for src, palettes in (("wave-1200.png", list(PALETTES)), ("alpha-1024.png", ["bw", "pico8", "auto8"]), ("tetons-1600.jpg", ["bw", "gray16", "auto8"])):
        for algorithm in ALGORITHMS:
            for palette in palettes:
                for space in SPACES:
                    add(f"matrix/{src}/{algorithm}/{palette}/{space}",
                        ["apply", c(src), "-a", algorithm, *PALETTES[palette], "--color-space", space, "-o", "{O}/out.bmp"])

    # 3. dither and preprocess flags
    for algorithm in CORE_ALGORITHMS:
        for label, flags in FLAG_VARIANTS.items():
            add(f"flags/{algorithm}/{label}", ["apply", c("wave-1200.png"), "-a", algorithm, "-p", "pico8", *flags, "-o", "{O}/out.png"])
        for label, flags in ALPHA_VARIANTS.items():
            add(f"flags/{algorithm}/{label}", ["apply", c("alpha-1024.png"), "-a", algorithm, "-p", "gameboy", *flags, "-o", "{O}/out.png"])

    # 4. the full corpus at full size
    for src in ("tapestry-4000.jpg", "pillars-3200.jpg", "wheatfield-3000.jpg", "rhinoceros-2400.png", "earth-2048.png", "tetons-1600.jpg"):
        add(f"corpus/{src}/default", ["apply", c(src), "-o", "{O}/out.png"])
    for algorithm in CORE_ALGORITHMS:
        add(f"corpus/tapestry/{algorithm}/pico8", ["apply", c("tapestry-4000.jpg"), "-a", algorithm, "-p", "pico8", "-o", "{O}/out.bmp"])
        add(f"corpus/tapestry/{algorithm}/bw", ["apply", c("tapestry-4000.jpg"), "-a", algorithm, "-p", "bw", "-o", "{O}/out.bmp"])
        add(f"corpus/rhinoceros/{algorithm}/auto16", ["apply", c("rhinoceros-2400.png"), "-a", algorithm, "--auto-palette", "16", "-o", "{O}/out.bmp"])
    add("corpus/tapestry/resize", ["apply", c("tapestry-4000.jpg"), "--width", "1500", "--sharpen", "0.8", "-o", "{O}/out.png"])

    # 5. generators
    for pattern in PATTERNS:
        add(f"generate/{pattern}/default", ["generate", pattern, "-o", "{O}/out.png"])
        add(f"generate/{pattern}/large-fs", ["generate", pattern, "--canvas-width", "1600", "--canvas-height", "1000", "-a", "floyd-steinberg", "-p", "pico8", "-o", "{O}/out.png"])
        add(f"generate/{pattern}/stops", ["generate", pattern, "--stops", "#0f172a,#38bdf8,#f8fafc", "--angle", "30", "--frequency", "2.5", "-a", "blue-noise", "--auto-palette", "6", "-o", "{O}/out.png"])
        add(f"generate/{pattern}/odd-size", ["generate", pattern, "--canvas-width", "333", "--canvas-height", "517", "-P", "gameboy", "-o", "{O}/out.png"])

    # 6. animation: procedural loops, photo boil, GIF and sequence input
    for pattern in PATTERNS:
        add(f"animate/{pattern}/gif", ["animate", pattern, "--frames", "12", "-o", "{O}/out.gif"])
    for label, flags in {
        "apng": ["-o", "{O}/out.png", "--anim-format", "apng"], "spritesheet": ["-o", "{O}/out.png", "--anim-format", "spritesheet"],
        "frames": ["--frames-dir", "{O}/frames"], "fs-pico8": ["-a", "floyd-steinberg", "-p", "pico8", "-o", "{O}/out.gif"],
        "riemersma": ["-a", "riemersma", "-p", "gameboy", "-o", "{O}/out.gif"], "boil": ["--temporal", "boil", "-o", "{O}/out.gif"],
        "grain": ["--temporal", "grain", "--boil-amount", "0.2", "-o", "{O}/out.gif"], "locked-bayer": ["--temporal", "locked", "-a", "bayer8", "-o", "{O}/out.gif"],
        "boil-rate": ["--temporal", "boil", "--boil-rate", "3", "-a", "halftone", "-o", "{O}/out.gif"], "drift": ["--screen-drift", "1,-1", "-a", "bayer4", "-o", "{O}/out.gif"],
        "easing": ["--easing", "bounce", "--cycles", "2", "--phase-offset", "0.25", "-o", "{O}/out.gif"], "ping-pong": ["--ping-pong", "--hold-start", "2", "--hold-end", "3", "--reverse", "-o", "{O}/out.gif"],
        "sweeps": ["--sweep-bias=-0.2..0.2", "--sweep-contrast", "0..0.4", "--sweep-strength", "0.2..1.2", "--sweep-mode", "pulse", "-o", "{O}/out.gif"],
        "sweep-pixel": ["--sweep-pixel-size", "1..6", "--sweep-gamma", "0.8..1.2", "--sweep-brightness=-0.1..0.1", "-o", "{O}/out.gif"],
        "cycle-palette": ["--cycle-palette", "-p", "pico8", "-o", "{O}/out.gif"], "auto-palette": ["--auto-palette", "8", "-o", "{O}/out.gif"],
        "per-frame-palette": ["--auto-palette", "6", "--per-frame-palette", "-o", "{O}/out.png", "--anim-format", "apng"],
        "preset-riso": ["-P", "risograph", "-o", "{O}/out.gif"], "preset-manga": ["-P", "manga", "-o", "{O}/out.gif"],
        "canvas": ["--canvas-width", "500", "--canvas-height", "300", "--motion-amplitude", "1.8", "-o", "{O}/out.gif"],
    }.items():
        add(f"animate/plasma/{label}", ["animate", "plasma", "--frames", "10", *flags])
        add(f"animate/sphere/{label}", ["animate", "--pattern", "sphere", "--frames", "7", "--fps", "12", *flags])
    for label, flags in {
        "boil-gif": ["--temporal", "boil", "-o", "{O}/out.gif"], "boil-apng": ["--temporal", "boil", "-o", "{O}/out.png", "--anim-format", "apng"],
        "grain-fs": ["--temporal", "grain", "-a", "floyd-steinberg", "-p", "pico8", "-o", "{O}/out.gif"], "locked": ["--temporal", "locked", "-o", "{O}/out.gif"],
        "auto-palette": ["--auto-palette", "12", "-a", "blue-noise", "-o", "{O}/out.gif"], "sweep": ["--sweep-contrast=-0.2..0.4", "--sweep-pixel-size", "1..4", "-o", "{O}/out.gif"],
        "preset": ["-P", "halftone-zine", "-o", "{O}/out.gif"], "spritesheet": ["--anim-format", "spritesheet", "--sheet-cols", "4", "-o", "{O}/out.png"],
        "resized": ["--width", "400", "--temporal", "boil", "-a", "atkinson", "-o", "{O}/out.gif"],
    }.items():
        add(f"animate/photo-wave/{label}", ["animate", c("wave-1200.png"), "--frames", "8", *flags])
        add(f"animate/photo-alpha/{label}", ["animate", c("alpha-1024.png"), "--frames", "6", *flags])
        add(f"animate/gif-input/{label}", ["animate", c("loop-320.gif"), *flags])
    add("animate/sequence/gif", ["animate", c("seq-01.png"), c("seq-02.png"), c("seq-03.png"), c("seq-04.png"), "-o", "{O}/out.gif"])
    add("animate/sequence/fs-apng", ["animate", c("seq-01.png"), c("seq-02.png"), c("seq-03.png"), c("seq-04.png"), "-a", "floyd-steinberg", "--auto-palette", "8", "--anim-format", "apng", "-o", "{O}/out.png"])
    add("animate/gif-input/max-frames", ["animate", c("loop-320.gif"), "--max-frames", "5", "--fps", "20", "--max-size", "40kb", "-o", "{O}/out.gif"])

    # 7. batch and compare
    everything = [c(n) for n in ("tapestry-4000.jpg", "pillars-3200.jpg", "wheatfield-3000.jpg", "rhinoceros-2400.png", "earth-2048.png", "tetons-1600.jpg", "wave-1200.png", "migrant-800.jpg", "alpha-1024.png")]
    add("batch/default", ["batch", *everything, "-d", "{O}/out"])
    add("batch/pico8-bmp", ["batch", *everything, "-d", "{O}/out", "-P", "pico8", "-f", "bmp"])
    add("batch/auto-palette-resize", ["batch", *everything, "-d", "{O}/out", "--auto-palette", "8", "--max-dim", "900", "-a", "atkinson", "--suffix=-x"])
    add("compare/default", ["compare", c("pillars-3200.jpg"), "-o", "{O}/sheet.png"])
    add("compare/algorithms", ["compare", c("wave-1200.png"), "--algorithms", ",".join(ALGORITHMS), "--tile-max-dim", "300", "-o", "{O}/sheet.png"])
    add("compare/presets", ["compare", c("wave-1200.png"), "--presets", ",".join(presets), "-o", "{O}/sheet.png", "--out-dir", "{O}/variants"])
    add("compare/palettes", ["compare", c("alpha-1024.png"), "--palettes", "bw,newsprint,risograph-blue,gameboy,pico8,c64,web216", "--light", "--columns", "3", "-o", "{O}/sheet.webp"])
    add("compare/pattern", ["compare", "--pattern", "mesh", "--algorithms", "floyd-steinberg,bayer8,blue-noise,riemersma", "-o", "{O}/sheet.png"])
    add("compare/cover", ["compare", c("wheatfield-3000.jpg"), "--algorithms", "atkinson,bayer4", "--width", "300", "--height", "300", "--fit", "cover", "-o", "{O}/sheet.png"])
    add("compare/contain", ["compare", c("wheatfield-3000.jpg"), "--algorithms", "atkinson,bayer4", "--width", "300", "--height", "300", "-o", "{O}/sheet.png"])
    add("compare/scale", ["compare", c("wheatfield-3000.jpg"), "--presets", "pico8,gameboy,manga", "--scale", "0.15", "-o", "{O}/sheet.png", "--out-dir", "{O}/variants"])
    add("compare/pixel-size", ["compare", c("wheatfield-3000.jpg"), "--palettes", "bw,pico8", "--pixel-size", "3", "--tile-max-dim", "500", "-o", "{O}/sheet.png"])
    add("compare/small-source", ["compare", c("seq-01.png"), "--algorithms", "floyd-steinberg,bayer8", "--tile-max-dim", "400", "-o", "{O}/sheet.png"])
    add("compare/flags", ["compare", c("tetons-1600.jpg"), "--algorithms", "atkinson,ostromoukhov,halftone", "-p", "gameboy", "--contrast", "0.2", "--tile-max-dim", "500", "-o", "{O}/sheet.png"])
    return cases


def run_case(binary, out_root, case, keep):
    name, template = case
    out_dir = os.path.join(out_root, "files", name)
    os.makedirs(out_dir, exist_ok=True)
    argv = [binary] + [a.replace("{O}", out_dir) for a in template]
    proc = subprocess.run(argv, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, check=False)
    entries = {}
    if proc.returncode != 0:
        entries[name + " (exit status)"] = f"{proc.returncode}: {proc.stderr.decode(errors='replace').strip()[:300]}"
    files = sorted(os.path.join(root, f) for root, _, names in os.walk(out_dir) for f in names)
    if proc.returncode == 0 and not files:
        entries[name + " (exit status)"] = "no output files"
    for path in files:
        with open(path, "rb") as handle:
            entries[os.path.relpath(path, os.path.join(out_root, "files"))] = hashlib.sha256(handle.read()).hexdigest()
    if not keep:
        shutil.rmtree(out_dir, ignore_errors=True)
    return entries


def cmd_render(args):
    binary = os.path.abspath(args.bin)
    corpus = os.path.abspath(args.corpus)
    out_root = os.path.abspath(args.out)
    shutil.rmtree(out_root, ignore_errors=True)
    os.makedirs(out_root)
    cases = build_cases(binary, corpus)
    if args.filter:
        cases = [case for case in cases if any(f in case[0] for f in args.filter.split(","))]
    manifest = {}
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
        for entries in pool.map(lambda case: run_case(binary, out_root, case, args.keep), cases):
            manifest.update(entries)
    failures = [k for k in manifest if k.endswith("(exit status)")]
    with open(os.path.join(out_root, "manifest.json"), "w", encoding="utf-8") as handle:
        json.dump(manifest, handle, indent=0, sort_keys=True)
    print(f"{len(cases)} cases, {len(manifest) - len(failures)} output files, {len(failures)} failed cases -> {out_root}/manifest.json")
    for key in failures[:20]:
        print(f"  FAILED {key}: {manifest[key]}")
    if failures and not args.allow_failures:
        sys.exit(1)


def cmd_diff(args):
    manifests = []
    for directory in (args.a, args.b):
        with open(os.path.join(directory, "manifest.json"), encoding="utf-8") as handle:
            manifests.append(json.load(handle))
    a, b = manifests
    differing = sorted(k for k in a.keys() & b.keys() if a[k] != b[k])
    missing = sorted(a.keys() ^ b.keys())
    print(f"{len(a)} vs {len(b)} outputs: {len(differing)} differ, {len(missing)} present on one side only")
    for key in (differing + missing)[:40]:
        print(f"  {key}")
    if differing or missing:
        sys.exit(1)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    render = sub.add_parser("render")
    render.add_argument("--bin", required=True)
    render.add_argument("--corpus", default="bench-corpus")
    render.add_argument("--out", required=True)
    render.add_argument("--jobs", type=int, default=os.cpu_count() or 1)
    render.add_argument("--filter", help="comma-separated substrings; run only matching cases")
    render.add_argument("--keep", action="store_true", help="keep the rendered files, not only their hashes")
    render.add_argument("--allow-failures", action="store_true")
    render.set_defaults(func=cmd_render)
    diff = sub.add_parser("diff")
    diff.add_argument("a")
    diff.add_argument("b")
    diff.set_defaults(func=cmd_diff)
    args = parser.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()

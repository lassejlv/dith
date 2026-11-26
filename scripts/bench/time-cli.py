#!/usr/bin/env python3
"""Time the dith CLI over the benchmark corpus and matrix.

    scripts/bench/time-cli.py run --bin target/release/dith --corpus bench-corpus --json after.json
    scripts/bench/time-cli.py diff before.json after.json

`run` executes every case RUNS times (default 5) and records the median wall clock and the peak
resident set size of the child process (ru_maxrss, the number `/usr/bin/time -v` reports).
`diff` prints a Markdown before/after table from two `run` results.

Most cases write BMP so the timing is the decode -> preprocess -> dither pipeline and not an
encoder; the cases whose name ends in the format (`-png`, `-gif`, `-apng`) are end to end.
"""

import argparse
import json
import os
import shutil
import statistics
import subprocess
import sys
import tempfile
import time

BIG = "{C}/tapestry-4000.jpg"

# name -> argv ({C} = corpus dir, {O} = scratch output dir)
CASES = {
    # apply on the 4000 px photo: error diffusion
    "apply-4000-fs-bw": ["apply", BIG, "-o", "{O}/o.bmp", "-a", "floyd-steinberg", "-p", "bw"],
    "apply-4000-fs-pico8": ["apply", BIG, "-o", "{O}/o.bmp", "-a", "floyd-steinberg", "-p", "pico8"],
    "apply-4000-fs-web216": ["apply", BIG, "-o", "{O}/o.bmp", "-a", "floyd-steinberg", "-p", "web216"],
    "apply-4000-fs-auto32": ["apply", BIG, "-o", "{O}/o.bmp", "-a", "floyd-steinberg", "--auto-palette", "32"],
    "apply-4000-fs-pico8-srgb": ["apply", BIG, "-o", "{O}/o.bmp", "-a", "floyd-steinberg", "-p", "pico8", "--color-space", "srgb"],
    "apply-4000-fs-pico8-linear": ["apply", BIG, "-o", "{O}/o.bmp", "-a", "floyd-steinberg", "-p", "pico8", "--color-space", "linear"],
    "apply-4000-atkinson-bw": ["apply", BIG, "-o", "{O}/o.bmp", "-a", "atkinson", "-p", "bw"],
    "apply-4000-atkinson-pico8": ["apply", BIG, "-o", "{O}/o.bmp", "-a", "atkinson", "-p", "pico8"],
    "apply-4000-ostromoukhov-bw": ["apply", BIG, "-o", "{O}/o.bmp", "-a", "ostromoukhov", "-p", "bw"],
    "apply-4000-ostromoukhov-pico8": ["apply", BIG, "-o", "{O}/o.bmp", "-a", "ostromoukhov", "-p", "pico8"],
    "apply-4000-jjn-pico8": ["apply", BIG, "-o", "{O}/o.bmp", "-a", "jarvis-judice-ninke", "-p", "pico8"],
    # ordered, blue noise, riemersma, threshold
    "apply-4000-bayer8-bw": ["apply", BIG, "-o", "{O}/o.bmp", "-a", "bayer8", "-p", "bw"],
    "apply-4000-bayer8-pico8": ["apply", BIG, "-o", "{O}/o.bmp", "-a", "bayer8", "-p", "pico8"],
    "apply-4000-halftone-bw": ["apply", BIG, "-o", "{O}/o.bmp", "-a", "halftone", "-p", "bw"],
    "apply-4000-halftone-web216": ["apply", BIG, "-o", "{O}/o.bmp", "-a", "halftone", "-p", "web216"],
    "apply-4000-bluenoise-pico8": ["apply", BIG, "-o", "{O}/o.bmp", "-a", "blue-noise", "-p", "pico8"],
    "apply-4000-bluenoise-auto16": ["apply", BIG, "-o", "{O}/o.bmp", "-a", "blue-noise", "--auto-palette", "16"],
    "apply-4000-riemersma-bw": ["apply", BIG, "-o", "{O}/o.bmp", "-a", "riemersma", "-p", "bw"],
    "apply-4000-riemersma-pico8": ["apply", BIG, "-o", "{O}/o.bmp", "-a", "riemersma", "-p", "pico8"],
    "apply-4000-threshold-web216": ["apply", BIG, "-o", "{O}/o.bmp", "-a", "threshold", "-p", "web216"],
    # preprocess and resize
    "apply-4000-resize-2000": ["apply", BIG, "-o", "{O}/o.bmp", "--width", "2000"],
    "apply-4000-tone": ["apply", BIG, "-o", "{O}/o.bmp", "--contrast", "0.2", "--gamma", "0.9", "--saturation", "1.2", "--auto-levels"],
    "apply-4000-sharpen-blur": ["apply", BIG, "-o", "{O}/o.bmp", "--sharpen", "1", "--blur", "1.5"],
    "apply-4000-pixel-size-4": ["apply", BIG, "-o", "{O}/o.bmp", "--pixel-size", "4", "-p", "gameboy"],
    "apply-4000-preset-manga": ["apply", BIG, "-o", "{O}/o.bmp", "-P", "manga"],
    "apply-4000-preset-risograph": ["apply", BIG, "-o", "{O}/o.bmp", "-P", "risograph"],
    # end to end with the default encoder
    "apply-4000-default-png": ["apply", BIG, "-o", "{O}/o.png"],
    "apply-2048-default-png": ["apply", "{C}/earth-2048.png", "-o", "{O}/o.png"],
    "apply-800-default-png": ["apply", "{C}/migrant-800.jpg", "-o", "{O}/o.png"],
    # generators
    "generate-plasma-3000": ["generate", "plasma", "-o", "{O}/g.bmp", "--canvas-width", "3000", "--canvas-height", "2000"],
    "generate-noise-3000": ["generate", "noise", "-o", "{O}/g.bmp", "--canvas-width", "3000", "--canvas-height", "2000"],
    "generate-sphere-3000": ["generate", "sphere", "-o", "{O}/g.bmp", "--canvas-width", "3000", "--canvas-height", "2000"],
    "generate-mesh-3000": ["generate", "mesh", "-o", "{O}/g.bmp", "--canvas-width", "3000", "--canvas-height", "2000"],
    # animation
    "animate-sphere-48f-gif": ["animate", "sphere", "-o", "{O}/a.gif", "--frames", "48"],
    "animate-plasma-48f-800-gif": ["animate", "plasma", "-o", "{O}/a.gif", "--frames", "48", "--canvas-width", "800", "--canvas-height", "600"],
    "animate-plasma-48f-fs-gif": ["animate", "plasma", "-o", "{O}/a.gif", "--frames", "48", "--canvas-width", "800", "--canvas-height", "600", "-a", "floyd-steinberg", "-p", "pico8"],
    "animate-boil-wave-24f-gif": ["animate", "{C}/wave-1200.png", "-o", "{O}/a.gif", "--frames", "24", "--temporal", "boil"],
    "animate-boil-wave-24f-apng": ["animate", "{C}/wave-1200.png", "-o", "{O}/a.png", "--anim-format", "apng", "--frames", "24", "--temporal", "boil"],
    "animate-boil-tetons-12f-riso-width-gif": ["animate", "{C}/tetons-1600.jpg", "-o", "{O}/a.gif", "--frames", "12", "-P", "risograph", "--width", "1000"],
    "animate-boil-wave-24f-auto16-gif": ["animate", "{C}/wave-1200.png", "-o", "{O}/a.gif", "--frames", "24", "--temporal", "boil", "--auto-palette", "16"],
    # batch and compare
    "batch-corpus-png": ["batch", "{C}/tapestry-4000.jpg", "{C}/pillars-3200.jpg", "{C}/wheatfield-3000.jpg", "{C}/rhinoceros-2400.png", "{C}/earth-2048.png", "{C}/tetons-1600.jpg", "{C}/wave-1200.png", "{C}/migrant-800.jpg", "{C}/alpha-1024.png", "-d", "{O}/batch"],
    "batch-corpus-pico8-bmp": ["batch", "{C}/tapestry-4000.jpg", "{C}/pillars-3200.jpg", "{C}/wheatfield-3000.jpg", "{C}/rhinoceros-2400.png", "{C}/earth-2048.png", "{C}/tetons-1600.jpg", "{C}/wave-1200.png", "{C}/migrant-800.jpg", "{C}/alpha-1024.png", "-d", "{O}/batch", "-p", "pico8", "--format", "bmp"],
    "compare-default-3200": ["compare", "{C}/pillars-3200.jpg", "-o", "{O}/c.png"],
    "compare-algorithms-800": ["compare", "{C}/pillars-3200.jpg", "-o", "{O}/c.png", "--algorithms", "floyd-steinberg,atkinson,ostromoukhov,bayer8,halftone,blue-noise,riemersma", "--tile-max-dim", "800"],
    "compare-presets-3200": ["compare", "{C}/pillars-3200.jpg", "-o", "{O}/c.png", "--presets", "mac-classic,editorial-print,risograph,film-stipple,gameboy,pico8"],
    # startup
    "startup-version": ["--version"],
    "startup-list-presets": ["list", "presets", "--json"],
}


def run_once(argv, env):
    start = time.perf_counter()
    pid = os.posix_spawn(argv[0], argv, env, file_actions=[
        (os.POSIX_SPAWN_OPEN, 1, os.devnull, os.O_WRONLY, 0),
        (os.POSIX_SPAWN_OPEN, 2, os.devnull, os.O_WRONLY, 0),
    ])
    _, status, usage = os.wait4(pid, 0)
    elapsed = time.perf_counter() - start
    if status != 0:
        sys.exit(f"case failed (status {status}): {' '.join(argv)}")
    return elapsed, usage.ru_maxrss


def cmd_run(args):
    binary = os.path.abspath(args.bin)
    corpus = os.path.abspath(args.corpus)
    scratch = tempfile.mkdtemp(prefix="dith-bench-")
    env = dict(os.environ)
    results = {}
    try:
        for name, template in CASES.items():
            if args.filter and not any(f in name for f in args.filter.split(",")):
                continue
            argv = [binary] + [a.replace("{C}", corpus).replace("{O}", scratch) for a in template]
            times, peak = [], 0
            for _ in range(args.runs):
                elapsed, rss = run_once(argv, env)
                times.append(elapsed)
                peak = max(peak, rss)
            results[name] = {"median_s": statistics.median(times), "min_s": min(times), "peak_rss_kb": peak}
            print(f"{name:36s} {results[name]['median_s'] * 1000:9.1f} ms  (min {min(times) * 1000:8.1f})  {peak / 1024:7.0f} MB", flush=True)
    finally:
        shutil.rmtree(scratch, ignore_errors=True)
    if args.json:
        with open(args.json, "w", encoding="utf-8") as handle:
            json.dump({"bin": binary, "runs": args.runs, "threads": os.environ.get("RAYON_NUM_THREADS", "default"), "cases": results}, handle, indent=1)


def cmd_diff(args):
    with open(args.before, encoding="utf-8") as handle:
        before = json.load(handle)["cases"]
    with open(args.after, encoding="utf-8") as handle:
        after = json.load(handle)["cases"]
    print("| case | before | after | speedup | peak RSS before | peak RSS after |")
    print("| --- | ---: | ---: | ---: | ---: | ---: |")
    for name, b in before.items():
        a = after.get(name)
        if a is None:
            continue
        print(f"| {name} | {b['median_s'] * 1000:.0f} ms | {a['median_s'] * 1000:.0f} ms | {b['median_s'] / a['median_s']:.2f}x | {b['peak_rss_kb'] / 1024:.0f} MB | {a['peak_rss_kb'] / 1024:.0f} MB |")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    run = sub.add_parser("run")
    run.add_argument("--bin", required=True)
    run.add_argument("--corpus", default="bench-corpus")
    run.add_argument("--runs", type=int, default=5)
    run.add_argument("--filter", help="comma-separated substrings; run only matching cases")
    run.add_argument("--json", help="write results to this file")
    run.set_defaults(func=cmd_run)
    diff = sub.add_parser("diff")
    diff.add_argument("before")
    diff.add_argument("after")
    diff.set_defaults(func=cmd_diff)
    args = parser.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()

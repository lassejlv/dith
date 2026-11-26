#!/usr/bin/env python3
"""Fetch the fixed public-domain benchmark corpus and resize it to the benchmark widths.

Usage: scripts/bench/fetch-corpus.py [DEST_DIR]   (default: ./bench-corpus, git-ignored)

Needs Python 3 with Pillow. Originals are cached in DEST_DIR/originals so reruns are offline.
Every source is public domain or CC0; see the SOURCES table for the URL and licence of each.
"""

import hashlib
import sys
import urllib.request
from pathlib import Path

from PIL import Image

Image.MAX_IMAGE_PIXELS = None
UA = "dith-bench/1.0 (https://github.com/lassejlv/diths)"

# name, output width, output format, mode, source URL, licence
SOURCES = [
    ("tapestry-4000.jpg", 4000, "JPEG", "RGB",
     "https://images.metmuseum.org/CRDImages/cl/original/DP118991.jpg",
     "CC0 (The Met Open Access, object 467642, The Unicorn Rests in a Garden)"),
    ("pillars-3200.jpg", 3200, "JPEG", "RGB",
     "https://upload.wikimedia.org/wikipedia/commons/6/68/Pillars_of_creation_2014_HST_WFC3-UVIS_full-res_denoised.jpg",
     "Public domain (NASA/ESA Hubble)"),
    ("wheatfield-3000.jpg", 3000, "JPEG", "RGB",
     "https://images.metmuseum.org/CRDImages/ep/original/DP-42549-001.jpg",
     "CC0 (The Met Open Access, object 436535, Van Gogh, Wheat Field with Cypresses)"),
    ("rhinoceros-2400.png", 2400, "PNG", "L",
     "https://upload.wikimedia.org/wikipedia/commons/3/3f/D%C3%BCrer%27s_Rhinoceros%2C_1515.jpg",
     "Public domain (Albrecht Duerer, 1515, woodcut; grayscale illustration)"),
    ("earth-2048.png", 2048, "PNG", "RGB",
     "https://upload.wikimedia.org/wikipedia/commons/9/97/The_Earth_seen_from_Apollo_17.jpg",
     "Public domain (NASA, Apollo 17)"),
    ("tetons-1600.jpg", 1600, "JPEG", "L",
     "https://upload.wikimedia.org/wikipedia/commons/2/21/Adams_The_Tetons_and_the_Snake_River.jpg",
     "Public domain (Ansel Adams, US National Archives)"),
    ("wave-1200.png", 1200, "PNG", "RGB",
     "https://images.metmuseum.org/CRDImages/as/original/DP130155.jpg",
     "CC0 (The Met Open Access, object 45434, Hokusai, Under the Wave off Kanagawa)"),
    ("migrant-800.jpg", 800, "JPEG", "L",
     "https://upload.wikimedia.org/wikipedia/commons/5/54/Lange-MigrantMother02.jpg",
     "Public domain (Dorothea Lange, US Farm Security Administration)"),
]


def fetch(url: str, dest: Path) -> None:
    if dest.exists():
        return
    request = urllib.request.Request(url, headers={"User-Agent": UA})
    with urllib.request.urlopen(request) as response:
        dest.write_bytes(response.read())


def main() -> None:
    root = Path(sys.argv[1] if len(sys.argv) > 1 else "bench-corpus")
    originals = root / "originals"
    originals.mkdir(parents=True, exist_ok=True)
    for name, width, fmt, mode, url, licence in SOURCES:
        original = originals / (hashlib.sha256(url.encode()).hexdigest()[:16] + ".jpg")
        fetch(url, original)
        out = root / name
        if not out.exists():
            img = Image.open(original).convert(mode)
            height = round(img.height * width / img.width)
            img = img.resize((width, height), Image.LANCZOS)
            if fmt == "JPEG":
                img.save(out, fmt, quality=92)
            else:
                img.save(out, fmt)
        digest = hashlib.sha256(out.read_bytes()).hexdigest()[:16]
        with Image.open(out) as img:
            print(f"{name}\t{img.width}x{img.height}\t{digest}\t{licence}\t{url}")

    # Synthetic RGBA fixture (soft radial alpha over the wave) for the alpha code paths.
    alpha_out = root / "alpha-1024.png"
    if not alpha_out.exists():
        base = Image.open(root / "wave-1200.png").convert("RGB").resize((1024, 704), Image.LANCZOS)
        mask = Image.radial_gradient("L").resize(base.size, Image.BILINEAR).point(lambda v: 255 - v)
        mask = mask.point(lambda v: 0 if v < 40 else min(255, (v - 40) * 2))
        base.putalpha(mask)
        base.save(alpha_out, "PNG")
    print(f"alpha-1024.png\t1024x704\t{hashlib.sha256(alpha_out.read_bytes()).hexdigest()[:16]}\tsynthetic (derived from wave-1200.png)\t-")

    # Synthetic animation inputs: a 12-frame GIF and a 4-frame PNG sequence panned across the wave.
    wave = Image.open(root / "wave-1200.png").convert("RGB")
    loop_out = root / "loop-320.gif"
    if not loop_out.exists():
        frames = [wave.crop((40 * i, 60, 40 * i + 640, 540)).resize((320, 240), Image.LANCZOS) for i in range(12)]
        frames = [f.quantize(colors=64, method=Image.MEDIANCUT, dither=Image.NONE) for f in frames]
        frames[0].save(loop_out, save_all=True, append_images=frames[1:], duration=80, loop=0)
    print(f"loop-320.gif\t320x240\t{hashlib.sha256(loop_out.read_bytes()).hexdigest()[:16]}\tsynthetic (derived from wave-1200.png)\t-")
    for i in range(4):
        seq_out = root / f"seq-{i + 1:02d}.png"
        if not seq_out.exists():
            wave.crop((120 * i, 100, 120 * i + 600, 500)).resize((300, 200), Image.LANCZOS).save(seq_out, "PNG")
        print(f"{seq_out.name}\t300x200\t{hashlib.sha256(seq_out.read_bytes()).hexdigest()[:16]}\tsynthetic (derived from wave-1200.png)\t-")


if __name__ == "__main__":
    main()

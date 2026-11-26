# Cat icon, made with dith alone

A 512×512 cat-head icon and its blink, built from hand-written SVGs and dith commands only: no
image editor, no script, no image generator. `build.sh` holds the exact commands, and the test
`crates/dith-cli/tests/cat_icon.rs` runs it on every CI build.

```sh
sh build.sh                                # needs dith on PATH; writes ./out
DITH=../../target/release/dith sh build.sh # or point at a local build
```

## The files

| File | Role | How dith uses it |
| --- | --- | --- |
| `body.svg` | Head (ellipse at 0, 0.10, rx 0.66, ry 0.54) and two rounded-triangle ears, each its own element | `--shape`: inflated from its distance field, merged with `--blend 24`, lit from the top left (`--angle 135`) with `--ground-shadow` |
| `blush.svg` | Two blurred, semi-transparent ellipses on the cheeks | `--paint`: darkens the lit surface, so the blush is dithered with the body |
| `face.svg` | Outline, nose and "w" mouth | `--overlay`: exact ink pixels on top of the dither |
| `eyes-open.svg` | Oval eyes with two highlights each | `--overlay` on the icon, frame 1 of `--overlay-frames` |
| `eyes-half.svg`, `eyes-closed.svg` | Half-closed eyes; closed eyes as small downward arcs | `--overlay-frames` of the blink |
| `blink.json` | `[4200, 50, 100, 50]` | `--timings`: open 4.2 s, half 50 ms, closed 100 ms, half 50 ms (a 4.4 s loop in 20 fps steps) |

All SVGs share `viewBox="-1 -1 2 2"` at 512×512, so overlays line up with the shape.

## The look

`--algorithm atkinson --colors '#F5F1EA,#171717' --pixel-size 2 --bias 0.12 --no-auto-levels`

## Why the blink stays still

The eyes are overlays, so the dithered body underneath never changes, and `--temporal locked-diff`
copies every pixel that did not change in the source from the previous frame. `build.sh` ends with
`dith diff --frames out/cat-blink.gif --loop --json`: its `union.changed_columns` are two runs, one
per eye, and `union.changed_rows` is one run around the eyes. Nothing else ever changes, so the GIF
stores the full icon once and then only the eyes (about 7 KB in total).

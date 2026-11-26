//! The cat-icon example (examples/cat-icon/build.sh) as an acceptance test: the icon and its blink
//! are built with dith commands and hand-written SVGs only, and nothing outside the eyes moves.

use std::path::PathBuf;
use std::process::Command;

use image::AnimationDecoder;
use image::codecs::gif::GifDecoder;
use serde_json::Value;

/// The eye boxes in the 512px icon (eyes at x = ±0.25, y = 0.04 in -1..1 units, 0.075 x 0.10
/// radii), plus 4px of slack for the 2px pixel grid and the lids.
const EYE_COLUMNS: [[u64; 2]; 2] = [[164, 220], [292, 348]];
const EYE_ROWS: [u64; 2] = [236, 296];

#[test]
fn cat_icon_example_builds_with_dith_alone_and_only_the_eyes_blink() {
    let example = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/cat-icon");
    let out = tempfile::TempDir::new().unwrap();
    let status = Command::new("sh")
        .arg(example.join("build.sh"))
        .env("DITH", env!("CARGO_BIN_EXE_dith"))
        .env("OUT", out.path())
        .status()
        .unwrap();
    assert!(status.success(), "build.sh failed");

    let cat = image::open(out.path().join("cat.png")).unwrap().to_rgba8();
    assert_eq!(cat.dimensions(), (512, 512));
    assert!(
        cat.pixels()
            .all(|p| p.0 == [0xF5, 0xF1, 0xEA, 255] || p.0 == [0x17, 0x17, 0x17, 255]),
        "two colours only"
    );
    for (x, y) in [(192, 266), (320, 266)] {
        assert_eq!(
            cat.get_pixel(x, y).0,
            [0x17, 0x17, 0x17, 255],
            "ink eye at {x},{y}"
        );
    }

    let gif = std::fs::File::open(out.path().join("cat-blink.gif")).unwrap();
    let frames: Vec<_> = GifDecoder::new(std::io::BufReader::new(gif))
        .unwrap()
        .into_frames()
        .collect_frames()
        .unwrap();
    let delays: Vec<u32> = frames
        .iter()
        .map(|f| {
            let (n, d) = f.delay().numer_denom_ms();
            n / d
        })
        .collect();
    assert_eq!(
        delays,
        vec![4200, 50, 100, 50],
        "4.4 s loop at 20 fps steps"
    );
    assert_eq!(frames[0].buffer(), &cat, "the open-eyed frame is the icon");
    assert!(
        std::fs::metadata(out.path().join("cat-blink.gif"))
            .unwrap()
            .len()
            < 16_000,
        "only the eyes are stored after the first frame"
    );

    let report: Value =
        serde_json::from_slice(&std::fs::read(out.path().join("blink-diff.json")).unwrap())
            .unwrap();
    assert_eq!(report["frame_count"], 4);
    let union = &report["union"];
    assert!(
        union["changed_pixels"].as_u64().unwrap() > 0,
        "the eyes do blink"
    );
    for run in union["changed_columns"].as_array().unwrap() {
        let (a, b) = (run[0].as_u64().unwrap(), run[1].as_u64().unwrap());
        assert!(
            EYE_COLUMNS.iter().any(|[lo, hi]| *lo <= a && b <= *hi),
            "column run {a}-{b} is outside both eyes"
        );
    }
    for run in union["changed_rows"].as_array().unwrap() {
        let (a, b) = (run[0].as_u64().unwrap(), run[1].as_u64().unwrap());
        assert!(
            EYE_ROWS[0] <= a && b <= EYE_ROWS[1],
            "row run {a}-{b} is outside the eyes"
        );
    }
}

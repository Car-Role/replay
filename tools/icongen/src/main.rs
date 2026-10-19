//! Renders `assets/icon.svg` and `assets/icon-recording.svg` into:
//! - `assets/generated/replay.ico` and `replay-recording.ico` (Windows exe and tray),
//! - `assets/generated/png/icon_<size>.png` (macOS iconset, Linux hicolor),
//! - `assets/generated/icon-64.rgba` and `icon-recording-64.rgba` (window and tray icons, raw RGBA).
//!
//! Run from the repo root: `cargo run -p icongen --release`.

use std::fs;
use std::path::Path;

use resvg::{tiny_skia, usvg};

fn render(svg: &[u8], size: u32) -> tiny_skia::Pixmap {
    let tree = usvg::Tree::from_data(svg, &usvg::Options::default()).expect("valid svg");
    let mut pix = tiny_skia::Pixmap::new(size, size).expect("pixmap");
    let scale = size as f32 / tree.size().width();
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pix.as_mut());
    pix
}

/// Straight (not premultiplied) RGBA, which ICO and PNG expect.
fn straight_rgba(p: &tiny_skia::Pixmap) -> Vec<u8> {
    p.pixels()
        .iter()
        .flat_map(|c| {
            let c = c.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect()
}

fn write_ico(svg: &[u8], path: &Path) {
    let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
    for size in [16, 20, 24, 32, 40, 48, 64, 128, 256] {
        let pix = render(svg, size);
        let img = ico::IconImage::from_rgba_data(size, size, straight_rgba(&pix));
        dir.add_entry(ico::IconDirEntry::encode(&img).expect("encode"));
    }
    dir.write(fs::File::create(path).expect("create ico")).expect("write ico");
}

fn main() {
    let out = Path::new("assets/generated");
    fs::create_dir_all(out.join("png")).expect("mkdir");
    let main_svg = fs::read("assets/icon.svg").expect("assets/icon.svg (run from the repo root)");
    let rec_svg = fs::read("assets/icon-recording.svg").expect("assets/icon-recording.svg");

    write_ico(&main_svg, &out.join("replay.ico"));
    write_ico(&rec_svg, &out.join("replay-recording.ico"));
    for size in [16, 32, 48, 64, 128, 256, 512, 1024] {
        render(&main_svg, size).save_png(out.join("png").join(format!("icon_{size}.png"))).expect("png");
    }
    fs::write(out.join("icon-64.rgba"), straight_rgba(&render(&main_svg, 64))).expect("rgba");
    fs::write(out.join("icon-recording-64.rgba"), straight_rgba(&render(&rec_svg, 64))).expect("rgba");
    println!("icons written to {}", out.display());
}

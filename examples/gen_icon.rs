// Regenerate assets/icon.png from the procedural design used by `make_icon()`.
//
// Run when you change the icon shape:
//   cargo run --example gen_icon --release
//
// Writes a 1024×1024 PNG used by the Dioxus bundler for .dmg / .deb / .msi
// branding (Finder, Dock, Add/Remove Programs, etc.). The runtime window
// icon is generated separately at 64×64 by `make_icon()` in src/main.rs.

use image::{ImageBuffer, Rgba};

const SIZE: u32 = 1024;

fn main() {
    let mut img: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::new(SIZE, SIZE);
    for y in 0..SIZE {
        for x in 0..SIZE {
            // Circular mask
            let cx = x as f32 - SIZE as f32 / 2.0 + 0.5;
            let cy = y as f32 - SIZE as f32 / 2.0 + 0.5;
            let r_sq = cx * cx + cy * cy;
            let radius = SIZE as f32 / 2.0;
            let alpha = if r_sq > radius * radius { 0u8 } else { 255u8 };

            // Gradient: blue → purple
            let t = (x + y) as f32 / (SIZE * 2) as f32;
            let r = (0.0_f32 + t * 120.0) as u8;
            let g = (120.0 - t * 60.0) as u8;
            let b = (212.0 - t * 20.0) as u8;

            let (r, g, b) = if is_screens_shape(x, y, SIZE) {
                (255u8, 255u8, 255u8)
            } else {
                (r, g, b)
            };

            img.put_pixel(x, y, Rgba([r, g, b, alpha]));
        }
    }
    img.save("assets/icon.png").expect("write icon.png");
    println!("Wrote assets/icon.png ({}×{})", SIZE, SIZE);
}

fn is_screens_shape(x: u32, y: u32, size: u32) -> bool {
    let s = size as f32;
    let fx = x as f32;
    let fy = y as f32;
    let phone = |cx: f32, cy: f32, w: f32, h: f32, corner: f32| -> bool {
        let left   = cx - w / 2.0;
        let right  = cx + w / 2.0;
        let top    = cy - h / 2.0;
        let bottom = cy + h / 2.0;
        if fx < left || fx > right || fy < top || fy > bottom { return false; }
        let dx = if fx < left + corner { left + corner - fx }
                 else if fx > right - corner { fx - (right - corner) }
                 else { 0.0 };
        let dy = if fy < top + corner { top + corner - fy }
                 else if fy > bottom - corner { fy - (bottom - corner) }
                 else { 0.0 };
        dx * dx + dy * dy <= corner * corner
    };
    if phone(s * 0.42, s * 0.45, s * 0.32, s * 0.48, s * 0.05) { return true; }
    if phone(s * 0.58, s * 0.55, s * 0.32, s * 0.48, s * 0.05) { return true; }
    false
}

use std::io::Write;
use std::path::Path;

const ICO_SIZES: &[usize] = &[16, 32, 48, 64, 128, 256];

mod config {
    pub const DEFAULT_CONFIG_NAME: &str = "\u{9ed8}\u{8ba4}";
}

#[allow(dead_code)]
mod runtime_icon {
    include!("src/icon.rs");
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=src/icon.rs");

    let out_dir = std::env::var("OUT_DIR")?;
    let ico_path = Path::new(&out_dir).join("app.ico");

    generate_ico(&ico_path)?;

    let mut res = winres::WindowsResource::new();
    res.set_icon(ico_path.to_string_lossy().as_ref());
    res.compile()?;
    Ok(())
}

fn generate_ico(path: &Path) -> std::io::Result<()> {
    let images: Vec<(usize, Vec<u8>)> = ICO_SIZES
        .iter()
        .copied()
        .map(|size| {
            let rgba = runtime_icon::render_icon_rgba_at(
                size,
                false,
                config::DEFAULT_CONFIG_NAME,
            );
            (size, encode_bmp_icon_image(size, rgba))
        })
        .collect();

    let mut ico = Vec::new();
    ico.write_all(&0u16.to_le_bytes())?;
    ico.write_all(&1u16.to_le_bytes())?;
    ico.write_all(&(images.len() as u16).to_le_bytes())?;

    let mut offset = 6 + images.len() * 16;
    for (size, data) in &images {
        ico.push(if *size >= 256 { 0 } else { *size as u8 });
        ico.push(if *size >= 256 { 0 } else { *size as u8 });
        ico.push(0u8);
        ico.push(0u8);
        ico.write_all(&1u16.to_le_bytes())?;
        ico.write_all(&32u16.to_le_bytes())?;
        ico.write_all(&(data.len() as u32).to_le_bytes())?;
        ico.write_all(&(offset as u32).to_le_bytes())?;
        offset += data.len();
    }

    for (_, data) in images {
        ico.write_all(&data)?;
    }

    std::fs::write(path, ico)
}

fn encode_bmp_icon_image(size: usize, mut rgba: Vec<u8>) -> Vec<u8> {
    for chunk in rgba.chunks_exact_mut(4) {
        chunk.swap(0, 2);
    }

    let bmp_header_size = 40u32;
    let and_mask_row_size = size.div_ceil(32) * 4;
    let and_mask_size = (and_mask_row_size * size) as u32;
    let pixel_data_size = (size * size * 4) as u32;
    let mut data = Vec::with_capacity((bmp_header_size + pixel_data_size + and_mask_size) as usize);

    data.write_all(&bmp_header_size.to_le_bytes()).unwrap();
    data.write_all(&(size as i32).to_le_bytes()).unwrap();
    data.write_all(&((size as i32) * 2).to_le_bytes()).unwrap();
    data.write_all(&1u16.to_le_bytes()).unwrap();
    data.write_all(&32u16.to_le_bytes()).unwrap();
    data.write_all(&0u32.to_le_bytes()).unwrap();
    data.write_all(&pixel_data_size.to_le_bytes()).unwrap();
    data.write_all(&0u32.to_le_bytes()).unwrap();
    data.write_all(&0u32.to_le_bytes()).unwrap();
    data.write_all(&0u32.to_le_bytes()).unwrap();
    data.write_all(&0u32.to_le_bytes()).unwrap();

    for y in (0..size).rev() {
        let row_start = y * size * 4;
        data.write_all(&rgba[row_start..row_start + size * 4])
            .unwrap();
    }

    let and_mask = vec![0u8; and_mask_size as usize];
    data.write_all(&and_mask).unwrap();
    data
}

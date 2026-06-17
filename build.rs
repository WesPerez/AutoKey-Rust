use std::io::Write;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = std::env::var("OUT_DIR")?;
    let ico_path = Path::new(&out_dir).join("app.ico");

    // Generate a 32x32 ICO file with sky blue background and red center circle
    if !ico_path.exists() {
        generate_ico(&ico_path)?;
    }

    // Embed the icon into the EXE using winres
    let mut res = winres::WindowsResource::new();
    res.set_icon(ico_path.to_string_lossy().as_ref());
    res.compile()?;
    Ok(())
}

fn generate_ico(path: &Path) -> std::io::Result<()> {
    const SIZE: usize = 32;
    let bg_color: [u8; 4] = [21, 101, 192, 255];
    let accent: [u8; 4] = [211, 47, 47, 255];
    let white: [u8; 4] = [255, 255, 255, 255];

    let mut rgba = vec![0u8; SIZE * SIZE * 4];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let offset = (y * SIZE + x) * 4;
            let dx = x as f32 - 15.5;
            let dy = y as f32 - 15.5;
            let distance = (dx * dx + dy * dy).sqrt();
            let color = if distance <= 14.0 {
                bg_color
            } else if distance <= 15.5 {
                let alpha = ((15.5 - distance) / 1.5 * 255.0) as u8;
                [bg_color[0], bg_color[1], bg_color[2], alpha]
            } else {
                [0, 0, 0, 0]
            };
            rgba[offset..offset + 4].copy_from_slice(&color);
        }
    }
    for y in 0..SIZE {
        for x in 0..SIZE {
            let offset = (y * SIZE + x) * 4;
            let dx = x as f32 - 15.5;
            let dy = y as f32 - 15.5;
            let distance = (dx * dx + dy * dy).sqrt();
            if distance <= 8.0 {
                rgba[offset..offset + 4].copy_from_slice(&accent);
            } else if distance <= 9.5 {
                let alpha = ((9.5 - distance) / 1.5 * 255.0) as u8;
                let blended = [white[0], white[1], white[2], alpha.min(rgba[offset + 3])];
                rgba[offset..offset + 4].copy_from_slice(&blended);
            }
        }
    }

    // Convert RGBA to BGRA for BMP
    let mut bgra = rgba;
    for chunk in bgra.chunks_exact_mut(4) {
        chunk.swap(0, 2);
    }

    let mut ico = Vec::new();
    ico.write_all(&0u16.to_le_bytes())?;
    ico.write_all(&1u16.to_le_bytes())?;
    ico.write_all(&1u16.to_le_bytes())?;

    ico.push(SIZE as u8);
    ico.push(SIZE as u8);
    ico.push(0u8);
    ico.push(0u8);
    ico.write_all(&1u16.to_le_bytes())?;
    ico.write_all(&32u16.to_le_bytes())?;

    let bmp_header_size = 40u32;
    let and_mask_row_size = SIZE.div_ceil(32) * 4;
    let and_mask_size = (and_mask_row_size * SIZE) as u32;
    let pixel_data_size = (SIZE * SIZE * 4) as u32;
    let data_size = bmp_header_size + pixel_data_size + and_mask_size;
    ico.write_all(&data_size.to_le_bytes())?;
    ico.write_all(&((6 + 16) as u32).to_le_bytes())?;

    ico.write_all(&bmp_header_size.to_le_bytes())?;
    ico.write_all(&(SIZE as i32).to_le_bytes())?;
    ico.write_all(&(SIZE as i32 * 2).to_le_bytes())?;
    ico.write_all(&1u16.to_le_bytes())?;
    ico.write_all(&32u16.to_le_bytes())?;
    ico.write_all(&0u32.to_le_bytes())?;
    ico.write_all(&pixel_data_size.to_le_bytes())?;
    ico.write_all(&0u32.to_le_bytes())?;
    ico.write_all(&0u32.to_le_bytes())?;
    ico.write_all(&0u32.to_le_bytes())?;
    ico.write_all(&0u32.to_le_bytes())?;

    for y in (0..SIZE).rev() {
        let row_start = y * SIZE * 4;
        ico.write_all(&bgra[row_start..row_start + SIZE * 4])?;
    }

    let and_mask = vec![0u8; and_mask_size as usize];
    ico.write_all(&and_mask)?;

    std::fs::write(path, ico)
}

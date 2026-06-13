use std::io::Write;

/// Generate a 32x32 ICO file with a sky blue background and status circle.
/// This creates the same icon as the tray icon but in .ico format for the EXE.
pub fn generate_ico(is_running: bool) -> Vec<u8> {
    const SIZE: usize = 32;
    let bg_color: [u8; 4] = [21, 101, 192, 255];
    let accent: [u8; 4] = if is_running {
        [76, 175, 80, 255]
    } else {
        [211, 47, 47, 255]
    };
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

    // Convert RGBA to BGRA for BMP (ICO uses BMP format)
    let mut bgra = rgba.clone();
    for chunk in bgra.chunks_exact_mut(4) {
        chunk.swap(0, 2); // R <-> B
    }

    // Build ICO file
    let mut ico = Vec::new();
    // ICO header
    ico.write_all(&0u16.to_le_bytes()).unwrap();      // Reserved
    ico.write_all(&1u16.to_le_bytes()).unwrap();      // Type: ICO
    ico.write_all(&1u16.to_le_bytes()).unwrap();      // Count: 1 image

    // Image directory entry
    ico.push(SIZE as u8);                              // Width
    ico.push(SIZE as u8);                              // Height
    ico.push(0u8);                                     // Color palette
    ico.push(0u8);                                     // Reserved
    ico.write_all(&1u16.to_le_bytes()).unwrap();       // Color planes
    ico.write_all(&32u16.to_le_bytes()).unwrap();      // Bits per pixel

    // BMP data: BITMAPINFOHEADER + pixels + AND mask
    let bmp_header_size = 40u32;
    let and_mask_row_size = ((SIZE + 31) / 32) * 4;
    let and_mask_size = (and_mask_row_size * SIZE) as u32;
    let pixel_data_size = (SIZE * SIZE * 4) as u32;
    let data_size = bmp_header_size + pixel_data_size + and_mask_size;
    ico.write_all(&data_size.to_le_bytes()).unwrap();
    let data_offset = 6 + 16; // header(6) + directory(16)
    ico.write_all(&(data_offset as u32).to_le_bytes()).unwrap();

    // BITMAPINFOHEADER
    ico.write_all(&bmp_header_size.to_le_bytes()).unwrap();
    ico.write_all(&(SIZE as i32).to_le_bytes()).unwrap();    // Width
    ico.write_all(&(SIZE as i32 * 2).to_le_bytes()).unwrap(); // Height (doubled for ICO)
    ico.write_all(&1u16.to_le_bytes()).unwrap();              // Planes
    ico.write_all(&32u16.to_le_bytes()).unwrap();             // BPP
    ico.write_all(&0u32.to_le_bytes()).unwrap();              // Compression
    ico.write_all(&pixel_data_size.to_le_bytes()).unwrap();   // Image size
    ico.write_all(&0u32.to_le_bytes()).unwrap();              // X ppm
    ico.write_all(&0u32.to_le_bytes()).unwrap();              // Y ppm
    ico.write_all(&0u32.to_le_bytes()).unwrap();              // Colors used
    ico.write_all(&0u32.to_le_bytes()).unwrap();              // Important colors

    // Pixel data (bottom-up BMP)
    for y in (0..SIZE).rev() {
        let row_start = y * SIZE * 4;
        ico.write_all(&bgra[row_start..row_start + SIZE * 4]).unwrap();
    }

    // AND mask (all zeros = fully opaque)
    let and_mask = vec![0u8; and_mask_size as usize];
    ico.write_all(&and_mask).unwrap();

    ico
}

/// Write the default (stopped state) ICO to a file path.
pub fn write_default_ico(path: &std::path::Path) -> std::io::Result<()> {
    let data = generate_ico(false);
    std::fs::write(path, data)
}

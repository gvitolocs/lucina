//! Reads the crash screen back from a QEMU screen dump (a binary PPM), line by line, for the test scripts.
//!
//! usage: read-screen <dump.ppm>

use lucina_crash::screen::{HEADLINE_ROWS, Layout, read_row};

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: read-screen <dump.ppm>");
    let data = std::fs::read(&path).expect("read the dump");
    let (width, height, pixels) = parse_ppm(&data).expect("a binary PPM with 8-bit samples");
    let pixel = |x: usize, y: usize| {
        let p = &pixels[(y * width + x) * 3..];
        u32::from(p[0]) << 16 | u32::from(p[1]) << 8 | u32::from(p[2])
    };
    let layout = Layout::new(width, height);
    let mut row_text = vec![0u8; layout.columns];
    // The headline, read as one line: two lines of the large font, or, for a long one, four of the normal font.
    let mut headline = String::new();
    for (big, step) in [(2, 2), (1, 1)] {
        headline.clear();
        for row in (0..HEADLINE_ROWS).step_by(step) {
            let len = read_row(pixel, &layout, row, big, &mut row_text);
            headline.push_str(&String::from_utf8_lossy(&row_text[..len]));
        }
        if !headline.contains('~') {
            break;
        }
    }
    println!("{headline}");
    for row in HEADLINE_ROWS..layout.rows {
        let len = read_row(pixel, &layout, row, 1, &mut row_text);
        println!("{}", String::from_utf8_lossy(&row_text[..len]));
    }
}

fn parse_ppm(data: &[u8]) -> Option<(usize, usize, &[u8])> {
    let mut at = 0;
    let mut token = || -> Option<&[u8]> {
        while data.get(at)?.is_ascii_whitespace() {
            at += 1;
        }
        let start = at;
        while !data.get(at)?.is_ascii_whitespace() {
            at += 1;
        }
        Some(&data[start..at])
    };
    if token()? != b"P6" {
        return None;
    }
    let number = |t: &[u8]| std::str::from_utf8(t).ok()?.parse::<usize>().ok();
    let width = number(token()?)?;
    let height = number(token()?)?;
    if number(token()?)? != 255 {
        return None;
    }
    let pixels = data.get(at + 1..)?;
    (pixels.len() >= width * height * 3).then_some((width, height, pixels))
}

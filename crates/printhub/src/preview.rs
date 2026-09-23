//! A picture of a job, drawn from its G-code: the OrcaSlicer CLI renders no thumbnail on 2.4.2,
//! so every extruding move is drawn as seen from above the bed's front-left corner.

use std::{
    f32::consts::{FRAC_1_SQRT_2, FRAC_PI_6},
    fs::File,
    io::{self, BufRead, BufReader},
    path::{Path, PathBuf},
};

use tokio::sync::Mutex;

use crate::jobs;

/// Side of the square PNG, in pixels.
pub const SIZE: usize = 256;
/// Drawn at this multiple of `SIZE` and averaged down, which smooths the edges.
const SUPERSAMPLE: usize = 2;
/// A little wider than a 0.4 mm line, so neighbouring layers close up into a surface.
const LINE_MM: f32 = 0.5;
/// How far the camera looks down.
const ELEVATION: f32 = FRAC_PI_6;
/// Towards the light: above the bed, in front of it and to the left.
const LIGHT: [f32; 3] = [-0.3, -0.6, 0.75];
const FALLBACK_COLOR: [u8; 3] = [0x9a, 0xa0, 0xa6];

/// One render at a time: each holds a whole file's moves, and a queue page asks for many.
static RENDERING: Mutex<()> = Mutex::const_new(());

/// Draws a job's preview in the background as soon as its G-code is in place, so the first page
/// to show it does not wait. A large file takes about a second.
pub fn prepare(data_dir: PathBuf, id: i64) {
    tokio::spawn(async move {
        if let Err(err) = for_job(&data_dir, id).await {
            tracing::warn!(job = id, %err, "drawing the preview failed");
        }
    });
}

/// A job's preview, drawn from its G-code and kept next to it the first time it is asked for.
/// `None` while the G-code is not there, or when it draws nothing.
pub async fn for_job(data_dir: &Path, id: i64) -> io::Result<Option<Vec<u8>>> {
    let gcode = jobs::gcode_path(data_dir, id);
    let target = jobs::preview_path(data_dir, id);
    if let Ok(png) = tokio::fs::read(&target).await {
        return Ok(Some(png));
    }
    let _rendering = RENDERING.lock().await;
    // Another request may have drawn it while this one waited.
    if let Ok(png) = tokio::fs::read(&target).await {
        return Ok(Some(png));
    }
    let png = match tokio::task::spawn_blocking(move || render(&gcode)).await? {
        Ok(Some(png)) => png,
        Ok(None) => return Ok(None),
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let partial = target.with_extension("png.part");
    tokio::fs::write(&partial, &png).await?;
    tokio::fs::rename(&partial, &target).await?;
    Ok(Some(png))
}

/// Holds every move while it frames and draws them: parsing is most of the time, so reading the
/// file once is worth the memory (tens of MB for a large file).
pub fn render(gcode: &Path) -> io::Result<Option<Vec<u8>>> {
    let side = SIZE * SUPERSAMPLE;
    let mut lines = Vec::new();
    let colors = walk(BufReader::new(File::open(gcode)?), |line| {
        lines.push(line.clone())
    })?;

    let mut low = [f32::INFINITY; 2];
    let mut high = [f32::NEG_INFINITY; 2];
    for line in &lines {
        for end in [line.from, line.to] {
            let point = project(end);
            for axis in 0..2 {
                low[axis] = low[axis].min(point[axis]);
                high[axis] = high[axis].max(point[axis]);
            }
        }
    }
    if lines.is_empty() {
        return Ok(None);
    }
    let extent = [high[0] - low[0], high[1] - low[1]];
    let scale = side as f32 * 0.9 / extent[0].max(extent[1]).max(1.0);
    let margin = [
        (side as f32 - extent[0] * scale) / 2.0,
        (side as f32 - extent[1] * scale) / 2.0,
    ];
    let to_pixel = |point: [f32; 3]| {
        let [across, up, depth] = project(point);
        [
            (across - low[0]) * scale + margin[0],
            (high[1] - up) * scale + margin[1],
            depth,
        ]
    };

    let mut depth = vec![f32::INFINITY; side * side];
    let mut color = vec![[0u8; 3]; side * side];
    let width = (LINE_MM * scale).ceil().max(1.0);
    let half = width / 2.0;
    for line in &lines {
        let base = colors
            .get(usize::from(line.tool))
            .copied()
            .unwrap_or(FALLBACK_COLOR);
        let rgb = shade(base, normal(line));
        let (a, b) = (to_pixel(line.from), to_pixel(line.to));
        let length = (b[0] - a[0]).abs().max((b[1] - a[1]).abs());
        // Squares half a line apart overlap, so the line has no gaps.
        let steps = (length / half.max(1.0)).ceil().max(1.0) as usize;
        for step in 0..=steps {
            let t = step as f32 / steps as f32;
            let [x, y, z] = [0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * t);
            let clamp = |v: f32| (v.round().max(0.0) as usize).min(side);
            for py in clamp(y - half)..clamp(y + half) {
                for px in clamp(x - half)..clamp(x + half) {
                    let at = py * side + px;
                    if z < depth[at] {
                        depth[at] = z;
                        color[at] = rgb;
                    }
                }
            }
        }
    }

    let mut rgba = Vec::with_capacity(SIZE * SIZE * 4);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (mut sum, mut covered) = ([0u32; 3], 0u32);
            for sy in 0..SUPERSAMPLE {
                for sx in 0..SUPERSAMPLE {
                    let at = (y * SUPERSAMPLE + sy) * side + x * SUPERSAMPLE + sx;
                    if depth[at].is_finite() {
                        covered += 1;
                        for (total, channel) in sum.iter_mut().zip(color[at]) {
                            *total += u32::from(channel);
                        }
                    }
                }
            }
            let samples = (SUPERSAMPLE * SUPERSAMPLE) as u32;
            let average = sum.map(|total| (total / covered.max(1)) as u8);
            rgba.extend_from_slice(&average);
            rgba.push((covered * 255 / samples) as u8);
        }
    }

    let mut png = Vec::new();
    let mut encoder = png::Encoder::new(&mut png, SIZE as u32, SIZE as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .and_then(|mut writer| {
            writer.write_image_data(&rgba)?;
            writer.finish()
        })
        .map_err(io::Error::other)?;
    Ok(Some(png))
}

/// `[across, up, depth]`: across the screen to the right, up the screen, and away from the
/// camera, all in millimetres.
fn project([x, y, z]: [f32; 3]) -> [f32; 3] {
    let across = (x - y) * FRAC_1_SQRT_2;
    let along = (x + y) * FRAC_1_SQRT_2;
    let (sin, cos) = ELEVATION.sin_cos();
    [across, along * sin + z * cos, along * cos - z * sin]
}

/// Walls face sideways, across the direction they were printed in; everything else is taken to
/// face up.
fn normal(line: &Line) -> [f32; 3] {
    let (dx, dy) = (line.to[0] - line.from[0], line.to[1] - line.from[1]);
    let length = dx.hypot(dy);
    if !line.wall || length == 0.0 {
        return [0.0, 0.0, 1.0];
    }
    let sideways = [-dy / length, dx / length];
    // Lit on whichever side faces the camera, since the print direction does not say.
    let sign = if sideways[0] + sideways[1] > 0.0 {
        -1.0
    } else {
        1.0
    };
    [sideways[0] * sign, sideways[1] * sign, 0.0]
}

fn shade(base: [u8; 3], normal: [f32; 3]) -> [u8; 3] {
    let length = LIGHT.iter().map(|v| v * v).sum::<f32>().sqrt();
    let lambert = normal
        .iter()
        .zip(LIGHT)
        .map(|(n, l)| n * l / length)
        .sum::<f32>()
        .max(0.0);
    let brightness = 0.3 + 0.7 * lambert;
    base.map(|channel| (f32::from(channel) * brightness) as u8)
}

#[derive(Clone)]
struct Line {
    from: [f32; 3],
    to: [f32; 3],
    tool: u8,
    wall: bool,
}

/// Calls `each` for every extruding move of the model itself: not the start and end G-code
/// (`;TYPE:Custom`, which holds the purge line), nor supports, which would hide it. Returns the
/// file's filament colours by tool.
// ponytail: arcs (G2/G3) are drawn as their chord; interpolate them if arc fitting gets used.
fn walk(mut file: impl BufRead, mut each: impl FnMut(&Line)) -> io::Result<Vec<[u8; 3]>> {
    let mut at = [0.0f32; 3];
    let mut extruder = 0.0f32;
    let (mut relative, mut relative_e) = (false, false);
    let (mut hidden, mut wall, mut tool) = (false, false, 0u8);
    let mut colors = Vec::new();
    // Bytes in one reused buffer: a `String` per line was a quarter of the time on large files.
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        if file.read_until(b'\n', &mut buffer)? == 0 {
            break;
        }
        let line = buffer.trim_ascii();
        if let Some(kind) = line.strip_prefix(b";TYPE:") {
            hidden = kind == b"Custom" || kind.starts_with(b"Support");
            wall = kind.windows(4).any(|word| word == b"wall");
            continue;
        }
        if let Some(list) = line.strip_prefix(b"; filament_colour = ") {
            colors = String::from_utf8_lossy(list)
                .split(';')
                .map(parse_color)
                .collect();
            continue;
        }
        let code = line.split(|&byte| byte == b';').next().unwrap_or_default();
        let mut words = code
            .split(u8::is_ascii_whitespace)
            .filter(|word| !word.is_empty());
        let Some(command) = words.next() else {
            continue;
        };
        let values = words.filter_map(|word| {
            let (axis, number) = word.split_first()?;
            Some((*axis, parse_number(number)?))
        });
        match command {
            b"G90" => relative = false,
            b"G91" => relative = true,
            b"M82" => relative_e = false,
            b"M83" => relative_e = true,
            b"G92" => {
                for (axis, value) in values {
                    match axis {
                        b'X' => at[0] = value,
                        b'Y' => at[1] = value,
                        b'Z' => at[2] = value,
                        b'E' => extruder = value,
                        _ => {}
                    }
                }
            }
            b"G0" | b"G1" | b"G2" | b"G3" => {
                let mut to = at;
                let mut extruded = 0.0;
                for (axis, value) in values {
                    let index = match axis {
                        b'X' => 0,
                        b'Y' => 1,
                        b'Z' => 2,
                        b'E' => {
                            extruded = if relative_e { value } else { value - extruder };
                            extruder += extruded;
                            continue;
                        }
                        _ => continue,
                    };
                    to[index] = if relative { at[index] + value } else { value };
                }
                let moved = to[0] != at[0] || to[1] != at[1];
                if command != b"G0" && extruded > 0.0 && moved && !hidden {
                    each(&Line {
                        from: at,
                        to,
                        tool,
                        wall,
                    });
                }
                at = to;
            }
            _ => {
                if let Some(index) = command.strip_prefix(b"T").and_then(parse_number) {
                    tool = index as u8;
                }
            }
        }
    }
    Ok(colors)
}

/// G-code numbers are plain decimals such as `-12.345`, which this reads several times faster
/// than `str::parse`, the other half of a large file's parse time.
fn parse_number(text: &[u8]) -> Option<f32> {
    let (sign, digits) = match text.split_first()? {
        (b'-', rest) => (-1.0, rest),
        _ => (1.0, text),
    };
    let (mut mantissa, mut divisor, mut point, mut count) = (0u64, 1u64, false, 0);
    for &byte in digits {
        match byte {
            // More digits than a u64 holds is not G-code a slicer writes.
            b'0'..=b'9' if count < 18 => {
                mantissa = mantissa * 10 + u64::from(byte - b'0');
                count += 1;
                if point {
                    divisor *= 10;
                }
            }
            b'.' if !point => point = true,
            _ => return None,
        }
    }
    (count > 0).then(|| (sign * mantissa as f64 / divisor as f64) as f32)
}

fn parse_color(raw: &str) -> [u8; 3] {
    let hex = raw.trim().trim_start_matches('#');
    let channel = |i: usize| {
        hex.get(i..i + 2)
            .and_then(|pair| u8::from_str_radix(pair, 16).ok())
    };
    match [channel(0), channel(2), channel(4)] {
        [Some(r), Some(g), Some(b)] => [r, g, b],
        _ => FALLBACK_COLOR,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(gcode: &str) -> (Vec<Line>, Vec<[u8; 3]>) {
        let mut found = Vec::new();
        let colors = walk(gcode.as_bytes(), |line| found.push(line.clone())).unwrap();
        (found, colors)
    }

    #[test]
    fn only_extruding_moves_of_the_print_are_drawn() {
        let (found, colors) = lines(
            "\
;TYPE:Custom
G90
M83
G1 X0 Y0 Z0.2 F9000
G1 X100 E5 ; purge line
;TYPE:Outer wall
G0 X10 Y10
G1 X20 Y10 E0.5
G1 E-0.8 ; retract
G1 X20 Y20 E0
;TYPE:Support interface
G1 X40 Y40 E1
;TYPE:Support
G1 X20 Y20 E1
T1
;TYPE:Sparse infill
G91
G1 X5 Y5 E0.2
M82
G92 E10
G1 X5 E10.5
G1 X5 E10.5
; filament_colour = #F2754E;#2850DF
",
        );
        let summary: Vec<_> = found
            .iter()
            .map(|line| (line.from, line.to, line.tool, line.wall))
            .collect();
        assert_eq!(
            summary,
            [
                ([10.0, 10.0, 0.2], [20.0, 10.0, 0.2], 0, true),
                ([20.0, 20.0, 0.2], [25.0, 25.0, 0.2], 1, false),
                ([25.0, 25.0, 0.2], [30.0, 25.0, 0.2], 1, false),
            ],
            "absolute E after M82 extrudes only when it grows"
        );
        assert_eq!(colors, [[0xF2, 0x75, 0x4E], [0x28, 0x50, 0xDF]]);
    }

    #[test]
    fn numbers_read_like_str_parse() {
        for text in ["10", "-0.8", "-.8", "123.4567", "0.", "7.000001", "250"] {
            assert_eq!(parse_number(text.as_bytes()), text.parse().ok(), "{text}");
        }
        for text in ["", "-", ".", "1e3", "1.2.3", "+1", "12x"] {
            assert_eq!(parse_number(text.as_bytes()), None, "{text}");
        }
    }

    #[test]
    fn a_sliced_cube_is_drawn_in_its_filament_colour() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/orca-2.4.2-cc2-pla-cube.gcode");
        let png = render(&fixture).unwrap().unwrap();
        let mut reader = png::Decoder::new(io::Cursor::new(png)).read_info().unwrap();
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut pixels).unwrap();
        assert_eq!((info.width, info.height), (SIZE as u32, SIZE as u32));
        let pixel = |x: usize, y: usize| &pixels[(y * SIZE + x) * 4..][..4];
        assert_eq!(pixel(0, 0)[3], 0, "the background is transparent");
        let centre = pixel(SIZE / 2, SIZE / 2);
        assert_eq!(centre[3], 255);
        assert!(centre[0] > centre[2], "orange PLA: {centre:?}");
    }

    #[test]
    fn nothing_printed_draws_nothing() {
        let dir = std::env::temp_dir().join(format!("printhub-preview-{}", std::process::id()));
        std::fs::write(&dir, "G28\nG1 X10 Y10\n").unwrap();
        let rendered = render(&dir).unwrap();
        std::fs::remove_file(&dir).unwrap();
        assert!(rendered.is_none());
    }
}

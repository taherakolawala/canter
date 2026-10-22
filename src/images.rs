//! Inline images in assignment, page and announcement text.
//!
//! The text converter in `html` turns each `<img>` into an `[image: alt][n]`
//! mark, where `n` is the image's place in the page's files & links list. The
//! file itself is already in the library, downloaded when the page was opened.
//! This module finds those marks, reads the local file, and builds a box of
//! text cells to draw under the mark.
//!
//! Cells are drawn with the upper half block `▀`: the foreground colour is the
//! top pixel of the cell, the background colour the bottom one, so one cell
//! carries two pixels and a row of cells is two pixel rows. That needs only
//! truecolor, which every terminal this reader targets has, and it looks
//! deliberately low-resolution.
//!
//! Sharper protocols (sixel, kitty graphics) are detected but not drawn; see
//! `protocol` and `docs/architecture.md` for why.
//!
//! The setting `TUICANVAS_IMAGES=off` turns images off, which leaves the
//! `[n]` marks exactly as they were before this module existed. `NO_COLOR`
//! turns them off too, since half blocks are nothing but colour.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::html::{Link, Target};

/// Upper half block. The top half takes the foreground colour, the bottom the
/// background colour.
const UPPER_HALF: char = '\u{2580}';

/// Environment variable that turns images off.
pub const SETTING_ENV: &str = "TUICANVAS_IMAGES";
/// Environment variable that forces a protocol, for trying one out.
pub const PROTOCOL_ENV: &str = "TUICANVAS_IMAGE_PROTOCOL";

/// Widest box, in text cells, whatever the page width.
const MAX_COLS: u16 = 72;
/// Tallest box, in text cells. A box this tall is 40 pixel rows.
const MAX_ROWS: u16 = 20;
/// Refuse files larger than this without opening them.
const MAX_FILE_BYTES: u64 = 24 * 1024 * 1024;
/// Refuse images whose header declares more pixels than this.
const MAX_PIXELS: u64 = 16_000_000;
/// Bytes the decoder may allocate for one image.
const MAX_ALLOC: u64 = 96 * 1024 * 1024;
/// All the images on one page share this much decoding time. Images that do
/// not fit in it keep their `[n]` mark.
const DECODE_BUDGET: Duration = Duration::from_millis(400);

/// Whether images are drawn at all.
pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        !crate::theme::plain() && setting_on(std::env::var(SETTING_ENV).ok().as_deref())
    })
}

/// Read the setting: anything but an explicit "off" leaves images on.
pub fn setting_on(value: Option<&str>) -> bool {
    !matches!(
        value.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("off" | "0" | "no" | "false" | "none")
    )
}

/// How a terminal can draw an image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    /// Half blocks: text cells, so always available.
    HalfBlock,
    /// The sixel protocol, which recent Windows Terminal versions support.
    Sixel,
    /// The kitty graphics protocol.
    Kitty,
}

impl Protocol {
    pub fn label(self) -> &'static str {
        match self {
            Protocol::HalfBlock => "half blocks",
            Protocol::Sixel => "sixel",
            Protocol::Kitty => "kitty graphics",
        }
    }
}

/// One line for the help screen, so the setting and what the terminal was
/// found to support are visible without reading the docs.
pub fn help_line() -> String {
    if !enabled() {
        return format!("Images are off ({SETTING_ENV}); text shows [n] marks instead.");
    }
    let found = protocol();
    if found == Protocol::HalfBlock {
        "Images in text are drawn as half blocks.".to_string()
    } else {
        format!(
            "Images in text are drawn as half blocks; this terminal also reports {}.",
            found.label()
        )
    }
}

static DETECTED: OnceLock<Protocol> = OnceLock::new();

/// What the terminal can do. `HalfBlock` until `detect` has run, so the
/// headless dump and the tests never depend on a terminal being there.
pub fn protocol() -> Protocol {
    *DETECTED.get().unwrap_or(&Protocol::HalfBlock)
}

/// Ask the terminal what it supports, once, and remember the answer.
///
/// Called from the runtime after the terminal is in raw mode, because the
/// reply arrives on standard input as ordinary input bytes. Everything else
/// reads the remembered answer through `protocol`.
pub fn detect() {
    let _ = DETECTED.set(detect_now());
}

fn detect_now() -> Protocol {
    if let Some(forced) = forced_protocol(std::env::var(PROTOCOL_ENV).ok().as_deref()) {
        return forced;
    }
    if !enabled() {
        return Protocol::HalfBlock;
    }
    let kitty = std::env::var_os("KITTY_WINDOW_ID").is_some()
        || std::env::var("TERM").is_ok_and(|t| t.contains("kitty"));
    choose(primary_device_attributes().as_deref(), kitty)
}

/// Read a forced protocol from the setting. `auto` and anything unrecognised
/// mean "detect".
pub fn forced_protocol(value: Option<&str>) -> Option<Protocol> {
    match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        Some("halfblock" | "half-block" | "half" | "blocks") => Some(Protocol::HalfBlock),
        Some("sixel") => Some(Protocol::Sixel),
        Some("kitty") => Some(Protocol::Kitty),
        _ => None,
    }
}

/// Pick a protocol from a primary device attributes reply and whether the
/// environment looks like kitty. Kept separate from the terminal I/O so it can
/// be tested.
pub fn choose(device_attributes: Option<&str>, kitty_env: bool) -> Protocol {
    if kitty_env {
        return Protocol::Kitty;
    }
    if device_attributes.is_some_and(reports_sixel) {
        return Protocol::Sixel;
    }
    Protocol::HalfBlock
}

/// A primary device attributes reply is `ESC [ ? 62 ; 4 ; 6 c`. Attribute 4 is
/// sixel support.
pub fn reports_sixel(reply: &str) -> bool {
    let Some(body) = reply
        .split_once("\u{1b}[?")
        .map(|(_, rest)| rest)
        .and_then(|rest| rest.split_once('c'))
        .map(|(body, _)| body)
    else {
        return false;
    };
    body.split(';').any(|a| a.trim() == "4")
}

/// Longest wait for a terminal that does not answer the query at all. A
/// terminal that does answer is not waited for: the sentinel ends the read.
const QUERY_TIMEOUT: Duration = Duration::from_millis(250);
/// Device status report, which every terminal answers with `ESC [ 0 n`. Sent
/// after the attributes query so a terminal that ignores the query still ends
/// the read, instead of costing the whole timeout.
const SENTINEL: &[u8] = b"\x1b[0n";

/// Ask the terminal for its primary device attributes and read the reply.
/// Returns `None` if there is no terminal or it does not answer.
fn primary_device_attributes() -> Option<String> {
    use std::io::{IsTerminal, Read, Write};

    let mut out = std::io::stdout();
    if !out.is_terminal() || !std::io::stdin().is_terminal() {
        return None;
    }
    out.write_all(b"\x1b[c\x1b[5n").ok()?;
    out.flush().ok()?;
    // Raw mode is on, so the replies arrive as input bytes and are not echoed.
    let deadline = Instant::now() + QUERY_TIMEOUT;
    let mut reply: Vec<u8> = Vec::new();
    let mut stdin = std::io::stdin();
    let mut byte = [0u8; 1];
    while Instant::now() < deadline && reply.len() < 128 {
        match stdin.read(&mut byte) {
            Ok(1) => reply.push(byte[0]),
            _ => break,
        }
        if reply.ends_with(SENTINEL) {
            break;
        }
    }
    String::from_utf8(reply).ok().filter(|r| r.contains("[?"))
}

/// A page's text with its image boxes. `lines` is what the text converter
/// produced, with blank lines inserted to make room for each box; `boxes` maps
/// a line index to the cells drawn on that line.
#[derive(Default)]
pub struct Body {
    pub lines: Vec<String>,
    pub boxes: HashMap<usize, Line<'static>>,
}

impl Body {
    pub fn plain(lines: Vec<String>) -> Self {
        Body {
            lines,
            boxes: HashMap::new(),
        }
    }

    /// The line to draw at `index`: an image row, or nothing (the caller
    /// styles the text itself).
    pub fn image_row(&self, index: usize) -> Option<Line<'static>> {
        self.boxes.get(&index).cloned()
    }
}

/// An image decoded and scaled to the pixel size of its box.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    /// Row-major RGB, `width * height` entries.
    pub rgb: Vec<[u8; 3]>,
}

impl Pixels {
    fn at(&self, x: u32, y: u32) -> [u8; 3] {
        let i = (y as usize) * (self.width as usize) + (x as usize);
        self.rgb.get(i).copied().unwrap_or([0, 0, 0])
    }
}

/// The box, in text cells, for an image of `width` by `height` pixels.
///
/// A cell holds one pixel across and two down, so a box of `(cols, rows)`
/// cells is `cols` by `2 * rows` pixels. The box keeps the image's aspect
/// ratio and is never enlarged past one pixel per half cell.
pub fn fit(width: u32, height: u32, max_cols: u16, max_rows: u16) -> (u16, u16) {
    if width == 0 || height == 0 || max_cols == 0 || max_rows == 0 {
        return (0, 0);
    }
    let (w, h) = (u64::from(width), u64::from(height));
    let (limit_cols, limit_rows) = (u64::from(max_cols), u64::from(max_rows));
    let cols = limit_cols.min(w).max(1);
    let rows = (cols * h).div_ceil(2 * w).max(1);
    if rows <= limit_rows {
        return (cols as u16, rows as u16);
    }
    let rows = limit_rows;
    let cols = (rows * 2 * w).div_ceil(h).clamp(1, limit_cols);
    (cols as u16, rows as u16)
}

/// Turn scaled pixels into one line of half-block cells per cell row.
///
/// `pixels` is expected to be `cols` by `2 * rows`; a short last pixel row
/// repeats the row above it so the bottom cells are not black.
pub fn half_block_rows(pixels: &Pixels) -> Vec<Line<'static>> {
    let rows = (pixels.height as usize).div_ceil(2);
    (0..rows)
        .map(|row| {
            let top = (row as u32) * 2;
            let bottom = (top + 1).min(pixels.height.saturating_sub(1));
            let spans: Vec<Span<'static>> = (0..pixels.width)
                .map(|x| {
                    let [tr, tg, tb] = pixels.at(x, top);
                    let [br, bg, bb] = pixels.at(x, bottom);
                    Span::styled(
                        UPPER_HALF.to_string(),
                        Style::new()
                            .fg(Color::Rgb(tr, tg, tb))
                            .bg(Color::Rgb(br, bg, bb)),
                    )
                })
                .collect();
            Line::from(spans)
        })
        .collect()
}

/// Read an image file and scale it to `(cols, 2 * rows)` pixels.
///
/// Returns `None` for anything that is not a readable image, is larger than
/// the caps, or fails to decode. Nothing here can panic on a malformed file:
/// the decoder's own limits bound how much it reads and allocates, and every
/// error becomes `None`.
pub fn decode(path: &Path, max_cols: u16, max_rows: u16) -> Option<Pixels> {
    let size = std::fs::metadata(path).ok()?.len();
    if size == 0 || size > MAX_FILE_BYTES {
        return None;
    }
    let (width, height) = dimensions(path)?;
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return None;
    }
    let (cols, rows) = fit(width, height, max_cols, max_rows);
    if cols == 0 || rows == 0 {
        return None;
    }
    let image = reader(path)?.decode().ok()?;
    let scaled = image
        .resize_exact(
            u32::from(cols),
            u32::from(rows) * 2,
            image::imageops::FilterType::Triangle,
        )
        .to_rgb8();
    Some(Pixels {
        width: scaled.width(),
        height: scaled.height(),
        rgb: scaled.pixels().map(|p| p.0).collect(),
    })
}

// `image::Limits` is marked non-exhaustive, so its fields can only be set on
// an existing value, not in a struct literal.
#[allow(clippy::field_reassign_with_default)]
fn reader(path: &Path) -> Option<image::ImageReader<std::io::BufReader<std::fs::File>>> {
    let mut reader = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_PIXELS as u32);
    limits.max_image_height = Some(MAX_PIXELS as u32);
    limits.max_alloc = Some(MAX_ALLOC);
    reader.limits(limits);
    Some(reader)
}

/// Pixel size from the file's header, without decoding it.
fn dimensions(path: &Path) -> Option<(u32, u32)> {
    reader(path)?.into_dimensions().ok()
}

/// Place a box under every `[image: ...][n]` mark whose file is in the library.
///
/// `lines` is the converted text, `links` the page's files & links list (the
/// `[n]` marks are one-based positions in it). Marks whose file is not
/// downloaded, is not an image, or fails to decode are left as they are.
pub fn place(
    lines: Vec<String>,
    links: &[Link],
    library: Option<&crate::library::Library>,
    width: u16,
) -> Body {
    if !enabled() || width == 0 {
        return Body::plain(lines);
    }
    let Some(library) = library else {
        return Body::plain(lines);
    };
    let max_cols = width.min(MAX_COLS);
    let started = Instant::now();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut boxes: HashMap<usize, Line<'static>> = HashMap::new();
    let mut done: Vec<usize> = Vec::new();
    for line in lines {
        let numbers = mark_numbers(&line);
        out.push(line);
        for n in numbers {
            if done.contains(&n) {
                continue;
            }
            let Some(id) = image_file_id(links, n) else {
                continue;
            };
            done.push(n);
            if started.elapsed() > DECODE_BUDGET {
                continue;
            }
            let Some(path) = local_path(library, &id) else {
                continue;
            };
            let Some(pixels) = decode(&path, max_cols, MAX_ROWS) else {
                continue;
            };
            for row in half_block_rows(&pixels) {
                boxes.insert(out.len(), row);
                out.push(String::new());
            }
        }
    }
    Body { lines: out, boxes }
}

/// The library's copy of a file, if one is on disk.
fn local_path(library: &crate::library::Library, file_id: &str) -> Option<PathBuf> {
    library
        .entry(file_id)?
        .paths
        .into_iter()
        .find(|p| p.is_file())
}

/// The Canvas file id behind the `[n]` mark, if entry `n` is an embedded image.
fn image_file_id(links: &[Link], n: usize) -> Option<String> {
    let link = links.get(n.checked_sub(1)?)?;
    match (&link.target, link.image) {
        (Target::File { id }, true) => Some(id.clone()),
        _ => None,
    }
}

/// The link numbers in `][n]` marks on one line, in order.
///
/// The number alone decides whether a box is drawn, because the caller checks
/// it against the links list; an alt text long enough to wrap does not hide
/// the mark, which always ends on one line.
pub fn mark_numbers(line: &str) -> Vec<usize> {
    let mut found = Vec::new();
    let bytes = line.as_bytes();
    let mut at = 0;
    while let Some(pos) = line[at..].find("][") {
        let start = at + pos + 2;
        let digits = bytes[start..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        at = start;
        if digits == 0 || bytes.get(start + digits) != Some(&b']') {
            continue;
        }
        if let Ok(n) = line[start..start + digits].parse::<usize>() {
            found.push(n);
        }
        at = start + digits + 1;
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(image: bool, id: &str) -> Link {
        Link {
            text: "chart".into(),
            url: format!("https://ufl.instructure.com/courses/5/files/{id}/preview"),
            target: Target::File { id: id.into() },
            image,
        }
    }

    /// A `width` by `height` PNG whose left half is red and right half is blue.
    fn two_tone_png(width: u32, height: u32) -> Vec<u8> {
        let mut buffer = image::RgbImage::new(width, height);
        for (x, _y, pixel) in buffer.enumerate_pixels_mut() {
            *pixel = if x < width / 2 {
                image::Rgb([220, 30, 30])
            } else {
                image::Rgb([30, 30, 220])
            };
        }
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(buffer)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .expect("encode");
        bytes.into_inner()
    }

    fn temp_file(tag: &str, bytes: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tuicanvas-images-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(tag);
        std::fs::write(&path, bytes).expect("write");
        path
    }

    #[test]
    fn the_setting_only_turns_images_off_when_it_says_off() {
        assert!(setting_on(None));
        assert!(setting_on(Some("")));
        assert!(setting_on(Some("on")));
        assert!(setting_on(Some("1")));
        assert!(!setting_on(Some("off")));
        assert!(!setting_on(Some(" OFF ")));
        assert!(!setting_on(Some("0")));
        assert!(!setting_on(Some("no")));
        assert!(!setting_on(Some("false")));
    }

    #[test]
    fn a_forced_protocol_overrides_detection() {
        assert_eq!(forced_protocol(Some("sixel")), Some(Protocol::Sixel));
        assert_eq!(forced_protocol(Some("Kitty")), Some(Protocol::Kitty));
        assert_eq!(
            forced_protocol(Some("half-block")),
            Some(Protocol::HalfBlock)
        );
        assert_eq!(forced_protocol(Some("auto")), None);
        assert_eq!(forced_protocol(None), None);
    }

    #[test]
    fn sixel_is_read_from_the_device_attributes_reply() {
        assert!(reports_sixel("\u{1b}[?62;4;6;9;22c"));
        assert!(reports_sixel("\u{1b}[?4c"));
        assert!(!reports_sixel("\u{1b}[?62;1;6;9;22c"));
        // Attribute 4 must be its own attribute, not part of 14 or 44.
        assert!(!reports_sixel("\u{1b}[?62;14;44c"));
        assert!(!reports_sixel("garbage"));
        assert!(!reports_sixel("\u{1b}[?62;4"));
    }

    #[test]
    fn detection_prefers_kitty_then_sixel_then_half_blocks() {
        assert_eq!(choose(Some("\u{1b}[?62;4c"), true), Protocol::Kitty);
        assert_eq!(choose(None, true), Protocol::Kitty);
        assert_eq!(choose(Some("\u{1b}[?62;4c"), false), Protocol::Sixel);
        assert_eq!(choose(Some("\u{1b}[?62;1c"), false), Protocol::HalfBlock);
        assert_eq!(choose(None, false), Protocol::HalfBlock);
    }

    #[test]
    fn protocol_is_half_blocks_until_detection_has_run() {
        assert_eq!(protocol(), Protocol::HalfBlock);
    }

    #[test]
    fn a_box_keeps_the_aspect_ratio_inside_its_limits() {
        // A cell is one pixel wide and two tall, so a square image is twice as
        // wide in cells as it is tall.
        assert_eq!(fit(40, 40, 80, 40), (40, 20));
        // Wider than the limit: the width decides.
        assert_eq!(fit(400, 200, 80, 40), (80, 20));
        // Taller than the limit: the height decides and the width follows.
        // 2 cells across and 10 down is 2 by 20 pixels, the image's 1:10.
        assert_eq!(fit(100, 1000, 80, 10), (2, 10));
        // Never enlarged past one pixel per half cell.
        assert_eq!(fit(10, 10, 80, 40), (10, 5));
        assert_eq!(fit(0, 10, 80, 40), (0, 0));
        assert_eq!(fit(10, 0, 80, 40), (0, 0));
        assert_eq!(fit(10, 10, 0, 40), (0, 0));
        // A very wide strip still gets at least one row.
        assert_eq!(fit(1000, 1, 80, 40), (80, 1));
    }

    #[test]
    fn a_box_fits_the_width_it_is_given() {
        for width in [1u32, 7, 64, 333, 2048] {
            for height in [1u32, 9, 100, 1600] {
                let (cols, rows) = fit(width, height, 60, 20);
                assert!(cols <= 60 && rows <= 20, "{width}x{height}");
                assert!(cols >= 1 && rows >= 1, "{width}x{height}");
            }
        }
    }

    #[test]
    fn cells_carry_two_pixel_rows_each() {
        let pixels = Pixels {
            width: 2,
            height: 4,
            rgb: vec![
                [1, 1, 1],
                [2, 2, 2],
                [3, 3, 3],
                [4, 4, 4],
                [5, 5, 5],
                [6, 6, 6],
                [7, 7, 7],
                [8, 8, 8],
            ],
        };
        let rows = half_block_rows(&pixels);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].spans.len(), 2);
        assert_eq!(rows[0].spans[0].content.as_ref(), "\u{2580}");
        assert_eq!(rows[0].spans[0].style.fg, Some(Color::Rgb(1, 1, 1)));
        assert_eq!(rows[0].spans[0].style.bg, Some(Color::Rgb(3, 3, 3)));
        assert_eq!(rows[1].spans[1].style.fg, Some(Color::Rgb(6, 6, 6)));
        assert_eq!(rows[1].spans[1].style.bg, Some(Color::Rgb(8, 8, 8)));
    }

    #[test]
    fn an_odd_pixel_height_repeats_its_last_row() {
        let pixels = Pixels {
            width: 1,
            height: 1,
            rgb: vec![[9, 9, 9]],
        };
        let rows = half_block_rows(&pixels);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].spans[0].style.fg, Some(Color::Rgb(9, 9, 9)));
        assert_eq!(rows[0].spans[0].style.bg, Some(Color::Rgb(9, 9, 9)));
    }

    #[test]
    fn decoding_scales_to_the_box_and_keeps_the_colours() {
        let path = temp_file("two-tone.png", &two_tone_png(64, 32));
        let pixels = decode(&path, 16, 20).expect("decodes");
        let (cols, rows) = fit(64, 32, 16, 20);
        assert_eq!(
            (pixels.width, pixels.height),
            (u32::from(cols), u32::from(rows) * 2)
        );
        // The left edge stays red and the right edge blue.
        let left = pixels.at(0, 0);
        let right = pixels.at(pixels.width - 1, 0);
        assert!(left[0] > left[2], "left is red: {left:?}");
        assert!(right[2] > right[0], "right is blue: {right:?}");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn malformed_and_oversized_files_are_skipped() {
        let truncated = two_tone_png(32, 32)[..40].to_vec();
        let path = temp_file("truncated.png", &truncated);
        assert!(decode(&path, 20, 10).is_none());
        let _ = std::fs::remove_file(&path);

        let path = temp_file("not-an-image.png", b"this is plain text, not a PNG");
        assert!(decode(&path, 20, 10).is_none());
        let _ = std::fs::remove_file(&path);

        let path = temp_file("empty.png", b"");
        assert!(decode(&path, 20, 10).is_none());
        let _ = std::fs::remove_file(&path);

        assert!(decode(Path::new("no-such-file-anywhere.png"), 20, 10).is_none());
    }

    #[test]
    fn a_png_header_claiming_a_huge_image_is_refused() {
        // A valid PNG signature and IHDR declaring 60000 by 60000 pixels, which
        // is past the pixel cap, so no decoding is attempted.
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        let mut ihdr: Vec<u8> = Vec::new();
        ihdr.extend_from_slice(b"IHDR");
        ihdr.extend_from_slice(&60_000u32.to_be_bytes());
        ihdr.extend_from_slice(&60_000u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        bytes.extend_from_slice(&13u32.to_be_bytes());
        bytes.extend_from_slice(&ihdr);
        bytes.extend_from_slice(&0u32.to_be_bytes()); // wrong CRC; never reached
        let path = temp_file("huge.png", &bytes);
        assert!(decode(&path, 20, 10).is_none());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn marks_are_read_off_a_line() {
        assert_eq!(mark_numbers("[image: Speedup chart][3]"), vec![3]);
        assert_eq!(
            mark_numbers("see [the slides][2] and [image: plot][11]."),
            vec![2, 11]
        );
        assert_eq!(mark_numbers("no marks here"), Vec::<usize>::new());
        assert_eq!(mark_numbers("[a][] [b][x] [c][4"), Vec::<usize>::new());
        // A bracketed literal that is not a mark.
        assert_eq!(mark_numbers("array[i][0] is set"), vec![0]);
    }

    #[test]
    fn only_embedded_images_get_a_box() {
        let links = vec![link(false, "22"), link(true, "33")];
        assert_eq!(image_file_id(&links, 2).as_deref(), Some("33"));
        assert!(image_file_id(&links, 1).is_none());
        assert!(image_file_id(&links, 3).is_none());
        assert!(image_file_id(&links, 0).is_none());
    }

    #[test]
    fn without_a_library_the_text_is_left_alone() {
        let lines = vec!["[image: chart][1]".to_string(), "after".to_string()];
        let body = place(lines.clone(), &[link(true, "33")], None, 60);
        assert_eq!(body.lines, lines);
        assert!(body.boxes.is_empty());
    }

    #[test]
    fn a_zero_width_page_draws_no_image() {
        let lines = vec!["[image: chart][1]".to_string()];
        let body = place(lines.clone(), &[link(true, "33")], None, 0);
        assert_eq!(body.lines, lines);
        assert!(body.boxes.is_empty());
    }

    #[test]
    fn a_plain_body_has_no_image_rows() {
        let body = Body::plain(vec!["one".into(), "two".into()]);
        assert_eq!(body.lines.len(), 2);
        assert!(body.image_row(0).is_none());
    }

    /// End to end through a real library: an image installed under a file id
    /// shows as a box of cell rows under its mark, and the plain `[n]` text is
    /// untouched everywhere else.
    #[test]
    fn a_mark_whose_file_is_in_the_library_gets_a_box() {
        if !enabled() {
            // NO_COLOR or the setting is off in this environment; the off
            // behaviour is covered by its own tests.
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "tuicanvas-images-lib-{}-{}",
            std::process::id(),
            "place"
        ));
        let _ = std::fs::remove_dir_all(&root);
        let library = crate::library::Library::open(&root, "ufl.instructure.com");
        let downloaded = root.join("incoming.png");
        std::fs::create_dir_all(&root).expect("root");
        std::fs::write(&downloaded, two_tone_png(48, 24)).expect("write");
        library
            .install(
                "33",
                &downloaded,
                crate::library::FileVersion::default(),
                &root.join("chart.png"),
                true,
            )
            .expect("install");

        let lines = vec![
            "Before the image.".to_string(),
            "[image: chart][2]".to_string(),
            "After the image.".to_string(),
        ];
        let links = vec![link(false, "22"), link(true, "33")];
        let body = place(lines, &links, Some(&library), 24);

        let expected_rows = fit(48, 24, 24, MAX_ROWS).1 as usize;
        assert!(expected_rows > 0);
        assert_eq!(body.lines.len(), 3 + expected_rows);
        assert_eq!(body.lines[1], "[image: chart][2]", "the mark is kept");
        for row in 0..expected_rows {
            let line = body.image_row(2 + row).expect("image row");
            assert!(line.width() > 0);
            assert!(line.spans.iter().all(|s| s.content.as_ref() == "\u{2580}"));
        }
        assert_eq!(body.lines[2 + expected_rows], "After the image.");
        assert!(body.image_row(0).is_none());
        assert!(body.image_row(1).is_none());
        assert!(body.image_row(2 + expected_rows).is_none());

        // The link that is not an image is left as text.
        let only_links = place(
            vec!["see [the slides][1]".to_string()],
            &links,
            Some(&library),
            24,
        );
        assert_eq!(only_links.lines, vec!["see [the slides][1]".to_string()]);
        assert!(only_links.boxes.is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    /// `place` needs a library, so this covers the same assembly directly: the
    /// mark's line is kept and the box's rows follow it as blank text lines, so
    /// the line count the page scrolls over matches the rows it draws.
    #[test]
    fn image_rows_follow_the_mark_and_are_counted_as_lines() {
        let path = temp_file("rows.png", &two_tone_png(40, 20));
        let pixels = decode(&path, 20, 10).expect("decodes");
        let rows = half_block_rows(&pixels);
        let mut body = Body::plain(vec!["[image: chart][1]".to_string()]);
        for row in rows.clone() {
            body.boxes.insert(body.lines.len(), row);
            body.lines.push(String::new());
        }
        body.lines.push("after".to_string());
        assert_eq!(body.lines.len(), rows.len() + 2);
        assert!(body.image_row(0).is_none(), "the mark keeps its text");
        for i in 1..=rows.len() {
            assert!(body.image_row(i).is_some(), "row {i} draws cells");
            assert_eq!(body.lines[i], "", "row {i} holds no text");
        }
        assert!(body.image_row(rows.len() + 1).is_none());
        let width = body.image_row(1).expect("row").width();
        assert_eq!(width, pixels.width as usize);
        let _ = std::fs::remove_file(path);
    }
}

use std::io::{self, Cursor, Write};
use std::path::PathBuf;
use std::time::Duration;
use std::{fs, mem};

use image::codecs::gif::GifDecoder;
use image::imageops::{self, FilterType};
use image::{AnimationDecoder as _, ImageFormat, RgbaImage};
use ratatui::style::Style;
use ratatui::text::Span;

use crate::kitty::{self, Ids, ImageId};

/// The pixel size of one terminal cell, as the terminal renders it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellSize {
    /// Width, in pixels.
    pub width: u32,
    /// Height, in pixels.
    pub height: u32,
}

impl CellSize {
    /// Asks the terminal for its cell size, assuming 10 × 20 pixels when it doesn't report pixel dimensions.
    pub fn query() -> Self {
        ratatui::crossterm::terminal::window_size()
            .ok()
            .filter(|size| size.columns > 0 && size.rows > 0)
            .map(|size| Self {
                width: u32::from(size.width / size.columns),
                height: u32::from(size.height / size.rows),
            })
            .filter(|cell| cell.width > 0 && cell.height > 0)
            .unwrap_or(Self {
                width: 10,
                height: 20,
            })
    }
}

/// A rectangle of terminal cells a [`Picture`] is drawn into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellBox {
    /// Width, in cells.
    pub cols: u16,
    /// Height, in cells.
    pub rows: u16,
}

impl CellBox {
    /// Where an emoji goes: two cells wide and one tall, which is roughly square.
    pub const EMOJI: Self = Self { cols: 2, rows: 1 };

    /// The smallest box showing a `width` × `height` pixel picture at its native size, shrunk (keeping the aspect
    /// ratio) to fit within `max`, on cells of `cell` pixels: the box [`Fit::Within`] `max` draws it in.
    pub fn fitting(width: u32, height: u32, max: CellBox, cell: CellSize) -> Self {
        let max = Self {
            cols: max.cols.clamp(1, kitty::MAX_CELLS),
            rows: max.rows.clamp(1, kitty::MAX_CELLS),
        };
        let (max_width, max_height) = max.pixels(cell);
        let scale = f64::min(
            1.0,
            f64::min(max_width / f64::from(width), max_height / f64::from(height)),
        );
        let cells = |pixels: u32, per_cell: u32, limit: u16| {
            let cells = (f64::from(pixels) * scale / f64::from(per_cell)).ceil();
            // `as` saturates; the clamp keeps the box within `max` despite rounding up.
            (cells as u16).clamp(1, limit)
        };
        Self {
            cols: cells(width, cell.width, max.cols),
            rows: cells(height, cell.height, max.rows),
        }
    }

    fn pixels(self, cell: CellSize) -> (f64, f64) {
        (
            f64::from(self.cols) * f64::from(cell.width),
            f64::from(self.rows) * f64::from(cell.height),
        )
    }
}

/// How big to draw a picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fit {
    /// As an emoji, in [`CellBox::EMOJI`].
    Emoji,
    /// At its own size, shrunk to fit within this box.
    Within(CellBox),
}

/// Why a picture couldn't be prepared.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The bytes aren't an image in a format slackers reads (GIF, PNG, JPEG, WebP), or are corrupt.
    #[error("couldn't decode the image")]
    Decode(#[from] image::ImageError),
    /// The image has no frames to show.
    #[error("the image has no frames")]
    Empty,
}

/// A picture decoded, sized and staged for the terminal, but not sent yet.
///
/// Dropping it unsent deletes its staged files.
#[derive(Debug)]
pub struct Prepared {
    /// The commands that transmit and place every frame.
    commands: Vec<u8>,
    /// Files holding frames' pixels, which the terminal deletes once it reads them.
    staged: Vec<PathBuf>,
    picture: Picture,
}

/// Decodes `bytes` (every frame of a GIF; the single frame of a PNG, JPEG or WebP), sizes it to `fit` cells of `cell`
/// pixels, and stages its frames for the terminal under fresh `ids`.
///
/// Slow: it decodes, resizes and writes files. Call it off the UI thread.
///
/// # Errors
///
/// If the bytes aren't an image slackers reads.
pub fn prepare(bytes: &[u8], fit: Fit, cell: CellSize, ids: &Ids) -> Result<Prepared, Error> {
    let mut frames = decode(bytes)?;
    let (width, height) = frames[0].pixels.dimensions();
    let cells = match fit {
        Fit::Emoji => CellBox::EMOJI,
        Fit::Within(max) => CellBox::fitting(width, height, max, cell),
    };
    shrink(&mut frames, cells, cell);

    let mut commands = Vec::new();
    let mut staged = Vec::new();
    let mut ends = Duration::ZERO;
    let mut timed = Vec::with_capacity(frames.len());
    for frame in &frames {
        let id = ids.next();
        staged.extend(kitty::transmit(&mut commands, id, &frame.pixels));
        kitty::place(&mut commands, id, cells.cols, cells.rows);
        ends += frame.delay;
        timed.push((id, ends));
    }
    Ok(Prepared {
        commands,
        staged,
        picture: Picture {
            cells,
            frames: timed,
        },
    })
}

impl Prepared {
    /// Sends the picture to the terminal, and returns it for drawing. Write it between draws, so that it doesn't
    /// interleave with ratatui's output.
    ///
    /// # Errors
    ///
    /// If writing to `out` fails; the picture is then lost.
    pub fn send(mut self, out: &mut impl Write) -> io::Result<Picture> {
        out.write_all(&self.commands)?;
        // The terminal owns the staged files now: it deletes them once read.
        self.staged.clear();
        Ok(mem::replace(
            &mut self.picture,
            Picture {
                cells: CellBox::EMOJI,
                frames: Vec::new(),
            },
        ))
    }
}

impl Drop for Prepared {
    fn drop(&mut self) {
        for path in &self.staged {
            // Best effort: /dev/shm is cleared at reboot anyway.
            let _ = fs::remove_file(path);
        }
    }
}

struct Frame {
    pixels: RgbaImage,
    /// How long this frame shows; zero for a still image.
    delay: Duration,
}

/// Decodes every frame of a GIF, or the single frame of any other format. Like browsers, treats GIF delays under
/// 20 ms as 100 ms; many GIFs rely on that.
fn decode(bytes: &[u8]) -> Result<Vec<Frame>, Error> {
    let format = image::guess_format(bytes)?;
    let frames: Vec<Frame> = if format == ImageFormat::Gif {
        GifDecoder::new(Cursor::new(bytes))?
            .into_frames()
            .collect_frames()?
            .into_iter()
            .map(|frame| {
                let delay = Duration::from(frame.delay());
                let delay = if delay < Duration::from_millis(20) {
                    Duration::from_millis(100)
                } else {
                    delay
                };
                Frame {
                    pixels: frame.into_buffer(),
                    delay,
                }
            })
            .collect()
    } else {
        let pixels = image::load_from_memory_with_format(bytes, format)?.into_rgba8();
        vec![Frame {
            pixels,
            delay: Duration::ZERO,
        }]
    };
    if frames.is_empty() {
        return Err(Error::Empty);
    }
    Ok(frames)
}

/// Shrinks every frame to fit `cells`, keeping the aspect ratio; never enlarges. The terminal scales the rest of the
/// way, so sending more pixels than the box shows only costs time and terminal memory.
fn shrink(frames: &mut [Frame], cells: CellBox, cell: CellSize) {
    let (width, height) = frames[0].pixels.dimensions();
    let (box_width, box_height) = cells.pixels(cell);
    let scale = f64::min(box_width / f64::from(width), box_height / f64::from(height));
    if scale >= 1.0 {
        return;
    }
    let scaled = |pixels: u32| ((f64::from(pixels) * scale).round() as u32).max(1);
    let (new_width, new_height) = (scaled(width), scaled(height));
    for frame in frames {
        frame.pixels = imageops::resize(&frame.pixels, new_width, new_height, FilterType::Triangle);
    }
}

/// A picture in the terminal, drawn as rows of placeholder cells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    cells: CellBox,
    /// One entry per frame, in order: its image, and when it stops showing as an offset into the animation's cycle.
    /// The last offset is the cycle's length; zero for a still image.
    frames: Vec<(ImageId, Duration)>,
}

impl Picture {
    /// The box it's drawn in.
    pub fn cells(&self) -> CellBox {
        self.cells
    }

    /// Row `row` (0-based) of the frame showing `elapsed` after the animation epoch. Every picture should be drawn
    /// against one shared epoch, so that copies of an emoji animate in step.
    ///
    /// # Panics
    ///
    /// If `row` isn't below `self.cells().rows`.
    pub fn row(&self, row: u16, elapsed: Duration) -> Span<'static> {
        assert!(
            row < self.cells.rows,
            "row {row} outside a {}-row picture",
            self.cells.rows
        );
        let (_, index) = self.position(elapsed);
        let id = self.frames[index].0;
        Span::styled(
            kitty::placeholder_row(row, self.cells.cols),
            Style::new().fg(id.color()),
        )
    }

    /// When the shown frame next changes, as time since the epoch; `None` for a still image.
    pub fn next_change(&self, elapsed: Duration) -> Option<Duration> {
        if self.frames.len() < 2 {
            return None;
        }
        let (cycle_start, index) = self.position(elapsed);
        Some(cycle_start + self.frames[index].1)
    }

    /// The start of the cycle `elapsed` falls in, and the index of the frame showing.
    fn position(&self, elapsed: Duration) -> (Duration, usize) {
        let cycle = self.frames.last().map_or(Duration::ZERO, |&(_, ends)| ends);
        if cycle.is_zero() {
            return (elapsed, 0);
        }
        // Nanoseconds since the epoch fit in u64 for 584 years.
        let into_cycle = Duration::from_nanos((elapsed.as_nanos() % cycle.as_nanos()) as u64);
        let index = self.frames.partition_point(|&(_, ends)| ends <= into_cycle);
        (elapsed - into_cycle, index)
    }
}

#[cfg(test)]
mod tests {
    use image::codecs::gif::{GifEncoder, Repeat};
    use image::{Delay, Rgba};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Color;
    use ratatui::text::Line;
    use ratatui::widgets::{Paragraph, Widget as _};

    use super::*;

    const CELL: CellSize = CellSize {
        width: 10,
        height: 20,
    };

    fn gif(delays_ms: &[u32]) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = GifEncoder::new(&mut bytes);
            encoder.set_repeat(Repeat::Infinite).unwrap();
            for (i, &ms) in delays_ms.iter().enumerate() {
                let pixels = RgbaImage::from_pixel(48, 48, Rgba([i as u8 * 40, 0, 0, 255]));
                encoder
                    .encode_frame(image::Frame::from_parts(
                        pixels,
                        0,
                        0,
                        Delay::from_numer_denom_ms(ms, 1),
                    ))
                    .unwrap();
            }
        }
        bytes
    }

    /// Sends `prepared` nowhere, deleting the staged files a terminal would have deleted.
    fn sent(prepared: Prepared) -> Picture {
        let staged = prepared.staged.clone();
        let picture = prepared.send(&mut io::sink()).unwrap();
        for path in staged {
            let _ = fs::remove_file(path);
        }
        picture
    }

    fn shown(picture: &Picture, ms: u64) -> Option<Color> {
        picture.row(0, Duration::from_millis(ms)).style.fg
    }

    #[test]
    fn an_animated_emoji_cycles_through_its_frames() {
        let ids = Ids::default();
        let picture = sent(prepare(&gif(&[100, 50]), Fit::Emoji, CELL, &ids).unwrap());
        assert_eq!(picture.cells(), CellBox::EMOJI);

        let (first, second) = (Some(Color::Rgb(0, 0, 1)), Some(Color::Rgb(0, 0, 2)));
        assert_eq!(shown(&picture, 0), first);
        assert_eq!(shown(&picture, 99), first);
        assert_eq!(shown(&picture, 100), second);
        assert_eq!(shown(&picture, 150), first);
        assert_eq!(
            picture.next_change(Duration::from_millis(120)),
            Some(Duration::from_millis(150))
        );
    }

    #[test]
    fn gif_delays_under_20ms_mean_100ms() {
        let picture = sent(prepare(&gif(&[0, 10]), Fit::Emoji, CELL, &Ids::default()).unwrap());
        assert_eq!(
            picture.next_change(Duration::ZERO),
            Some(Duration::from_millis(100))
        );
    }

    #[test]
    fn a_still_never_changes() {
        let mut png = Cursor::new(Vec::new());
        RgbaImage::from_pixel(95, 41, Rgba([0, 0, 0, 255]))
            .write_to(&mut png, ImageFormat::Png)
            .unwrap();
        let fit = Fit::Within(CellBox { cols: 60, rows: 14 });
        let picture = sent(prepare(png.get_ref(), fit, CELL, &Ids::default()).unwrap());
        assert_eq!(
            picture.cells(),
            CellBox { cols: 10, rows: 3 },
            "native size, rounded up to whole cells"
        );
        assert_eq!(picture.next_change(Duration::from_secs(5)), None);
    }

    #[test]
    fn fitting_shrinks_to_the_tighter_dimension() {
        let max = CellBox { cols: 60, rows: 14 };
        // 1200×280 px is twice too wide (600 px max) but fits in height: half size is 600×140 px = 60×7 cells.
        assert_eq!(
            CellBox::fitting(1200, 280, max, CELL),
            CellBox { cols: 60, rows: 7 }
        );
    }

    #[test]
    fn dropping_an_unsent_picture_deletes_its_staged_files() {
        let prepared = prepare(&gif(&[100, 100]), Fit::Emoji, CELL, &Ids::default()).unwrap();
        let staged = prepared.staged.clone();
        assert_eq!(staged.len(), 2);
        assert!(staged.iter().all(|path| path.exists()));
        drop(prepared);
        assert!(staged.iter().all(|path| !path.exists()));
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(matches!(
            prepare(b"not an image", Fit::Emoji, CELL, &Ids::default()),
            Err(Error::Decode(_))
        ));
    }

    /// The point of the approach: ratatui must treat each placeholder grapheme as one cell carrying the image's
    /// colour, inline between text.
    #[test]
    fn ratatui_draws_one_cell_per_placeholder() {
        let picture = sent(prepare(&gif(&[100]), Fit::Emoji, CELL, &Ids::default()).unwrap());
        let line = Line::from(vec!["a".into(), picture.row(0, Duration::ZERO), "b".into()]);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 6, 1));
        Paragraph::new(line).render(buffer.area, &mut buffer);

        let cells: Vec<String> = kitty::placeholder_row(0, 2)
            .chars()
            .collect::<Vec<_>>()
            .chunks(3)
            .map(String::from_iter)
            .collect();
        assert_eq!(buffer[(0, 0)].symbol(), "a");
        assert_eq!(buffer[(1, 0)].symbol(), cells[0]);
        assert_eq!(buffer[(2, 0)].symbol(), cells[1]);
        assert_eq!(buffer[(3, 0)].symbol(), "b");
        assert_eq!(buffer[(1, 0)].fg, Color::Rgb(0, 0, 1));
    }
}

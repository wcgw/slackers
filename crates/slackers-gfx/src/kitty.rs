//! The subset of the kitty graphics protocol slackers needs: transmitting RGBA images and drawing them through
//! Unicode placeholders.
//!
//! Protocol reference: <https://sw.kovidgoyal.net/kitty/graphics-protocol/>

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use image::RgbaImage;
use ratatui::style::Color;

/// An image the terminal stores for us.
///
/// Ids fit in 24 bits because a placeholder cell carries its image id in a truecolor foreground. (The protocol allows
/// 32 bits through a third diacritic; we don't need that many images.)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ImageId(u32);

impl ImageId {
    /// The foreground colour that makes placeholder cells draw this image.
    pub(crate) fn color(self) -> Color {
        let [_, r, g, b] = self.0.to_be_bytes();
        Color::Rgb(r, g, b)
    }
}

/// Hands out [`ImageId`]s, from any thread.
#[derive(Debug, Default)]
pub struct Ids {
    last: AtomicU32,
}

impl Ids {
    /// A fresh image id.
    ///
    /// # Panics
    ///
    /// After 2^24 - 1 ids: they aren't recycled yet.
    pub(crate) fn next(&self) -> ImageId {
        // Relaxed is enough: the counter only needs to hand out each value once.
        let id = self.last.fetch_add(1, Ordering::Relaxed) + 1;
        assert!(id <= 0x00FF_FFFF, "fewer than 2^24 images were uploaded");
        ImageId(id)
    }
}

/// Appends to `out` the commands that send `pixels` to the terminal as image `id`, replacing any earlier image with
/// that id. Replies are suppressed (`q=2`), so nothing comes back on stdin.
///
/// The pixels go into a file under `/dev/shm`, which the terminal reads and deletes; that path is returned, so
/// that the caller can delete the file should the commands never be sent. When that file can't be written, the
/// pixels go into the commands themselves, which costs about 1.3 bytes on the terminal per pixel byte.
pub(crate) fn transmit(out: &mut Vec<u8>, id: ImageId, pixels: &RgbaImage) -> Option<PathBuf> {
    let (width, height) = pixels.dimensions();
    let control = format!("a=t,f=32,s={width},v={height},i={},q=2", id.0);
    // Ghostty reads temporary files only under /dev/shm or its temp dir, and only if they're named this way. The
    // counter keeps names unique within the process, whichever `Ids` handed out `id`.
    static STAGED: AtomicU64 = AtomicU64::new(0);
    let serial = STAGED.fetch_add(1, Ordering::Relaxed);
    let path = PathBuf::from(format!(
        "/dev/shm/tty-graphics-protocol-slackers-{}-{serial}.rgba",
        std::process::id()
    ));
    match fs::write(&path, pixels.as_raw()) {
        Ok(()) => {
            out.extend_from_slice(format!("\x1b_G{control},t=t;").as_bytes());
            out.extend_from_slice(
                BASE64
                    .encode(path.as_os_str().as_encoded_bytes())
                    .as_bytes(),
            );
            out.extend_from_slice(b"\x1b\\");
            Some(path)
        }
        Err(error) => {
            tracing::debug!(%error, "can't write image data to /dev/shm; sending it in-band");
            send_chunked(out, &control, pixels.as_raw());
            None
        }
    }
}

/// Appends `data` base64-encoded, split into chunks as the protocol requires; `control` goes on the first chunk.
fn send_chunked(out: &mut Vec<u8>, control: &str, data: &[u8]) {
    // The spec caps each chunk's payload at 4096 bytes; 4096 is a multiple of 4, so every chunk but the last is
    // valid base64 on its own, as the spec requires.
    const CHUNK: usize = 4096;
    let payload = BASE64.encode(data);
    let mut chunks = payload.as_bytes().chunks(CHUNK).peekable();
    let mut first = true;
    while let Some(chunk) = chunks.next() {
        let more = u8::from(chunks.peek().is_some());
        let header = if first {
            format!("\x1b_G{control},m={more};")
        } else {
            format!("\x1b_Gm={more};")
        };
        first = false;
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(chunk);
        out.extend_from_slice(b"\x1b\\");
    }
}

/// Appends a command creating a virtual placement of image `id`, `cols` × `rows` cells, so that cells from
/// [`placeholder_row`] draw it. The terminal scales the image to fit that box, keeping its aspect ratio, and
/// centres it.
pub(crate) fn place(out: &mut Vec<u8>, id: ImageId, cols: u16, rows: u16) {
    out.extend_from_slice(
        format!("\x1b_Ga=p,U=1,i={},c={cols},r={rows},q=2\x1b\\", id.0).as_bytes(),
    );
}

/// The command that deletes every image and placement, freeing their memory in the terminal.
pub(crate) const DELETE_ALL: &[u8] = b"\x1b_Ga=d,d=A,q=2\x1b\\";

/// Most rows or columns a placeholder can address: one per diacritic.
pub(crate) const MAX_CELLS: u16 = DIACRITICS.len() as u16;

/// Text for row `row` of an image placed `cols` cells wide: one placeholder grapheme per cell, each carrying its row
/// and column. Style it with [`ImageId::color`] as the foreground to pick the image.
///
/// Every cell carries both diacritics, even though the protocol lets a run omit them, so that a partly redrawn row
/// (after a popup closes, say) still shows the right slice.
///
/// # Panics
///
/// If `row` or `cols` exceeds [`MAX_CELLS`].
pub(crate) fn placeholder_row(row: u16, cols: u16) -> String {
    assert!(
        row < MAX_CELLS && cols <= MAX_CELLS,
        "placeholder out of range: row {row}, {cols} cols"
    );
    let row_mark = DIACRITICS[usize::from(row)];
    let mut text = String::with_capacity(usize::from(cols) * 12);
    for &col_mark in &DIACRITICS[..usize::from(cols)] {
        text.push(PLACEHOLDER);
        text.push(row_mark);
        text.push(col_mark);
    }
    text
}

const PLACEHOLDER: char = '\u{10EEEE}';

/// Row and column numbers, in order: `DIACRITICS[n]` encodes `n`. Copied from kitty's `rowcolumn-diacritics.txt`;
/// Ghostty 1.3.1 uses the identical table.
#[rustfmt::skip]
const DIACRITICS: [char; 297] = [
    '\u{0305}', '\u{030D}', '\u{030E}', '\u{0310}', '\u{0312}', '\u{033D}', '\u{033E}', '\u{033F}',
    '\u{0346}', '\u{034A}', '\u{034B}', '\u{034C}', '\u{0350}', '\u{0351}', '\u{0352}', '\u{0357}',
    '\u{035B}', '\u{0363}', '\u{0364}', '\u{0365}', '\u{0366}', '\u{0367}', '\u{0368}', '\u{0369}',
    '\u{036A}', '\u{036B}', '\u{036C}', '\u{036D}', '\u{036E}', '\u{036F}', '\u{0483}', '\u{0484}',
    '\u{0485}', '\u{0486}', '\u{0487}', '\u{0592}', '\u{0593}', '\u{0594}', '\u{0595}', '\u{0597}',
    '\u{0598}', '\u{0599}', '\u{059C}', '\u{059D}', '\u{059E}', '\u{059F}', '\u{05A0}', '\u{05A1}',
    '\u{05A8}', '\u{05A9}', '\u{05AB}', '\u{05AC}', '\u{05AF}', '\u{05C4}', '\u{0610}', '\u{0611}',
    '\u{0612}', '\u{0613}', '\u{0614}', '\u{0615}', '\u{0616}', '\u{0617}', '\u{0657}', '\u{0658}',
    '\u{0659}', '\u{065A}', '\u{065B}', '\u{065D}', '\u{065E}', '\u{06D6}', '\u{06D7}', '\u{06D8}',
    '\u{06D9}', '\u{06DA}', '\u{06DB}', '\u{06DC}', '\u{06DF}', '\u{06E0}', '\u{06E1}', '\u{06E2}',
    '\u{06E4}', '\u{06E7}', '\u{06E8}', '\u{06EB}', '\u{06EC}', '\u{0730}', '\u{0732}', '\u{0733}',
    '\u{0735}', '\u{0736}', '\u{073A}', '\u{073D}', '\u{073F}', '\u{0740}', '\u{0741}', '\u{0743}',
    '\u{0745}', '\u{0747}', '\u{0749}', '\u{074A}', '\u{07EB}', '\u{07EC}', '\u{07ED}', '\u{07EE}',
    '\u{07EF}', '\u{07F0}', '\u{07F1}', '\u{07F3}', '\u{0816}', '\u{0817}', '\u{0818}', '\u{0819}',
    '\u{081B}', '\u{081C}', '\u{081D}', '\u{081E}', '\u{081F}', '\u{0820}', '\u{0821}', '\u{0822}',
    '\u{0823}', '\u{0825}', '\u{0826}', '\u{0827}', '\u{0829}', '\u{082A}', '\u{082B}', '\u{082C}',
    '\u{082D}', '\u{0951}', '\u{0953}', '\u{0954}', '\u{0F82}', '\u{0F83}', '\u{0F86}', '\u{0F87}',
    '\u{135D}', '\u{135E}', '\u{135F}', '\u{17DD}', '\u{193A}', '\u{1A17}', '\u{1A75}', '\u{1A76}',
    '\u{1A77}', '\u{1A78}', '\u{1A79}', '\u{1A7A}', '\u{1A7B}', '\u{1A7C}', '\u{1B6B}', '\u{1B6D}',
    '\u{1B6E}', '\u{1B6F}', '\u{1B70}', '\u{1B71}', '\u{1B72}', '\u{1B73}', '\u{1CD0}', '\u{1CD1}',
    '\u{1CD2}', '\u{1CDA}', '\u{1CDB}', '\u{1CE0}', '\u{1DC0}', '\u{1DC1}', '\u{1DC3}', '\u{1DC4}',
    '\u{1DC5}', '\u{1DC6}', '\u{1DC7}', '\u{1DC8}', '\u{1DC9}', '\u{1DCB}', '\u{1DCC}', '\u{1DD1}',
    '\u{1DD2}', '\u{1DD3}', '\u{1DD4}', '\u{1DD5}', '\u{1DD6}', '\u{1DD7}', '\u{1DD8}', '\u{1DD9}',
    '\u{1DDA}', '\u{1DDB}', '\u{1DDC}', '\u{1DDD}', '\u{1DDE}', '\u{1DDF}', '\u{1DE0}', '\u{1DE1}',
    '\u{1DE2}', '\u{1DE3}', '\u{1DE4}', '\u{1DE5}', '\u{1DE6}', '\u{1DFE}', '\u{20D0}', '\u{20D1}',
    '\u{20D4}', '\u{20D5}', '\u{20D6}', '\u{20D7}', '\u{20DB}', '\u{20DC}', '\u{20E1}', '\u{20E7}',
    '\u{20E9}', '\u{20F0}', '\u{2CEF}', '\u{2CF0}', '\u{2CF1}', '\u{2DE0}', '\u{2DE1}', '\u{2DE2}',
    '\u{2DE3}', '\u{2DE4}', '\u{2DE5}', '\u{2DE6}', '\u{2DE7}', '\u{2DE8}', '\u{2DE9}', '\u{2DEA}',
    '\u{2DEB}', '\u{2DEC}', '\u{2DED}', '\u{2DEE}', '\u{2DEF}', '\u{2DF0}', '\u{2DF1}', '\u{2DF2}',
    '\u{2DF3}', '\u{2DF4}', '\u{2DF5}', '\u{2DF6}', '\u{2DF7}', '\u{2DF8}', '\u{2DF9}', '\u{2DFA}',
    '\u{2DFB}', '\u{2DFC}', '\u{2DFD}', '\u{2DFE}', '\u{2DFF}', '\u{A66F}', '\u{A67C}', '\u{A67D}',
    '\u{A6F0}', '\u{A6F1}', '\u{A8E0}', '\u{A8E1}', '\u{A8E2}', '\u{A8E3}', '\u{A8E4}', '\u{A8E5}',
    '\u{A8E6}', '\u{A8E7}', '\u{A8E8}', '\u{A8E9}', '\u{A8EA}', '\u{A8EB}', '\u{A8EC}', '\u{A8ED}',
    '\u{A8EE}', '\u{A8EF}', '\u{A8F0}', '\u{A8F1}', '\u{AAB0}', '\u{AAB2}', '\u{AAB3}', '\u{AAB7}',
    '\u{AAB8}', '\u{AABE}', '\u{AABF}', '\u{AAC1}', '\u{FE20}', '\u{FE21}', '\u{FE22}', '\u{FE23}',
    '\u{FE24}', '\u{FE25}', '\u{FE26}', '\u{10A0F}', '\u{10A38}', '\u{1D185}', '\u{1D186}', '\u{1D187}',
    '\u{1D188}', '\u{1D189}', '\u{1D1AA}', '\u{1D1AB}', '\u{1D1AC}', '\u{1D1AD}', '\u{1D242}', '\u{1D243}',
    '\u{1D244}',
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Splits escape-sequence output into (control, payload) pairs.
    fn commands(out: &[u8]) -> Vec<(String, String)> {
        String::from_utf8(out.to_vec())
            .unwrap()
            .split("\x1b\\")
            .filter(|c| !c.is_empty())
            .map(|c| {
                let c = c.strip_prefix("\x1b_G").unwrap();
                let (control, payload) = c.split_once(';').unwrap_or((c, ""));
                (control.to_owned(), payload.to_owned())
            })
            .collect()
    }

    #[test]
    fn ids_fit_a_truecolor_foreground() {
        let ids = Ids::default();
        assert_eq!(ids.next().color(), Color::Rgb(0, 0, 1));
        assert_eq!(ids.next().color(), Color::Rgb(0, 0, 2));
    }

    #[test]
    fn placeholder_row_encodes_row_then_column() {
        let chars: Vec<char> = placeholder_row(1, 2).chars().collect();
        assert_eq!(
            chars,
            [
                '\u{10EEEE}',
                '\u{030D}',
                '\u{0305}',
                '\u{10EEEE}',
                '\u{030D}',
                '\u{030D}'
            ]
        );
    }

    #[test]
    fn chunked_transfer_round_trips() {
        let data: Vec<u8> = (0..10_240u32).map(|i| (i % 251) as u8).collect();
        let mut out = Vec::new();
        send_chunked(&mut out, "a=t,f=32,s=64,v=40,i=7,q=2", &data);
        place(&mut out, ImageId(7), 4, 2);
        let commands = commands(&out);

        let controls: Vec<&str> = commands.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(
            controls,
            [
                "a=t,f=32,s=64,v=40,i=7,q=2,m=1",
                "m=1",
                "m=1",
                "m=0",
                "a=p,U=1,i=7,c=4,r=2,q=2"
            ]
        );
        assert!(commands.iter().all(|(_, payload)| payload.len() <= 4096));
        let payload: String = commands.iter().map(|(_, p)| p.as_str()).collect();
        assert_eq!(BASE64.decode(payload).unwrap(), data);
    }

    #[test]
    fn transmit_names_a_temp_file_holding_the_pixels() {
        let pixels = RgbaImage::from_pixel(3, 2, image::Rgba([1, 2, 3, 4]));
        let mut out = Vec::new();
        let path = transmit(&mut out, ImageId(9), &pixels).expect("/dev/shm is writable here");
        let commands = commands(&out);

        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].0, "a=t,f=32,s=3,v=2,i=9,q=2,t=t");
        let named = String::from_utf8(BASE64.decode(&commands[0].1).unwrap()).unwrap();
        assert_eq!(PathBuf::from(&named), path);
        assert!(named.contains("tty-graphics-protocol"));
        assert_eq!(fs::read(&path).unwrap(), pixels.as_raw().as_slice());
        fs::remove_file(path).unwrap();
    }
}

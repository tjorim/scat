//! Clipboard support via the OSC 52 terminal escape sequence.
//!
//! OSC 52 asks the terminal emulator itself to set the system clipboard, so
//! it works locally and over SSH without any external clipboard tool (xclip,
//! pbcopy, …) or extra dependency. The text is base64-encoded per the OSC 52
//! spec; a tiny local encoder keeps this dependency-free.

use std::io::{self, Write};

/// Copy `text` to the system clipboard using OSC 52.
///
/// Writes `ESC ] 52 ; c ; <base64> BEL` to stdout (the same stream ratatui
/// draws to). The sequence renders nothing, so it interleaves harmlessly
/// between frames.
pub(super) fn copy_to_clipboard(text: &str) -> io::Result<()> {
    let sequence = format!("\x1b]52;c;{}\x07", base64_encode(text.as_bytes()));
    let mut stdout = io::stdout();
    stdout.write_all(sequence.as_bytes())?;
    stdout.flush()
}

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 (RFC 4648) with `=` padding.
fn base64_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).copied().map_or(0, u32::from);
        let b2 = chunk.get(2).copied().map_or(0, u32::from);
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(BASE64_ALPHABET[((n >> 18) & 0x3f) as usize] as char);
        out.push(BASE64_ALPHABET[((n >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            BASE64_ALPHABET[((n >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            BASE64_ALPHABET[(n & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::base64_encode;

    #[test]
    fn base64_matches_known_vectors() {
        // RFC 4648 test vectors.
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_encodes_a_logical_path() {
        assert_eq!(
            base64_encode(b"/catalog/scripts/tools/foo.py"),
            "L2NhdGFsb2cvc2NyaXB0cy90b29scy9mb28ucHk="
        );
    }
}

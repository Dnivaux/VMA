//! Messages between the application and `vma-html` (ADR 0009 § 3, spec `io-html.md`).
//!
//! One JSON object per line on a local TCP connection (127.0.0.1): `vma-html` connects to the
//! port the application gives it and introduces itself with the token it was given. Not the
//! standard input / output of the process (ADR 0009 § 3 says a pipe): Chromium's sub-processes
//! inherit them, and anything they print would break the framing.
//!
//! Pictures do not go through it: `vma-html` writes them into a slot of the input's shared
//! frames ([`crate::shm`]) and sends [`FromHelper::Frame`]; the slot belongs to the application
//! until it answers [`ToHelper::Release`]. JSON escapes line breaks inside strings, so a line is
//! always one message.

use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Both binaries come from the same build: a different version is refused at the connection.
pub const VERSION: u32 = 1;

/// Pictures in flight per browser: the shared frames hold this many slots.
pub const SLOTS: usize = 3;

/// How often `vma-html` sends [`FromHelper::Alive`].
pub const ALIVE_PERIOD: std::time::Duration = std::time::Duration::from_millis(250);

/// Longest line accepted (a URL, a script later): protects against a peer that never ends one.
pub const MAX_LINE: usize = 1 << 20;

/// From the application to `vma-html`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToHelper {
    /// Opens an off-screen browser on `url`, `width` × `height`, painting at `rate_num /
    /// rate_den` images per second into the shared frames at `frames`.
    Create {
        browser: u32,
        url: String,
        width: u32,
        height: u32,
        rate_num: u32,
        rate_den: u32,
        frames: PathBuf,
    },
    /// Loads the page again (after its renderer process ended).
    Reload {
        browser: u32,
    },
    Close {
        browser: u32,
    },
    /// The application has read `slot`: `vma-html` may write it again.
    Release {
        browser: u32,
        slot: u8,
    },
    /// Closes every browser and exits.
    Quit,
}

/// From `vma-html` to the application.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromHelper {
    /// First message: the protocol version and the token given on the command line.
    Hello {
        version: u32,
        token: String,
    },
    /// The main frame of the page finished loading (`http_status` 0 for a `file://` page).
    Loaded {
        browser: u32,
        http_status: i32,
    },
    /// Picture `seq` of the browser is in `slot`, BGRA, premultiplied alpha.
    Frame {
        browser: u32,
        slot: u8,
        seq: u64,
    },
    /// The page's renderer process ended (crash, killed, out of memory).
    RendererGone {
        browser: u32,
        reason: String,
    },
    /// The browser could not be opened, or its page could not be loaded.
    Failed {
        browser: u32,
        reason: String,
    },
    Closed {
        browser: u32,
    },
    /// Sent every [`ALIVE_PERIOD`] whatever the pages do: a static page paints only when it
    /// changes, a crashed one not at all, and the application must still tell a stuck helper.
    Alive,
}

/// Why a message could not be read.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("connection closed")]
    Closed,
    #[error("line longer than {MAX_LINE} bytes")]
    TooLong,
    #[error("unreadable message: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Writes one message and flushes it.
pub fn write<T: Serialize>(out: &mut impl Write, message: &T) -> io::Result<()> {
    let mut line = serde_json::to_vec(message).map_err(io::Error::other)?;
    line.push(b'\n');
    out.write_all(&line)?;
    out.flush()
}

/// Reads one message; `line` is reused between calls.
pub fn read<T: for<'de> Deserialize<'de>>(
    input: &mut impl BufRead,
    line: &mut Vec<u8>,
) -> Result<T, ReadError> {
    line.clear();
    let mut limited = io::Read::take(&mut *input, MAX_LINE as u64 + 1);
    let n = limited.read_until(b'\n', line)?;
    if n == 0 {
        return Err(ReadError::Closed);
    }
    if line.last() != Some(&b'\n') {
        return Err(if n > MAX_LINE {
            ReadError::TooLong
        } else {
            ReadError::Closed
        });
    }
    Ok(serde_json::from_slice(line)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip_one_per_line() {
        let mut buf = Vec::new();
        let create = ToHelper::Create {
            browser: 7,
            url: "file:///tmp/a b.html".into(),
            width: 1920,
            height: 1080,
            rate_num: 60000,
            rate_den: 1001,
            frames: PathBuf::from("/dev/shm/vma-html-1-7"),
        };
        write(&mut buf, &create).unwrap();
        write(
            &mut buf,
            &ToHelper::Release {
                browser: 7,
                slot: 2,
            },
        )
        .unwrap();
        assert_eq!(buf.iter().filter(|&&b| b == b'\n').count(), 2);

        let mut input = io::Cursor::new(buf);
        let mut line = Vec::new();
        assert_eq!(read::<ToHelper>(&mut input, &mut line).unwrap(), create);
        assert_eq!(
            read::<ToHelper>(&mut input, &mut line).unwrap(),
            ToHelper::Release {
                browser: 7,
                slot: 2
            }
        );
        assert!(matches!(
            read::<ToHelper>(&mut input, &mut line),
            Err(ReadError::Closed)
        ));
    }

    #[test]
    fn a_line_break_inside_a_string_stays_in_one_line() {
        let mut buf = Vec::new();
        let gone = FromHelper::RendererGone {
            browser: 1,
            reason: "crashed\nhard".into(),
        };
        write(&mut buf, &gone).unwrap();
        assert_eq!(buf.iter().filter(|&&b| b == b'\n').count(), 1);
        let mut line = Vec::new();
        assert_eq!(
            read::<FromHelper>(&mut io::Cursor::new(buf), &mut line).unwrap(),
            gone
        );
    }

    #[test]
    fn a_truncated_or_endless_line_is_refused() {
        let mut line = Vec::new();
        let half = br#"{"type":"quit""#.to_vec();
        assert!(matches!(
            read::<ToHelper>(&mut io::Cursor::new(half), &mut line),
            Err(ReadError::Closed)
        ));
        let endless = vec![b'a'; MAX_LINE + 10];
        assert!(matches!(
            read::<ToHelper>(&mut io::Cursor::new(endless), &mut line),
            Err(ReadError::TooLong)
        ));
    }
}

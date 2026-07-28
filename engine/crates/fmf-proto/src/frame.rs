//! 16-byte little-endian frame header + length-prefixed payload.
//!
//! The header type and its byte conversion live in `fmf_contract::pod`; this
//! module adds the stream I/O and the [`MAX_PAYLOAD_LEN`] policy.
//!
//! Requests are multiplexed by `request_id` and may complete out of order.
//! `status` is meaningful only on a response, and any malformed frame —
//! unknown opcode, truncation, or a length past the operation's cap — is a
//! disconnect plus a counter, never a partial parse.

use std::io::{Read, Write};

pub use fmf_contract::limits::MAX_PAYLOAD_LEN;
pub use fmf_contract::pod::FrameHeader;

/// Wire size of the fixed frame header, in bytes (16).
pub const HEADER_LEN: usize = FrameHeader::LEN;

/// `flags` bit marking a frame as a response to a prior request.
pub const FLAG_RESPONSE: u16 = 1 << 0;
/// `flags` bit marking a frame as an unsolicited server-pushed event.
pub const FLAG_EVENT: u16 = 1 << 1;

/// Why a frame could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// The header's payload length exceeds the applicable global or
    /// operation-specific cap; a protocol violation that tears down the
    /// connection.
    #[error("frame payload {len} bytes exceeds the {maximum}-byte cap")]
    TooLong {
        /// Announced or supplied payload length.
        len: u32,
        /// Applicable cap.
        maximum: u32,
    },
    /// The underlying stream read or write failed.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Decodes a header and enforces the payload cap. A header announcing more
/// is a protocol violation: the connection is torn down (counted by the
/// server).
///
/// # Errors
///
/// Returns [`FrameError::TooLong`] when the header's `len` exceeds
/// [`MAX_PAYLOAD_LEN`].
pub const fn decode_header(b: &[u8; HEADER_LEN]) -> Result<FrameHeader, FrameError> {
    let h = FrameHeader::from_bytes(b);
    if h.len > MAX_PAYLOAD_LEN {
        return Err(FrameError::TooLong {
            len: h.len,
            maximum: MAX_PAYLOAD_LEN,
        });
    }
    Ok(h)
}

/// Writes header + payload as one frame. `header.len` is taken from
/// `payload`, not the caller (a mismatch cannot be expressed).
///
/// # Errors
///
/// Returns [`FrameError::TooLong`] when `payload` exceeds [`MAX_PAYLOAD_LEN`],
/// or [`FrameError::Io`] if the underlying writer fails.
pub fn write_frame(
    w: &mut impl Write,
    mut header: FrameHeader,
    payload: &[u8],
) -> Result<(), FrameError> {
    if payload.len() as u64 > u64::from(MAX_PAYLOAD_LEN) {
        return Err(FrameError::TooLong {
            len: payload.len() as u32,
            maximum: MAX_PAYLOAD_LEN,
        });
    }
    header.len = payload.len() as u32;
    w.write_all(&header.to_bytes())?;
    w.write_all(payload)?;
    Ok(())
}

/// Reads exactly one frame. Errors leave the stream in an undefined
/// position — the caller must drop the connection (byte-mode pipes have no
/// resync point).
///
/// # Errors
///
/// Returns [`FrameError::TooLong`] if the header announces a payload over
/// [`MAX_PAYLOAD_LEN`], or [`FrameError::Io`] if the reader fails or the
/// stream ends before the full header and payload arrive.
pub fn read_frame(r: &mut impl Read) -> Result<(FrameHeader, Vec<u8>), FrameError> {
    read_frame_capped(r, |_| MAX_PAYLOAD_LEN)
}

/// Reads one frame while applying an operation-specific payload cap after
/// the fixed header and before allocating the payload buffer.
///
/// # Errors
///
/// Returns [`FrameError::TooLong`] when the header exceeds the global cap or
/// `maximum_for`'s smaller cap, or [`FrameError::Io`] for truncated/failed I/O.
pub fn read_frame_capped(
    r: &mut impl Read,
    maximum_for: impl FnOnce(&FrameHeader) -> u32,
) -> Result<(FrameHeader, Vec<u8>), FrameError> {
    let mut hb = [0u8; HEADER_LEN];
    r.read_exact(&mut hb)?;
    let header = decode_header(&hb)?;
    let maximum = maximum_for(&header).min(MAX_PAYLOAD_LEN);
    if header.len > maximum {
        return Err(FrameError::TooLong {
            len: header.len,
            maximum,
        });
    }
    let mut payload = vec![0u8; header.len as usize];
    r.read_exact(&mut payload)?;
    Ok((header, payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrips_and_pins_layout() {
        let h = FrameHeader {
            len: 0x0001_0203, // under the 16 MiB cap
            opcode: 0x0506,
            flags: FLAG_RESPONSE | FLAG_EVENT,
            request_id: 0x0708_090A,
            status: -2,
        };
        let b = h.to_bytes();
        // Golden bytes: LE field order {len, opcode, flags, request_id, status}.
        assert_eq!(
            b,
            [
                0x03, 0x02, 0x01, 0x00, // len
                0x06, 0x05, // opcode
                0x03, 0x00, // flags
                0x0A, 0x09, 0x08, 0x07, // request_id
                0xFE, 0xFF, 0xFF, 0xFF, // status (-2)
            ]
        );
        assert_eq!(decode_header(&b).unwrap(), h);
    }

    #[test]
    fn oversized_header_is_rejected() {
        let mut b = FrameHeader {
            len: 0,
            opcode: 1,
            flags: 0,
            request_id: 1,
            status: 0,
        }
        .to_bytes();
        b[0..4].copy_from_slice(&(MAX_PAYLOAD_LEN + 1).to_le_bytes());
        assert!(matches!(
            decode_header(&b),
            Err(FrameError::TooLong {
                len,
                maximum: MAX_PAYLOAD_LEN
            }) if len == MAX_PAYLOAD_LEN + 1
        ));
    }

    #[test]
    fn frame_roundtrips_over_a_stream() {
        let mut buf = Vec::new();
        let h = FrameHeader {
            len: 0, // overwritten by write_frame
            opcode: 7,
            flags: 0,
            request_id: 42,
            status: 0,
        };
        write_frame(&mut buf, h, b"payload").unwrap();
        let (rh, payload) = read_frame(&mut buf.as_slice()).unwrap();
        assert_eq!(rh.len, 7);
        assert_eq!(rh.opcode, 7);
        assert_eq!(rh.request_id, 42);
        assert_eq!(payload, b"payload");
    }

    #[test]
    fn operation_cap_is_enforced_before_payload_allocation() {
        let header = FrameHeader {
            len: 9,
            opcode: 7,
            flags: 0,
            request_id: 1,
            status: 0,
        };
        let mut input = header.to_bytes().to_vec();
        input.extend_from_slice(b"123456789");

        assert!(matches!(
            read_frame_capped(&mut input.as_slice(), |_| 8),
            Err(FrameError::TooLong { len: 9, maximum: 8 })
        ));
    }

    #[test]
    fn truncated_stream_errors_at_every_cut_point() {
        let mut buf = Vec::new();
        write_frame(
            &mut buf,
            FrameHeader {
                len: 0,
                opcode: 1,
                flags: 0,
                request_id: 1,
                status: 0,
            },
            b"abcdef",
        )
        .unwrap();
        for cut in 0..buf.len() {
            let r = read_frame(&mut &buf[..cut]);
            assert!(r.is_err(), "cut at {cut} must error, not invent a frame");
        }
    }
}

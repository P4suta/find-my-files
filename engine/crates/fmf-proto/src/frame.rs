//! 16-byte little-endian frame header + length-prefixed payload
//! (docs/ARCHITECTURE.md「Pipe プロトコル」§フレーム).

use std::io::{Read, Write};

pub const HEADER_LEN: usize = 16;

/// Hard cap on a single frame's payload. A header announcing more is a
/// protocol violation: the connection is torn down (counted by the server).
pub const MAX_PAYLOAD_LEN: u32 = 16 * 1024 * 1024;

pub const FLAG_RESPONSE: u16 = 1 << 0;
pub const FLAG_EVENT: u16 = 1 << 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    /// Payload length in bytes (the header itself excluded).
    pub len: u32,
    pub opcode: u16,
    pub flags: u16,
    /// Request/response correlation; 0 on event pushes.
    pub request_id: u32,
    /// Error code (`crate::codes`); meaningful on responses only.
    pub status: i32,
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("frame payload {0} bytes exceeds the {MAX_PAYLOAD_LEN}-byte cap")]
    TooLong(u32),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl FrameHeader {
    pub fn to_bytes(self) -> [u8; HEADER_LEN] {
        let mut b = [0u8; HEADER_LEN];
        b[0..4].copy_from_slice(&self.len.to_le_bytes());
        b[4..6].copy_from_slice(&self.opcode.to_le_bytes());
        b[6..8].copy_from_slice(&self.flags.to_le_bytes());
        b[8..12].copy_from_slice(&self.request_id.to_le_bytes());
        b[12..16].copy_from_slice(&self.status.to_le_bytes());
        b
    }

    pub fn from_bytes(b: &[u8; HEADER_LEN]) -> Result<Self, FrameError> {
        let h = Self {
            len: u32::from_le_bytes(b[0..4].try_into().unwrap()),
            opcode: u16::from_le_bytes(b[4..6].try_into().unwrap()),
            flags: u16::from_le_bytes(b[6..8].try_into().unwrap()),
            request_id: u32::from_le_bytes(b[8..12].try_into().unwrap()),
            status: i32::from_le_bytes(b[12..16].try_into().unwrap()),
        };
        if h.len > MAX_PAYLOAD_LEN {
            return Err(FrameError::TooLong(h.len));
        }
        Ok(h)
    }
}

/// Writes header + payload as one frame. `header.len` is taken from
/// `payload`, not the caller (a mismatch cannot be expressed).
pub fn write_frame(
    w: &mut impl Write,
    mut header: FrameHeader,
    payload: &[u8],
) -> Result<(), FrameError> {
    if payload.len() as u64 > MAX_PAYLOAD_LEN as u64 {
        return Err(FrameError::TooLong(payload.len() as u32));
    }
    header.len = payload.len() as u32;
    w.write_all(&header.to_bytes())?;
    w.write_all(payload)?;
    Ok(())
}

/// Reads exactly one frame. Errors leave the stream in an undefined
/// position — the caller must drop the connection (byte-mode pipes have no
/// resync point).
pub fn read_frame(r: &mut impl Read) -> Result<(FrameHeader, Vec<u8>), FrameError> {
    let mut hb = [0u8; HEADER_LEN];
    r.read_exact(&mut hb)?;
    let header = FrameHeader::from_bytes(&hb)?;
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
        assert_eq!(FrameHeader::from_bytes(&b).unwrap(), h);
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
            FrameHeader::from_bytes(&b),
            Err(FrameError::TooLong(_))
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

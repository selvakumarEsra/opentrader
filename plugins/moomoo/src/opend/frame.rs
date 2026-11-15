/*
 * OpenD's framing: a 44-byte header, then the body. The header is
 *   'F' 'T' | proto id u32 | format u8 | version u8 | serial u32 | body length u32 | SHA-1 of the body (20) | 8 reserved
 * with the numbers little endian. The plugin always sends JSON bodies (format 1).
 */
use sha1::{Digest, Sha1};

pub const HEADER_LEN: usize = 44;
pub const FORMAT_JSON: u8 = 1;
/// A body larger than this is a corrupt length, not a message.
const MAX_BODY: usize = 64 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub proto: u32,
    pub serial: u32,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    BadMagic,
    TooLarge(usize),
    BadChecksum { proto: u32 },
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::BadMagic => f.write_str("OpenD sent a message without its FT header"),
            FrameError::TooLarge(n) => write!(f, "OpenD sent a message of {n} bytes"),
            FrameError::BadChecksum { proto } => write!(f, "OpenD message {proto} failed its checksum"),
        }
    }
}

pub fn encode(proto: u32, serial: u32, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + body.len());
    out.extend_from_slice(b"FT");
    out.extend_from_slice(&proto.to_le_bytes());
    out.push(FORMAT_JSON);
    out.push(0);
    out.extend_from_slice(&serial.to_le_bytes());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&Sha1::digest(body));
    out.extend_from_slice(&[0; 8]);
    out.extend_from_slice(body);
    out
}

/// The first whole frame of `buf`, removed from it; `None` while it has not all arrived.
pub fn decode(buf: &mut Vec<u8>) -> Result<Option<Frame>, FrameError> {
    if buf.len() < HEADER_LEN {
        return Ok(None);
    }
    if &buf[..2] != b"FT" {
        return Err(FrameError::BadMagic);
    }
    let u32_at = |i: usize| u32::from_le_bytes([buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]);
    let (proto, serial, len) = (u32_at(2), u32_at(8), u32_at(12) as usize);
    if len > MAX_BODY {
        return Err(FrameError::TooLarge(len));
    }
    if buf.len() < HEADER_LEN + len {
        return Ok(None);
    }
    let body = buf[HEADER_LEN..HEADER_LEN + len].to_vec();
    if Sha1::digest(&body)[..] != buf[16..36] {
        return Err(FrameError::BadChecksum { proto });
    }
    buf.drain(..HEADER_LEN + len);
    Ok(Some(Frame { proto, serial, body }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_holds_the_fields_in_order() {
        let f = encode(3103, 7, b"{}");
        assert_eq!(f.len(), HEADER_LEN + 2);
        assert_eq!(&f[..2], b"FT");
        assert_eq!(u32::from_le_bytes(f[2..6].try_into().unwrap()), 3103);
        assert_eq!((f[6], f[7]), (FORMAT_JSON, 0));
        assert_eq!(u32::from_le_bytes(f[8..12].try_into().unwrap()), 7);
        assert_eq!(u32::from_le_bytes(f[12..16].try_into().unwrap()), 2);
        assert_eq!(&f[16..36], &Sha1::digest(b"{}")[..]);
        assert_eq!(&f[36..44], &[0; 8]);
    }

    #[test]
    fn decodes_what_it_encodes_and_waits_for_the_rest() {
        let whole = encode(1001, 1, br#"{"c2s":{}}"#);
        let mut buf = whole[..20].to_vec();
        assert_eq!(decode(&mut buf), Ok(None));
        buf.extend_from_slice(&whole[20..]);
        buf.extend_from_slice(&encode(1004, 2, b"{}")[..10]);
        let f = decode(&mut buf).unwrap().unwrap();
        assert_eq!((f.proto, f.serial, f.body.as_slice()), (1001, 1, &br#"{"c2s":{}}"#[..]));
        assert_eq!(buf.len(), 10, "the next frame's start stays");
        assert_eq!(decode(&mut buf), Ok(None));
    }

    #[test]
    fn refuses_a_bad_header_or_checksum() {
        let mut bad = encode(1, 1, b"x");
        bad[0] = b'X';
        assert_eq!(decode(&mut bad), Err(FrameError::BadMagic));
        let mut bad = encode(3005, 1, b"abc");
        let last = bad.len() - 1;
        bad[last] = b'z';
        assert_eq!(decode(&mut bad), Err(FrameError::BadChecksum { proto: 3005 }));
    }
}

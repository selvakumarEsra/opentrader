/*
 * RFC 6238 time-based one-time passwords (HMAC-SHA1, 30 s, 6 digits), as authenticator apps make them from the
 * base32 secret a broker shows when TOTP is turned on.
 */
use hmac::{Hmac, Mac};
use sha1::Sha1;

/// The bytes of a base32 secret (RFC 4648; case, spaces, dashes and padding ignored).
pub fn base32(secret: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = Vec::new();
    let (mut buffer, mut bits) = (0u32, 0u32);
    for c in secret.bytes().filter(|c| !matches!(c, b' ' | b'-' | b'=')) {
        let v = ALPHABET.iter().position(|a| *a == c.to_ascii_uppercase())? as u32;
        buffer = (buffer << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    (!out.is_empty()).then_some(out)
}

/// The 6-digit code of `secret` (base32) at `unix` seconds.
pub fn code(secret: &str, unix: i64) -> Result<String, String> {
    let key = base32(secret).ok_or("The TOTP secret is not a base32 key (letters A-Z and digits 2-7)")?;
    let counter = (unix.max(0) as u64) / 30;
    let mut mac = Hmac::<Sha1>::new_from_slice(&key).map_err(|e| e.to_string())?;
    mac.update(&counter.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = (digest[19] & 0x0f) as usize;
    let n = u32::from_be_bytes([digest[offset] & 0x7f, digest[offset + 1], digest[offset + 2], digest[offset + 3]]);
    Ok(format!("{:06}", n % 1_000_000))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 6238's SHA-1 secret "12345678901234567890", in base32.
    const RFC: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

    #[test]
    fn rfc_6238_vectors() {
        assert_eq!(code(RFC, 59).unwrap(), "287082");
        assert_eq!(code(RFC, 1_111_111_109).unwrap(), "081804");
        assert_eq!(code(RFC, 2_000_000_000).unwrap(), "279037");
    }

    #[test]
    fn secrets_are_read_loosely_and_bad_ones_refused() {
        assert_eq!(code("gezd gnbv-gy3t qojq gezd gnbv gy3t qojq", 59).unwrap(), "287082");
        assert!(code("not base32!", 59).is_err());
        assert!(code("", 59).is_err());
    }
}

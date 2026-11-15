/*
 * OpenD's encryption, used when the user gives OpenD's RSA key. InitConnect, both ways, is RSA (PKCS#1 v1.5) in
 * chunks sized to the key: each chunk of plain text a little under the key's size becomes one key-sized block.
 * Every later body is AES-128 with the key OpenD returns in InitConnect: CBC with PKCS#7 padding when OpenD also
 * returns an IV, else OpenD's ECB form — the body zero-padded to whole blocks, then one more block whose last byte
 * is the length of the last plain block (0 when it was whole).
 */
use aes::cipher::{generic_array::GenericArray, BlockDecrypt, BlockEncrypt, KeyInit};
use aes::Aes128;
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::traits::PublicKeyParts;
use rsa::{Pkcs1v15Encrypt, RsaPrivateKey, RsaPublicKey};

const BLOCK: usize = 16;

pub struct Rsa {
    private: RsaPrivateKey,
    public: RsaPublicKey,
}

impl Rsa {
    /// The PEM key file OpenD is set up with (PKCS#1 or PKCS#8).
    pub fn from_pem(pem: &str) -> Result<Rsa, String> {
        let private = RsaPrivateKey::from_pkcs1_pem(pem)
            .or_else(|_| RsaPrivateKey::from_pkcs8_pem(pem))
            .map_err(|_| "the RSA key file holds no RSA private key".to_string())?;
        Ok(Rsa { public: private.to_public_key(), private })
    }

    pub fn read(path: &str) -> Result<Rsa, String> {
        let pem = std::fs::read_to_string(path).map_err(|e| format!("the RSA key file {path} could not be read: {e}"))?;
        Rsa::from_pem(&pem).map_err(|e| format!("{e} ({path})"))
    }

    /// Plain bytes per chunk: 100 for a 1024-bit key, 200 for 2048 bits, as OpenD cuts them.
    fn plain_chunk(&self) -> usize {
        self.public.size() * 100 / 128
    }

    pub fn encrypt(&self, plain: &[u8]) -> Result<Vec<u8>, String> {
        let mut rng = rsa::rand_core::OsRng;
        let mut out = Vec::with_capacity(plain.len() / self.plain_chunk() * self.public.size() + self.public.size());
        for chunk in plain.chunks(self.plain_chunk()) {
            out.extend(self.public.encrypt(&mut rng, Pkcs1v15Encrypt, chunk).map_err(|e| format!("RSA: {e}"))?);
        }
        Ok(out)
    }

    pub fn decrypt(&self, sealed: &[u8]) -> Result<Vec<u8>, String> {
        let size = self.public.size();
        if !sealed.len().is_multiple_of(size) {
            return Err("OpenD sent an RSA message cut short".into());
        }
        let mut out = Vec::with_capacity(sealed.len());
        for chunk in sealed.chunks(size) {
            out.extend(self.private.decrypt(Pkcs1v15Encrypt, chunk).map_err(|_| "OpenD's RSA message did not decrypt with this key".to_string())?);
        }
        Ok(out)
    }
}

pub struct Aes {
    cipher: Aes128,
    /// CBC's IV; ECB form without one.
    iv: Option<[u8; BLOCK]>,
}

impl Aes {
    /// InitConnect's `connAESKey` and, when OpenD gives one, `aesCBCiv`.
    pub fn new(key: &[u8], iv: Option<&[u8]>) -> Result<Aes, String> {
        let key: [u8; BLOCK] = key.try_into().map_err(|_| format!("OpenD's AES key has {} bytes, not 16", key.len()))?;
        let iv = match iv.filter(|v| !v.is_empty()) {
            None => None,
            Some(v) => Some(<[u8; BLOCK]>::try_from(v).map_err(|_| format!("OpenD's AES IV has {} bytes, not 16", v.len()))?),
        };
        Ok(Aes { cipher: Aes128::new(&key.into()), iv })
    }

    pub fn encrypt(&self, plain: &[u8]) -> Vec<u8> {
        match self.iv {
            Some(iv) => {
                let pad = BLOCK - plain.len() % BLOCK;
                let mut data = plain.to_vec();
                data.extend(std::iter::repeat_n(pad as u8, pad));
                let mut prev = iv;
                for block in data.chunks_mut(BLOCK) {
                    block.iter_mut().zip(prev).for_each(|(b, p)| *b ^= p);
                    self.cipher.encrypt_block(GenericArray::from_mut_slice(block));
                    prev.copy_from_slice(block);
                }
                data
            }
            None => {
                let tail = plain.len() % BLOCK;
                let mut data = plain.to_vec();
                data.resize(plain.len().div_ceil(BLOCK) * BLOCK, 0);
                data.chunks_mut(BLOCK).for_each(|b| self.cipher.encrypt_block(GenericArray::from_mut_slice(b)));
                let mut last = [0u8; BLOCK];
                last[BLOCK - 1] = tail as u8;
                data.extend_from_slice(&last);
                data
            }
        }
    }

    pub fn decrypt(&self, sealed: &[u8]) -> Result<Vec<u8>, String> {
        let bad = || "OpenD's AES message did not decrypt with the connection's key".to_string();
        if sealed.is_empty() || !sealed.len().is_multiple_of(BLOCK) {
            return Err(bad());
        }
        match self.iv {
            Some(iv) => {
                let mut data = sealed.to_vec();
                let mut prev = iv;
                for block in data.chunks_mut(BLOCK) {
                    let sealed_block: [u8; BLOCK] = (&*block).try_into().map_err(|_| bad())?;
                    self.cipher.decrypt_block(GenericArray::from_mut_slice(block));
                    block.iter_mut().zip(prev).for_each(|(b, p)| *b ^= p);
                    prev = sealed_block;
                }
                let pad = *data.last().ok_or_else(bad)? as usize;
                if pad == 0 || pad > BLOCK || data[data.len() - pad..].iter().any(|b| *b as usize != pad) {
                    return Err(bad());
                }
                data.truncate(data.len() - pad);
                Ok(data)
            }
            None => {
                let (body, last) = sealed.split_at(sealed.len() - BLOCK);
                let tail = last[BLOCK - 1] as usize;
                if tail >= BLOCK || (tail != 0 && body.is_empty()) {
                    return Err(bad());
                }
                let mut data = body.to_vec();
                data.chunks_mut(BLOCK).for_each(|b| self.cipher.decrypt_block(GenericArray::from_mut_slice(b)));
                if tail != 0 {
                    data.truncate(data.len() - (BLOCK - tail));
                }
                Ok(data)
            }
        }
    }
}

/// A connection's encryption: RSA for InitConnect, AES for the rest once InitConnect gave its key.
#[derive(Default)]
pub struct Seal {
    pub rsa: Option<Rsa>,
    pub aes: Option<Aes>,
}

impl Seal {
    pub fn is_on(&self) -> bool {
        self.rsa.is_some()
    }

    pub fn close(&self, proto: u32, plain: &[u8]) -> Result<Vec<u8>, String> {
        match (&self.rsa, &self.aes) {
            (None, _) => Ok(plain.to_vec()),
            (Some(rsa), _) if proto == super::proto::INIT_CONNECT => rsa.encrypt(plain),
            (Some(_), Some(aes)) => Ok(aes.encrypt(plain)),
            (Some(_), None) => Err("the encrypted connection to OpenD is not set up yet".into()),
        }
    }

    pub fn open(&self, proto: u32, sealed: &[u8]) -> Result<Vec<u8>, String> {
        match (&self.rsa, &self.aes) {
            (None, _) => Ok(sealed.to_vec()),
            (Some(rsa), _) if proto == super::proto::INIT_CONNECT => rsa.decrypt(sealed),
            (Some(_), Some(aes)) => aes.decrypt(sealed),
            (Some(_), None) => Err(format!("OpenD sent message {proto} before the encrypted connection was set up")),
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use rsa::pkcs1::EncodeRsaPrivateKey;

    /// A fresh 1024-bit key as PEM, as a user would set in OpenD.
    pub fn test_pem() -> String {
        let key = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 1024).unwrap();
        key.to_pkcs1_pem(rsa::pkcs1::LineEnding::LF).unwrap().to_string()
    }

    #[test]
    fn rsa_round_trips_bodies_longer_than_a_chunk() {
        let rsa = Rsa::from_pem(&test_pem()).unwrap();
        let plain: Vec<u8> = (0..=255u8).cycle().take(333).collect();
        let sealed = rsa.encrypt(&plain).unwrap();
        assert_eq!(sealed.len(), 4 * 128);
        assert_eq!(rsa.decrypt(&sealed).unwrap(), plain);
        assert!(Rsa::from_pem("not a key").is_err());
    }

    #[test]
    fn aes_round_trips_in_both_forms() {
        let cbc = Aes::new(b"0123456789abcdef", Some(b"fedcba9876543210")).unwrap();
        let ecb = Aes::new(b"0123456789abcdef", None).unwrap();
        for n in [0usize, 15, 16, 33] {
            let plain: Vec<u8> = (0..n as u8).collect();
            assert_eq!(cbc.decrypt(&cbc.encrypt(&plain)).unwrap(), plain, "cbc {n}");
            let sealed = ecb.encrypt(&plain);
            assert_eq!(sealed.len(), n.div_ceil(16) * 16 + 16, "ecb length {n}");
            assert_eq!(ecb.decrypt(&sealed).unwrap(), plain, "ecb {n}");
        }
        assert!(Aes::new(b"short", None).is_err());
        assert!(cbc.decrypt(&[1, 2, 3]).is_err());
    }
}

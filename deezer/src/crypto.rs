//! The encryption of the audio: Blowfish-CBC in stripes.
//!
//! Deezer serves the audio encrypted: the cipher is Blowfish in CBC mode with the IV `[0, 1, …, 7]`, and the key
//! is derived from the id of the track. The pattern is a stripe of 6144 bytes in which the first 2048 are encrypted
//! and the two that follow are not (the cipher is called `BF_CBC_STRIPE` by the media API): each encrypted stripe
//! is a chain of its own, so a stripe can be decrypted on its own and the download can be split in parallel ranges.

use blowfish::Blowfish;
use cipher::{Block, BlockCipherDecrypt, KeyInit};
use md5::{Digest, Md5};

/// The secret the key is folded with.
const SECRET: &str = "g4el58wc0zvf9na1";
/// The IV of every stripe.
const IV: [u8; 8] = [0, 1, 2, 3, 4, 5, 6, 7];
/// The size of a block of the cipher.
const BLOCK: usize = 8;
/// The size of an encrypted stripe.
pub const STRIPE: usize = 2048;
/// The size of the pattern: an encrypted stripe and two clear ones.
pub const PATTERN: usize = STRIPE * 3;

/// The key of a track: the MD5 of its id (as text), folded with the secret.
///
/// The MD5 is written as text (its 32 hexadecimal digits) and every byte of the key is the XOR of two of those
/// digits and one of the secret: the same key the web player and the other downloaders use.
pub fn key(track_id: &str) -> [u8; 16] {
    let hash = Md5::digest(track_id.as_bytes());
    let hex = hex(&hash);
    let digits = hex.as_bytes();
    let mut key = [0u8; 16];
    for (i, byte) in key.iter_mut().enumerate() {
        *byte = digits[i] ^ digits[i + 16] ^ SECRET.as_bytes()[i];
    }
    key
}

/// The bytes of `hash` as their hexadecimal text (lowercase).
fn hex(hash: &[u8]) -> String {
    let mut out = String::with_capacity(hash.len() * 2);
    for byte in hash {
        out.push(char::from_digit((byte >> 4) as u32, 16).unwrap_or('0'));
        out.push(char::from_digit((byte & 0xf) as u32, 16).unwrap_or('0'));
    }
    out
}

/// Undoes the encryption of a stream of bytes.
///
/// The bytes come in whatever sizes the network gives them: they are gathered until a whole stripe is ready, the
/// encrypted ones are decrypted and the clear ones go straight through. A stripe that is not whole (the last one
/// of a file) is never encrypted, so it goes through untouched.
pub struct Decryptor {
    cipher: Blowfish,
    /// The bytes not yet a whole stripe.
    buf: Vec<u8>,
    /// How many stripes have gone through (the encrypted ones are the first of every three).
    stripes: u64,
}

impl Decryptor {
    /// A decryptor for the key of a track.
    pub fn new(key: &[u8; 16]) -> Result<Self, String> {
        let cipher = Blowfish::new_from_slice(key).map_err(|e| format!("clave: {e}"))?;
        Ok(Self {
            cipher,
            buf: Vec::with_capacity(STRIPE * 2),
            stripes: 0,
        })
    }

    /// The bytes to write: the encrypted stripes decrypted, the rest as they came.
    pub fn push(&mut self, data: &[u8]) -> Vec<u8> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        while self.buf.len() >= STRIPE {
            let stripe: Vec<u8> = self.buf.drain(..STRIPE).collect();
            out.extend_from_slice(&self.stripe(stripe));
            self.stripes += 1;
        }
        out
    }

    /// The last bytes (a stripe that is not whole is never encrypted).
    pub fn finish(mut self) -> Vec<u8> {
        std::mem::take(&mut self.buf)
    }

    /// One whole stripe: decrypted when it is the first of every three.
    fn stripe(&mut self, mut stripe: Vec<u8>) -> Vec<u8> {
        if self.stripes % 3 == 0 {
            decrypt_cbc(&self.cipher, &mut stripe);
        }
        stripe
    }
}

/// Decrypts `data` in place: Blowfish-CBC, no padding, the IV of [`IV`].
fn decrypt_cbc(cipher: &Blowfish, data: &mut [u8]) {
    let mut previous = IV;
    for block in data.chunks_exact_mut(BLOCK) {
        let mut encrypted = [0u8; BLOCK];
        encrypted.copy_from_slice(block);
        let mut decrypted = Block::<Blowfish>::try_from(&block[..]).expect("the block is 8 bytes");
        cipher.decrypt_block(&mut decrypted);
        for (i, byte) in decrypted.iter_mut().enumerate() {
            *byte ^= previous[i];
        }
        block.copy_from_slice(&decrypted);
        previous = encrypted;
    }
}

/// Whether the byte at `offset` belongs to an encrypted stripe.
///
/// The pattern starts at the first byte of the file: the stripes of 2048 bytes are numbered from there and the
/// first of every three is the encrypted one.
pub fn is_encrypted(offset: u64) -> bool {
    (offset / STRIPE as u64) % 3 == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vectors;
    use cipher::BlockCipherEncrypt;

    /// The key of a track, the way the other downloaders derive it (the MD5 of the id folded with the secret).
    #[test]
    fn the_key_is_the_md5_of_the_id_folded_with_the_secret() {
        assert_eq!(hex(&Md5::digest(vectors::FIRST_ID.as_bytes())), vectors::FIRST_MD5);
        assert_eq!(hex(&Md5::digest(vectors::SECOND_ID.as_bytes())), vectors::SECOND_MD5);
        assert_eq!(hex(&key(vectors::FIRST_ID)), vectors::FIRST_KEY);
        assert_eq!(hex(&key(vectors::SECOND_ID)), vectors::SECOND_KEY);
    }

    /// The stripe of the reference implementation comes back as it went in.
    ///
    /// The vectors are the stripes encrypted by the Blowfish of the other downloader: the engine has to give
    /// exactly the same bytes back, or the audio would be noise.
    #[test]
    fn a_reference_stripe_comes_back_as_it_went_in() {
        for (id, cipher) in [
            (vectors::FIRST_ID, vectors::FIRST_CIPHER),
            (vectors::SECOND_ID, vectors::SECOND_CIPHER),
        ] {
            let mut decryptor = Decryptor::new(&key(id)).expect("the key is 16 bytes");
            let encrypted = unhex(cipher);
            assert_eq!(encrypted.len(), STRIPE);
            let out = decryptor.push(&encrypted);
            assert_eq!(out.len(), STRIPE);
            let plain: Vec<u8> = (0..STRIPE).map(|i| (i % 255) as u8 + 1).collect();
            assert_eq!(out, plain, "the stripe of {id} did not come back as it went in");
        }
    }

    /// The stripe pattern: the first of every three stripes is encrypted.
    #[test]
    fn every_third_stripe_is_encrypted() {
        assert!(is_encrypted(0));
        assert!(!is_encrypted(STRIPE as u64));
        assert!(!is_encrypted(2 * STRIPE as u64));
        assert!(is_encrypted(PATTERN as u64));
        assert!(!is_encrypted(PATTERN as u64 + STRIPE as u64));
        assert!(is_encrypted(2 * PATTERN as u64));
    }

    /// The decryptor undoes what the cipher did: encrypting a stripe again with the same key gives the bytes back.
    #[test]
    fn a_stripe_comes_back_as_it_went_in() {
        let key = key("3135556");
        let mut decryptor = Decryptor::new(&key).expect("the key is 16 bytes");
        let plain: Vec<u8> = (0..STRIPE * 3).map(|i| (i % 251) as u8).collect();
        // Encrypt the first stripe with the same cipher and the same IV.
        let mut encrypted = plain.clone();
        encrypt_cbc(&key, &mut encrypted[..STRIPE]);
        let mixed = [encrypted[..STRIPE].to_vec(), plain[STRIPE..].to_vec()].concat();
        let out = decryptor.push(&mixed);
        assert_eq!(out, plain[..STRIPE * 3]);
    }

    /// The bytes that are not a whole stripe go through untouched (the last stripe of a file is never encrypted).
    #[test]
    fn a_partial_stripe_is_not_encrypted() {
        let mut data = vec![7u8; 100];
        data.extend_from_slice(&[8u8; 50]);
        let mut decryptor = Decryptor::new(&key("1")).expect("the key is 16 bytes");
        assert!(decryptor.push(&data).is_empty());
        assert_eq!(decryptor.finish(), data);
    }

    /// The bytes of a text of hexadecimal digits.
    fn unhex(text: &str) -> Vec<u8> {
        (0..text.len() / 2)
            .map(|i| u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).expect("the text is hexadecimal"))
            .collect()
    }

    /// Blowfish-CBC, the other way round (the tests need it to build a case).
    fn encrypt_cbc(key: &[u8; 16], data: &mut [u8]) {
        let cipher: Blowfish = Blowfish::new_from_slice(key).expect("the key is 16 bytes");
        let mut previous = IV;
        for block in data.chunks_exact_mut(BLOCK) {
            let mut input = [0u8; BLOCK];
            for i in 0..BLOCK {
                input[i] = block[i] ^ previous[i];
            }
            let mut encrypted = Block::<Blowfish>::try_from(&input[..]).expect("the block is 8 bytes");
            cipher.encrypt_block(&mut encrypted);
            block.copy_from_slice(&encrypted);
            let mut out = [0u8; BLOCK];
            out.copy_from_slice(block);
            previous = out;
        }
    }
}

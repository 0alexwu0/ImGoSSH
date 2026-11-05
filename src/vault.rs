//! Versioned, authenticated session encryption and OS-backed credentials.
//! The OS vault is the security boundary; opaque identifiers only reduce casual discovery.
use std::{num::NonZeroU32, path::Path};

use anyhow::{anyhow, bail, Context, Result};
use ring::{
    aead, digest, pbkdf2,
    rand::{SecureRandom, SystemRandom},
};
use zeroize::Zeroizing;

const MAGIC: &[u8] = b"ImGoSSH-SESSION-VAULT\0";
const VERSION: u8 = 1;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const ROUNDS: u32 = 600_000;
const HEADER_LEN: usize = MAGIC.len() + 1 + SALT_LEN + NONCE_LEN;

pub(crate) struct Key {
    bytes: Zeroizing<[u8; 32]>,
    salt: [u8; SALT_LEN],
}

pub(crate) fn is_encrypted(data: &[u8]) -> bool {
    data.starts_with(MAGIC)
}

impl Key {
    pub(crate) fn new(password: &str) -> Result<Self> {
        if password.is_empty() {
            bail!("Password cannot be empty");
        }
        let mut salt = [0; SALT_LEN];
        SystemRandom::new()
            .fill(&mut salt)
            .map_err(|_| anyhow!("Random generator failed"))?;
        Ok(Self::derive(password, salt))
    }

    fn derive(password: &str, salt: [u8; SALT_LEN]) -> Self {
        let mut bytes = Zeroizing::new([0; 32]);
        pbkdf2::derive(
            pbkdf2::PBKDF2_HMAC_SHA256,
            NonZeroU32::new(ROUNDS).unwrap(),
            &salt,
            password.as_bytes(),
            bytes.as_mut(),
        );
        Self { bytes, salt }
    }

    fn cipher(&self) -> Result<aead::LessSafeKey> {
        Ok(aead::LessSafeKey::new(
            aead::UnboundKey::new(&aead::AES_256_GCM, self.bytes.as_ref())
                .map_err(|_| anyhow!("Invalid encryption key"))?,
        ))
    }

    pub(crate) fn seal(&self, plain: &[u8]) -> Result<Vec<u8>> {
        let mut nonce = [0; NONCE_LEN];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| anyhow!("Random generator failed"))?;
        let mut out = MAGIC.to_vec();
        out.push(VERSION);
        out.extend_from_slice(&self.salt);
        out.extend_from_slice(&nonce);
        let mut body = Zeroizing::new(plain.to_vec());
        self.cipher()?
            .seal_in_place_append_tag(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(out.as_slice()),
                &mut *body,
            )
            .map_err(|_| anyhow!("Encryption failed"))?;
        out.extend_from_slice(&body);
        Ok(out)
    }

    pub(crate) fn open(data: &[u8], password: &str) -> Result<(Self, Zeroizing<Vec<u8>>)> {
        if data.len() < HEADER_LEN + aead::AES_256_GCM.tag_len() || !is_encrypted(data) {
            bail!("Invalid encrypted session file");
        }
        if data[MAGIC.len()] != VERSION {
            bail!("Unsupported session encryption version");
        }
        let salt_start = MAGIC.len() + 1;
        let salt = data[salt_start..salt_start + SALT_LEN].try_into().unwrap();
        let nonce = data[salt_start + SALT_LEN..HEADER_LEN].try_into().unwrap();
        let key = Self::derive(password, salt);
        let mut body = Zeroizing::new(data[HEADER_LEN..].to_vec());
        let len = key
            .cipher()?
            .open_in_place(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(&data[..HEADER_LEN]),
                &mut body,
            )
            .map_err(|_| anyhow!("Incorrect password or damaged session file"))?
            .len();
        body.truncate(len);
        Ok((key, body))
    }
}

// Opaque, per-store identifiers. Never embed a secret or a filesystem hiding place.
fn entry(path: &Path) -> Result<keyring::Entry> {
    let parent = path.parent().unwrap_or(Path::new("."));
    let absolute = std::fs::canonicalize(parent)
        .or_else(|_| std::env::current_dir().map(|cwd| cwd.join(parent)))?;
    let identity = absolute.join(path.file_name().unwrap_or_default());
    let hash = digest::digest(&digest::SHA256, identity.to_string_lossy().as_bytes());
    let account: String = hash.as_ref().iter().map(|b| format!("{b:02x}")).collect();
    let service: String = [
        0x73u8, 0x6d, 0x02, 0x6d, 0x69, 0x69, 0x72, 0x17, 0x0b, 0x08, 0x0e,
    ]
    .iter()
    .map(|b| (b ^ 0x3a) as char)
    .collect();
    keyring::Entry::new(&service, &account).context("Opening system credential store")
}

pub(crate) fn recalled(path: &Path) -> Result<Zeroizing<String>> {
    Ok(Zeroizing::new(
        entry(path)?
            .get_password()
            .context("Reading system credential store")?,
    ))
}

pub(crate) fn remember(path: &Path, password: &str) -> Result<()> {
    entry(path)?
        .set_password(password)
        .context("Saving password in system credential store")
}

pub(crate) fn forget(path: &Path) -> Result<()> {
    match entry(path)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(err) => Err(err).context("Removing password from system credential store"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encryption_authenticates_password_header_and_contents() {
        let key = Key::new("测试 master password").unwrap();
        let data = key.seal(b"session secret").unwrap();
        assert!(!data.windows(6).any(|w| w == b"secret"));
        assert_ne!(data, key.seal(b"session secret").unwrap());
        assert_eq!(
            Key::open(&data, "测试 master password")
                .unwrap()
                .1
                .as_slice(),
            b"session secret"
        );
        assert!(Key::open(&data, "wrong").is_err());
        for offset in [
            MAGIC.len(),
            MAGIC.len() + 1,
            HEADER_LEN - 1,
            HEADER_LEN,
            data.len() - 1,
        ] {
            let mut damaged = data.clone();
            damaged[offset] ^= 1;
            assert!(Key::open(&damaged, "测试 master password").is_err());
        }
        assert!(Key::open(&data[..HEADER_LEN], "测试 master password").is_err());
        assert!(Key::new("").is_err());
    }
}

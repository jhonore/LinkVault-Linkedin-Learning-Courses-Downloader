//! Per-platform protect/unprotect primitive for secrets at rest.
//!
//! Provider token stores own paths, filenames, and error types. This module
//! only encrypts and decrypts byte buffers, which is why it is the right seam
//! for the platform difference: the stores are identical everywhere.
//!
//! - Windows: DPAPI (`CryptProtectData`), the original and still the shipped
//!   platform.
//! - macOS: AES-256-GCM over a 32-byte key held in the login keychain. Same
//!   guarantee shape as DPAPI — ciphertext on disk, key protected by the OS and
//!   scoped to the user account.
//! - Anything else: unsupported, as before.
//!
//! `description` is domain-separation, not decoration. DPAPI treats it as an
//! informational label, but the macOS path binds it as AEAD associated data so
//! a LinkedIn blob cannot be decrypted as a Coursera one. That matters: both
//! blobs live in the same directory under the same key, and swapping them would
//! otherwise make the app send a LinkedIn cookie to Coursera.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DpapiError {
    #[error("DPAPI storage is unavailable on this platform")]
    UnsupportedPlatform,
    #[error("DPAPI operation failed: {0}")]
    Storage(String),
}

pub fn protect_bytes(plaintext: &[u8], description: &str) -> Result<Vec<u8>, DpapiError> {
    protect(plaintext, description)
}

pub fn unprotect_bytes(ciphertext: &[u8], description: &str) -> Result<Vec<u8>, DpapiError> {
    unprotect(ciphertext, description)
}

#[cfg(windows)]
fn protect(input: &[u8], description: &str) -> Result<Vec<u8>, DpapiError> {
    use std::ptr;
    use std::slice;
    use windows_sys::Win32::Foundation::{LocalFree, HLOCAL};
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let mut input_blob = CRYPT_INTEGER_BLOB {
        cbData: input.len() as u32,
        pbData: input.as_ptr() as *mut u8,
    };
    let mut output_blob = CRYPT_INTEGER_BLOB::default();
    let description = wide_null(description);

    let ok = unsafe {
        // SAFETY: input_blob points at `input` for the duration of the call;
        // description is a NUL-terminated UTF-16 buffer; output_blob is written
        // by CryptProtectData and freed with LocalFree below.
        CryptProtectData(
            &mut input_blob,
            description.as_ptr(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output_blob,
        )
    };

    if ok == 0 {
        return Err(DpapiError::Storage(
            "Windows DPAPI could not protect the buffer".to_string(),
        ));
    }

    let bytes = unsafe {
        // SAFETY: CryptProtectData succeeded and output_blob.pbData is a
        // LocalAlloc buffer of cbData bytes.
        let output =
            slice::from_raw_parts(output_blob.pbData, output_blob.cbData as usize).to_vec();
        LocalFree(output_blob.pbData as HLOCAL);
        output
    };
    Ok(bytes)
}

#[cfg(windows)]
fn unprotect(input: &[u8], _description: &str) -> Result<Vec<u8>, DpapiError> {
    // DPAPI does not authenticate the description: CryptUnprotectData returns it
    // as an out-parameter rather than checking it. Windows behaviour is therefore
    // unchanged by the new argument.

    use std::ptr;
    use std::slice;
    use windows_sys::Win32::Foundation::{LocalFree, HLOCAL};
    use windows_sys::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let mut input_blob = CRYPT_INTEGER_BLOB {
        cbData: input.len() as u32,
        pbData: input.as_ptr() as *mut u8,
    };
    let mut output_blob = CRYPT_INTEGER_BLOB::default();

    let ok = unsafe {
        // SAFETY: input_blob points at `input` for the duration of the call;
        // output_blob is written by CryptUnprotectData and freed with LocalFree.
        CryptUnprotectData(
            &mut input_blob,
            ptr::null_mut(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output_blob,
        )
    };

    if ok == 0 {
        return Err(DpapiError::Storage(
            "Windows DPAPI could not unprotect the buffer".to_string(),
        ));
    }

    let bytes = unsafe {
        // SAFETY: CryptUnprotectData succeeded and output_blob.pbData is a
        // LocalAlloc buffer of cbData bytes.
        let output =
            slice::from_raw_parts(output_blob.pbData, output_blob.cbData as usize).to_vec();
        LocalFree(output_blob.pbData as HLOCAL);
        output
    };
    Ok(bytes)
}

#[cfg(windows)]
fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(target_os = "macos")]
fn protect(input: &[u8], description: &str) -> Result<Vec<u8>, DpapiError> {
    let key = macos::keychain_key()?;
    let mut nonce = [0u8; macos::NONCE_LEN];
    macos::fill_random(&mut nonce);
    macos::seal(&key, nonce, input, description.as_bytes())
}

#[cfg(target_os = "macos")]
fn unprotect(input: &[u8], description: &str) -> Result<Vec<u8>, DpapiError> {
    let key = macos::keychain_key()?;
    macos::open(&key, input, description.as_bytes())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn protect(_input: &[u8], _description: &str) -> Result<Vec<u8>, DpapiError> {
    Err(DpapiError::UnsupportedPlatform)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn unprotect(_input: &[u8], _description: &str) -> Result<Vec<u8>, DpapiError> {
    Err(DpapiError::UnsupportedPlatform)
}

/// macOS backend, split into a pure core and one small impure edge.
///
/// `seal` and `open` never touch the keychain or the RNG, so every property of
/// the on-disk format is testable on any platform, including Windows CI.
/// `keychain_key` is the only function that reaches out to the OS.
#[cfg(target_os = "macos")]
mod macos {
    use super::DpapiError;
    use aes_gcm::aead::{rand_core::RngCore, Aead, OsRng, Payload};
    use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
    use zeroize::Zeroizing;

    /// `MAGIC | VERSION | NONCE | ciphertext+tag`. Without the frame a foreign
    /// or truncated file would surface as an opaque AEAD tag mismatch; with it
    /// the failure names itself, and the version byte leaves room to change the
    /// format later without guessing at what is on disk.
    const MAGIC: &[u8; 4] = b"LVS1";
    const VERSION: u8 = 1;
    pub(super) const NONCE_LEN: usize = 12;
    const KEY_LEN: usize = 32;
    const HEADER_LEN: usize = MAGIC.len() + 1 + NONCE_LEN;

    const KEYCHAIN_SERVICE: &str = "dev.linkvault.downloader";
    const KEYCHAIN_ACCOUNT: &str = "secret-store-key-v1";

    pub(super) type SecretKey = Zeroizing<[u8; KEY_LEN]>;

    pub(super) fn fill_random(buffer: &mut [u8]) {
        OsRng.fill_bytes(buffer);
    }

    pub(super) fn seal(
        key: &[u8; KEY_LEN],
        nonce: [u8; NONCE_LEN],
        plaintext: &[u8],
        aad: &[u8],
    ) -> Result<Vec<u8>, DpapiError> {
        let cipher = new_cipher(key)?;
        let sealed = cipher
            .encrypt(Nonce::from_slice(&nonce), Payload { msg: plaintext, aad })
            .map_err(|_| DpapiError::Storage("could not encrypt the secret".to_string()))?;

        let mut blob = Vec::with_capacity(HEADER_LEN + sealed.len());
        blob.extend_from_slice(MAGIC);
        blob.push(VERSION);
        blob.extend_from_slice(&nonce);
        blob.extend_from_slice(&sealed);
        Ok(blob)
    }

    pub(super) fn open(
        key: &[u8; KEY_LEN],
        blob: &[u8],
        aad: &[u8],
    ) -> Result<Vec<u8>, DpapiError> {
        if blob.len() <= HEADER_LEN {
            return Err(DpapiError::Storage(
                "stored secret is truncated".to_string(),
            ));
        }
        if &blob[..MAGIC.len()] != MAGIC {
            return Err(DpapiError::Storage(
                "stored secret is not a LinkedVault blob".to_string(),
            ));
        }
        let version = blob[MAGIC.len()];
        if version != VERSION {
            return Err(DpapiError::Storage(format!(
                "unsupported stored secret version {version}"
            )));
        }

        let cipher = new_cipher(key)?;
        cipher
            .decrypt(
                Nonce::from_slice(&blob[MAGIC.len() + 1..HEADER_LEN]),
                Payload {
                    msg: &blob[HEADER_LEN..],
                    aad,
                },
            )
            .map_err(|_| {
                DpapiError::Storage("stored secret could not be decrypted".to_string())
            })
    }

    fn new_cipher(key: &[u8; KEY_LEN]) -> Result<Aes256Gcm, DpapiError> {
        Aes256Gcm::new_from_slice(key)
            .map_err(|_| DpapiError::Storage("secret key has the wrong length".to_string()))
    }

    /// Read the data-encryption key from the login keychain, creating it on
    /// first use. This is the only impure part of the macOS backend.
    pub(super) fn keychain_key() -> Result<SecretKey, DpapiError> {
        use security_framework::passwords::{get_generic_password, set_generic_password};

        if let Ok(existing) = get_generic_password(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT) {
            return key_from_slice(&existing);
        }

        let mut fresh: SecretKey = Zeroizing::new([0u8; KEY_LEN]);
        fill_random(&mut fresh[..]);
        match set_generic_password(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT, &fresh[..]) {
            Ok(()) => Ok(fresh),
            // Lost a race, or the item appeared between our read and our write.
            // Re-read before reporting failure so we never overwrite a key that
            // existing ciphertext depends on.
            Err(error) => match get_generic_password(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT) {
                Ok(existing) => key_from_slice(&existing),
                Err(_) => Err(DpapiError::Storage(format!(
                    "macOS keychain could not store the LinkedVault key: {error}"
                ))),
            },
        }
    }

    fn key_from_slice(bytes: &[u8]) -> Result<SecretKey, DpapiError> {
        if bytes.len() != KEY_LEN {
            return Err(DpapiError::Storage(
                "keychain key has an unexpected length".to_string(),
            ));
        }
        let mut key: SecretKey = Zeroizing::new([0u8; KEY_LEN]);
        key.copy_from_slice(bytes);
        Ok(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn round_trips_bytes_without_leaving_plaintext_in_ciphertext() {
        let plaintext = b"linkvault-dpapi-roundtrip";
        let ciphertext = protect_bytes(plaintext, "LinkedVault test").unwrap();
        assert!(!ciphertext.windows(plaintext.len()).any(|w| w == plaintext));
        assert_eq!(
            unprotect_bytes(&ciphertext, "LinkedVault test").unwrap(),
            plaintext
        );
    }

    // macOS now has a real backend, so "unsupported" is only true for the
    // remaining platforms.
    #[cfg(not(any(windows, target_os = "macos")))]
    #[test]
    fn protect_is_unsupported_off_windows_and_macos() {
        assert!(matches!(
            protect_bytes(b"secret", "test"),
            Err(DpapiError::UnsupportedPlatform)
        ));
    }

    // The macOS format is exercised through the pure core, so these run without
    // touching the login keychain.
    #[cfg(target_os = "macos")]
    mod macos_format {
        use super::super::macos::{open, seal, NONCE_LEN};
        use super::super::DpapiError;

        const KEY: [u8; 32] = [7u8; 32];
        const NONCE: [u8; NONCE_LEN] = [3u8; NONCE_LEN];
        const AAD: &[u8] = b"LinkedVault LinkedIn session";

        #[test]
        fn round_trips_bytes_without_leaving_plaintext_in_ciphertext() {
            let plaintext = b"linkvault-keychain-roundtrip";
            let blob = seal(&KEY, NONCE, plaintext, AAD).unwrap();
            assert!(!blob.windows(plaintext.len()).any(|w| w == plaintext));
            assert_eq!(open(&KEY, &blob, AAD).unwrap(), plaintext);
        }

        /// The reason `description` is bound as associated data: a LinkedIn blob
        /// dropped into the Coursera token file must not decrypt, or the app
        /// would send a LinkedIn cookie to Coursera.
        #[test]
        fn rejects_a_blob_sealed_under_a_different_description() {
            let blob = seal(&KEY, NONCE, b"li_at-value", AAD).unwrap();
            assert!(matches!(
                open(&KEY, &blob, b"LinkedVault Coursera session"),
                Err(DpapiError::Storage(_))
            ));
        }

        #[test]
        fn rejects_a_blob_sealed_under_a_different_key() {
            let blob = seal(&KEY, NONCE, b"li_at-value", AAD).unwrap();
            assert!(matches!(
                open(&[9u8; 32], &blob, AAD),
                Err(DpapiError::Storage(_))
            ));
        }

        #[test]
        fn rejects_foreign_truncated_and_future_blobs() {
            let blob = seal(&KEY, NONCE, b"li_at-value", AAD).unwrap();

            let mut foreign = blob.clone();
            foreign[0] = b'X';
            assert!(matches!(
                open(&KEY, &foreign, AAD),
                Err(DpapiError::Storage(_))
            ));

            let mut future = blob.clone();
            future[4] = 99;
            assert!(matches!(
                open(&KEY, &future, AAD),
                Err(DpapiError::Storage(_))
            ));

            assert!(matches!(
                open(&KEY, &blob[..8], AAD),
                Err(DpapiError::Storage(_))
            ));
        }

        #[test]
        fn tampering_with_the_ciphertext_is_detected() {
            let mut blob = seal(&KEY, NONCE, b"li_at-value", AAD).unwrap();
            let last = blob.len() - 1;
            blob[last] ^= 0x01;
            assert!(matches!(open(&KEY, &blob, AAD), Err(DpapiError::Storage(_))));
        }
    }

    /// Touches the real login keychain, so it is not part of the default run.
    /// Exercise it with:
    ///   cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml \
    ///     keychain_round_trip -- --ignored
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "uses the login keychain; run explicitly with --ignored"]
    fn keychain_round_trip() {
        let plaintext = b"linkvault-live-keychain-roundtrip";
        let blob = protect_bytes(plaintext, "LinkedVault test").unwrap();
        assert!(!blob.windows(plaintext.len()).any(|w| w == plaintext));
        assert_eq!(
            unprotect_bytes(&blob, "LinkedVault test").unwrap(),
            plaintext
        );
    }
}

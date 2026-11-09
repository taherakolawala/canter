//! Protection for the saved session, readable only by the current user.
//!
//! Windows seals it with DPAPI. Elsewhere it is encrypted with a key kept in
//! the system keychain (Keychain on macOS, the Secret Service on Linux and
//! the BSDs), or, where no keychain answers, in a key file only the user can
//! read, next to the session.

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };

    fn run(data: &[u8], decrypt: bool) -> Result<Vec<u8>, String> {
        let len =
            u32::try_from(data.len()).map_err(|_| "Session data is too large.".to_string())?;
        let input = CRYPT_INTEGER_BLOB {
            cbData: len,
            pbData: data.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: input points to `len` readable bytes for the duration of the call;
        // DPAPI allocates output with LocalAlloc, which is copied and then freed once.
        let ok = unsafe {
            if decrypt {
                CryptUnprotectData(
                    &input,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut output,
                )
            } else {
                CryptProtectData(
                    &input,
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut output,
                )
            }
        };
        if ok == 0 || output.pbData.is_null() {
            return Err(if decrypt {
                "Windows could not unlock the saved session.".to_string()
            } else {
                "Windows could not protect the session.".to_string()
            });
        }
        // SAFETY: DPAPI returned a buffer of cbData bytes that we own until LocalFree.
        unsafe {
            let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
            LocalFree(output.pbData as _);
            Ok(bytes)
        }
    }

    pub fn protect(data: &[u8], _dir: &std::path::Path) -> Result<Vec<u8>, String> {
        run(data, false)
    }

    pub fn unprotect(data: &[u8], _dir: &std::path::Path) -> Result<Vec<u8>, String> {
        run(data, true)
    }
}

#[cfg(unix)]
mod imp {
    use chacha20poly1305::aead::{Aead, Generate, KeyInit};
    use chacha20poly1305::{XChaCha20Poly1305, XNonce};
    use keyring_core::api::CredentialStoreApi;
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    /// Marks a sealed session and the format it is in.
    const MAGIC: &[u8] = b"canter-session-1
";
    const NONCE_LEN: usize = 24;
    const KEY_LEN: usize = 32;
    /// The keychain item: service and account.
    const SERVICE: &str = "canter";
    const ACCOUNT: &str = "session-key";
    /// The key file used where no keychain answers.
    pub const KEY_FILE: &str = "session.key";

    type Key = [u8; KEY_LEN];

    /// The key, once found, for the folder it belongs to: the keychain is
    /// asked once a run rather than on every save of rotated cookies.
    static KEY: Mutex<Option<(PathBuf, Key)>> = Mutex::new(None);
    /// A key file made this run, to mention once canter exits.
    static MADE_KEY_FILE: Mutex<Option<PathBuf>> = Mutex::new(None);

    pub fn protect(data: &[u8], dir: &Path) -> Result<Vec<u8>, String> {
        let key = key(dir, true)?;
        let nonce = XNonce::generate();
        let sealed = XChaCha20Poly1305::new(&key.into())
            .encrypt(&nonce, data)
            .map_err(|_| "The session could not be encrypted.".to_string())?;
        Ok([MAGIC, nonce.as_slice(), &sealed].concat())
    }

    pub fn unprotect(data: &[u8], dir: &Path) -> Result<Vec<u8>, String> {
        let locked = || {
            "The saved session could not be unlocked. Run canter --login to sign in again."
                .to_string()
        };
        let rest = data.strip_prefix(MAGIC).ok_or_else(locked)?;
        if rest.len() < NONCE_LEN {
            return Err(locked());
        }
        let (nonce, sealed) = rest.split_at(NONCE_LEN);
        let nonce = XNonce::try_from(nonce).map_err(|_| locked())?;
        let key = key(dir, false).map_err(|_| locked())?;
        XChaCha20Poly1305::new(&key.into())
            .decrypt(&nonce, sealed)
            .map_err(|_| locked())
    }

    /// The key file made this run because no keychain answered, if any.
    pub fn key_file_made() -> Option<PathBuf> {
        MADE_KEY_FILE.lock().ok()?.clone()
    }

    /// The session key for `dir`: a key file there if one was made before,
    /// else the keychain's. `create` makes one where there is none, in the
    /// keychain if it answers and in a key file if not.
    fn key(dir: &Path, create: bool) -> Result<Key, String> {
        let mut cached = KEY
            .lock()
            .map_err(|_| "The session key is unavailable.".to_string())?;
        if let Some((for_dir, key)) = cached.as_ref() {
            if for_dir == dir {
                return Ok(*key);
            }
        }
        let file = dir.join(KEY_FILE);
        let key = match std::fs::read(&file) {
            Ok(bytes) => Key::try_from(bytes.as_slice())
                .map_err(|_| format!("The session key in {} is damaged.", file.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                from_keychain(create).or_else(|keychain| {
                    if !create {
                        return Err(keychain);
                    }
                    let key = fresh();
                    write_key_file(&file, &key)?;
                    if let Ok(mut made) = MADE_KEY_FILE.lock() {
                        *made = Some(file.clone());
                    }
                    Ok(key)
                })?
            }
            Err(_) => {
                return Err(format!(
                    "The session key in {} could not be read.",
                    file.display()
                ));
            }
        };
        *cached = Some((dir.to_path_buf(), key));
        Ok(key)
    }

    fn fresh() -> Key {
        chacha20poly1305::Key::generate().into()
    }

    fn from_keychain(create: bool) -> Result<Key, String> {
        let unavailable = |e: keyring_core::Error| format!("No keychain is available: {e}");
        let entry = store()
            .map_err(unavailable)?
            .build(SERVICE, ACCOUNT, None)
            .map_err(unavailable)?;
        match entry.get_secret() {
            Ok(bytes) => Key::try_from(bytes.as_slice())
                .map_err(|_| "The keychain's session key is damaged.".to_string()),
            Err(keyring_core::Error::NoEntry) if create => {
                let key = fresh();
                entry.set_secret(&key).map_err(unavailable)?;
                Ok(key)
            }
            Err(e) => Err(unavailable(e)),
        }
    }

    #[cfg(target_os = "macos")]
    fn store() -> keyring_core::Result<std::sync::Arc<apple_native_keyring_store::keychain::Store>>
    {
        apple_native_keyring_store::keychain::Store::new()
    }

    #[cfg(not(target_os = "macos"))]
    fn store() -> keyring_core::Result<std::sync::Arc<zbus_secret_service_keyring_store::Store>> {
        zbus_secret_service_keyring_store::Store::new()
    }

    /// Writes the key readable and writable by this user only.
    fn write_key_file(file: &Path, key: &Key) -> Result<(), String> {
        let fail = || format!("The session key could not be saved to {}.", file.display());
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).map_err(|_| fail())?;
        }
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(file)
            .map_err(|_| fail())?;
        out.write_all(key).map_err(|_| fail())
    }

    #[cfg(test)]
    pub fn use_key_file(dir: &Path) {
        write_key_file(&dir.join(KEY_FILE), &fresh()).unwrap();
    }
}

#[cfg(windows)]
pub fn key_file_made() -> Option<std::path::PathBuf> {
    None
}

#[cfg(unix)]
pub use imp::key_file_made;
pub use imp::{protect, unprotect};

#[cfg(test)]
mod tests {
    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("canter-protect-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A key file in the folder keeps the tests away from the keychain.
        #[cfg(unix)]
        super::imp::use_key_file(&dir);
        dir
    }

    #[test]
    fn round_trip() {
        let dir = temp_dir("round-trip");
        let sealed = super::protect(b"secret", &dir).unwrap();
        assert!(!sealed.windows(6).any(|w| w == b"secret"));
        assert_eq!(super::unprotect(&sealed, &dir).unwrap(), b"secret");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_damaged_session_does_not_unlock() {
        let dir = temp_dir("damaged");
        let mut sealed = super::protect(b"secret", &dir).unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 1;
        assert!(super::unprotect(&sealed, &dir).is_err());
        assert!(super::unprotect(b"", &dir).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Uses the real keychain, or makes a key file where there is none:
    /// `cargo test live_keychain -- --ignored --nocapture`. With
    /// `CANTER_EXPECT_KEYCHAIN` set, falling back to a key file fails.
    #[cfg(unix)]
    #[test]
    #[ignore]
    fn live_keychain() {
        let dir = std::env::temp_dir().join(format!("canter-protect-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let started = std::time::Instant::now();
        let sealed = super::protect(b"secret", &dir).unwrap();
        assert_eq!(super::unprotect(&sealed, &dir).unwrap(), b"secret");
        match super::key_file_made() {
            Some(file) => println!(
                "no keychain; key file {} ({:?})",
                file.display(),
                started.elapsed()
            ),
            None => println!("keychain used ({:?})", started.elapsed()),
        }
        if std::env::var_os("CANTER_EXPECT_KEYCHAIN").is_some() {
            assert!(
                super::key_file_made().is_none(),
                "the keychain did not answer"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn the_key_file_is_readable_by_its_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("mode");
        let mode = std::fs::metadata(dir.join(super::imp::KEY_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

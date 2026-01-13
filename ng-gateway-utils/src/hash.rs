use bcrypt::{hash, verify};
use md5::Md5;
use sha2::{Digest as Sha256Digest, Sha256};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

/// Hash a password using bcrypt
///
/// # Arguments
/// * `password` - The plaintext password to hash
///
/// # Returns
/// * `String` - The hashed password
///
/// # Example
/// ```
/// use ng_gateway_utils::hash::bcrypt_hash;
///
/// let hashed = bcrypt_hash("my_password");
/// ```
pub fn bcrypt_hash(password: &str) -> String {
    // Using unwrap here since bcrypt errors are very rare with valid input
    hash(password.as_bytes(), 8).unwrap()
}

/// Compare a plaintext password against a hashed password
///
/// # Arguments
/// * `password` - The plaintext password to check
/// * `hash` - The hashed password to compare against
///
/// # Returns
/// * `bool` - True if the passwords match, false otherwise
///
/// # Example
/// ```
/// use ng_gateway_utils::hash::{bcrypt_hash, bcrypt_check};
///
/// let hash = bcrypt_hash("my_password");
/// assert!(bcrypt_check("my_password", &hash));
/// assert!(!bcrypt_check("wrong_password", &hash));
/// ```
pub fn bcrypt_check(password: &str, hash: &str) -> bool {
    verify(password.as_bytes(), hash).unwrap_or(false)
}

/// Calculate MD5 hash of input bytes
///
/// # Arguments
/// * `data` - The bytes to hash
/// * `extra` - Optional additional bytes to include in the hash
///
/// # Returns
/// * `String` - The hexadecimal representation of the MD5 hash
///
/// # Example
/// ```
/// use ng_gateway_utils::hash::md5v;
///
/// let hash = md5v(b"hello world", None);
/// assert_eq!(hash.len(), 32);
/// ```
pub fn md5v(data: &[u8], extra: Option<&[u8]>) -> String {
    let mut hasher = Md5::new();
    hasher.update(data);

    if let Some(extra_data) = extra {
        hasher.update(extra_data);
    }

    let result = hasher.finalize();
    hex::encode(result)
}

/// Calculate SHA-256 hash of input bytes
///
/// Returns lowercase hex string of length 64.
pub fn sha256v(data: &[u8], extra: Option<&[u8]>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    if let Some(extra_data) = extra {
        hasher.update(extra_data);
    }
    let result = hasher.finalize();
    hex::encode(result)
}

/// Calculate SHA-256 hash of input bytes (no extra data)
///
/// Returns lowercase hex string of length 64.
#[inline]
pub fn sha256_bytes(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let result = hasher.finalize();
    hex::encode(result)
}

/// Calculate SHA-256 for a file by streaming to avoid high memory usage
///
/// Returns lowercase hex string of length 64.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 128 * 1024];

    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }

    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bcrypt_hash_and_check() {
        let password = "test_password";
        let hash = bcrypt_hash(password);

        assert!(bcrypt_check(password, &hash));
        assert!(!bcrypt_check("wrong_password", &hash));
    }

    #[test]
    fn test_md5v() {
        let data = b"hello world";
        let hash = md5v(data, None);
        assert_eq!(hash.len(), 32);

        // Test with extra data
        let extra = b"extra";
        let hash_with_extra = md5v(data, Some(extra));
        assert_eq!(hash_with_extra.len(), 32);
        assert_ne!(hash, hash_with_extra);
    }

    #[test]
    fn test_sha256v() {
        let data = b"hello world";
        let hash = sha256v(data, None);
        assert_eq!(hash.len(), 64);
        // Known SHA-256 of "hello world"
        assert_eq!(
            hash,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }
}

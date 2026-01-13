use thiserror::Error;

/// Represents TLS-related errors that can occur during TLS operations
#[derive(Error, Debug)]
pub enum TLSError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Certificate generation error: {0}")]
    Rcgen(#[from] rcgen::Error),
    #[error("Rustls error: {0}")]
    Rustls(#[from] rustls::Error),
    #[error("PEM parsing error (rustls-pemfile): {0}")]
    Pemfile(String),
    #[error("Private key parsing error: {0}")]
    PrivateKeyParseError(String),
    #[error("No private key found in PEM file")]
    NoPrivateKeyInPem,
    #[error("Multiple private keys found in PEM file, expected one")]
    MultiplePrivateKeysInPem,
    #[error("Invalid certificate data: {0}")]
    InvalidCertificateData(String),
    #[error("CA certificate and key paths are required for auto-generation")]
    CaPathsRequired,
    #[error("Certificate or key file not found: cert_path: {cert_path}, key_path: {key_path}")]
    CertOrKeyNotFound { cert_path: String, key_path: String },
    #[error("Could not parse any PEM items from file: {path}")]
    NoPemItemsFound { path: String },
    #[error("TLS configuration error: {0}")]
    ConfigError(String),
    #[error("Failed to acquire file lock for certificate generation: {path}")]
    FileLockError { path: String },
    #[error("UTF-8 conversion error: {0}")]
    Utf8Error(#[from] std::str::Utf8Error),
}

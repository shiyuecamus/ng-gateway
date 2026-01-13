//! TLS utilities for certificate generation and management.
//! This module provides functions to configure Rustls `ServerConfig` for Actix web servers,
//! including automatic generation and loading of CA certificates, server certificates, and private keys.

use chrono::Utc;
use fs2::FileExt;
use ng_gateway_error::{tls::TLSError, NGResult};
use rcgen::{CertificateParams, DistinguishedName, DnType, IsCa, Issuer, KeyPair};
use rustls::{
    pki_types::{CertificateDer, PrivateKeyDer},
    ServerConfig,
};
use rustls_pemfile::{certs, ec_private_keys, pkcs8_private_keys, rsa_private_keys};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufReader, Seek},
    path::Path,
    sync::Arc,
};
use time::OffsetDateTime;

/// Configures a Rustls `ServerConfig` for use with an HTTPS server.
///
/// This function handles the loading of server certificates and private keys from the specified paths.
/// If `is_auto` is true, it will attempt to automatically generate a CA certificate and key (if they
/// don't exist or are invalid) and then a server certificate and key signed by this CA.
/// The generated certificates are stored at the provided paths.
///
/// The function performs file locking to prevent race conditions when multiple instances might
/// attempt to generate certificates simultaneously.
///
/// # Arguments
///
/// * `cert_path_str`: Path to the server certificate PEM file.
/// * `key_path_str`: Path to the server private key PEM file.
/// * `is_auto`: If `true`, enables automatic generation of certificates if they are missing or invalid.
///   Requires `ca_cert_path_str` and `ca_key_path_str` to be `Some`.
/// * `ca_cert_path_str`: Optional path to the CA certificate PEM file. Required if `is_auto` is `true`.
/// * `ca_key_path_str`: Optional path to the CA private key PEM file. Required if `is_auto` is `true`.
///
/// # Returns
///
/// Returns a `NGResult<ServerConfig, TLSError>`:
/// * `Ok(ServerConfig)`: The configured Rustls `ServerConfig`.
/// * `Err(TLSError)`: An error occurred during configuration, such as:
///     - `TLSError::CaPathsRequired`: If `is_auto` is true but CA paths are not provided.
///     - `TLSError::CertOrKeyNotFound`: If `is_auto` is false and cert/key files are not found.
///     - `TLSError::Io`: An I/O error occurred while reading files.
///     - `TLSError::NoPemItemsFound`: No PEM items found in the certificate file.
///     - `TLSError::MultiplePrivateKeysInPem`: Multiple private keys found in the key file.
///     - `TLSError::NoPrivateKeyInPem`: No private key found in the key file.
///     - `TLSError::ConfigError`: Errors related to `rcgen` or Rustls configuration.
///     - `TLSError::Rustls`: Errors from the `rustls` library during ServerConfig creation.
///     - `TLSError::FileLockError`: Failed to acquire a file lock for certificate generation.
///     - `TLSError::Utf8Error`: PEM content was not valid UTF-8.
pub fn configure_rustls_server_config(
    cert_path_str: &str,
    key_path_str: &str,
    is_auto: bool,
    ca_cert_path_str: Option<&str>,
    ca_key_path_str: Option<&str>,
) -> NGResult<ServerConfig, TLSError> {
    let cert_path = Path::new(cert_path_str);
    let key_path = Path::new(key_path_str);

    if is_auto {
        let ca_cert_p = ca_cert_path_str.ok_or(TLSError::CaPathsRequired)?;
        let ca_key_p = ca_key_path_str.ok_or(TLSError::CaPathsRequired)?;
        // Ensure parent directories for certificates exist before attempting generation/loading.
        if let Some(parent) = cert_path.parent() {
            fs::create_dir_all(parent).map_err(TLSError::Io)?;
        }
        if let Some(parent) = key_path.parent() {
            fs::create_dir_all(parent).map_err(TLSError::Io)?;
        }
        // CA paths directories will be created within generate_or_load_ca if needed.
        generate_or_load_certificate(cert_path_str, key_path_str, ca_cert_p, ca_key_p)?;
    } else if !cert_path.exists() || !key_path.exists() {
        return Err(TLSError::CertOrKeyNotFound {
            cert_path: cert_path_str.to_string(),
            key_path: key_path_str.to_string(),
        });
    }

    let cert_file = File::open(cert_path).map_err(TLSError::Io)?;
    let mut cert_reader = BufReader::new(cert_file);
    let cert_chain = certs(&mut cert_reader)
        .map(|item_result| item_result.map_err(TLSError::Io))
        .collect::<Result<Vec<CertificateDer<'static>>, TLSError>>()?;

    if cert_chain.is_empty() {
        return Err(TLSError::NoPemItemsFound {
            path: cert_path_str.to_string(),
        });
    }

    let key_file = File::open(key_path).map_err(TLSError::Io)?;
    let mut key_reader = BufReader::new(key_file);

    // Attempt to parse private key, trying PKCS8, then PKCS1, then SEC1 formats.
    let private_key = {
        // Try PKCS8
        let pkcs8_keys = pkcs8_private_keys(&mut key_reader)
            .map(|item_result| item_result.map_err(TLSError::Io))
            .collect::<Result<Vec<_>, _>>()?;

        if !pkcs8_keys.is_empty() {
            if pkcs8_keys.len() > 1 {
                return Err(TLSError::MultiplePrivateKeysInPem);
            }
            PrivateKeyDer::Pkcs8(pkcs8_keys.into_iter().next().unwrap())
        } else {
            key_reader
                .seek(std::io::SeekFrom::Start(0))
                .map_err(TLSError::Io)?;
            // Try PKCS1 (RSA)
            let rsa_keys = rsa_private_keys(&mut key_reader)
                .map(|item_result| item_result.map_err(TLSError::Io))
                .collect::<Result<Vec<_>, _>>()?;

            if !rsa_keys.is_empty() {
                if rsa_keys.len() > 1 {
                    return Err(TLSError::MultiplePrivateKeysInPem);
                }
                PrivateKeyDer::Pkcs1(rsa_keys.into_iter().next().unwrap())
            } else {
                key_reader
                    .seek(std::io::SeekFrom::Start(0))
                    .map_err(TLSError::Io)?;
                // Try SEC1 (EC)
                let ec_keys = ec_private_keys(&mut key_reader)
                    .map(|item_result| item_result.map_err(TLSError::Io))
                    .collect::<Result<Vec<_>, _>>()?;

                if !ec_keys.is_empty() {
                    if ec_keys.len() > 1 {
                        return Err(TLSError::MultiplePrivateKeysInPem);
                    }
                    PrivateKeyDer::Sec1(ec_keys.into_iter().next().unwrap())
                } else {
                    return Err(TLSError::NoPrivateKeyInPem);
                }
            }
        }
    };

    let crypto_provider = Arc::new(rustls::crypto::ring::default_provider());

    ServerConfig::builder_with_provider(crypto_provider)
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
        .map_err(|e| TLSError::ConfigError(e.to_string()))?
        .with_no_client_auth()
        .with_single_cert(cert_chain, private_key)
        .map_err(TLSError::Rustls)
}

/// Generates a new CA certificate and private key or loads them if they already exist and are valid.
///
/// If the files at `ca_path_str` or `key_path_str` do not exist, or if they exist but are empty,
/// new ones will be generated. The generation process is protected by a file lock on `ca_path_str`
/// to prevent race conditions.
///
/// The generated CA is self-signed and configured with common distinguished name fields.
/// It is valid for 10 years (3650 days).
///
/// # Arguments
///
/// * `ca_path_str`: Path where the CA certificate PEM file should be stored/loaded from.
/// * `key_path_str`: Path where the CA private key PEM file should be stored/loaded from.
///
/// # Returns
///
/// Returns a `NGResult<(Vec<u8>, Vec<u8>), TLSError>`:
/// * `Ok((Vec<u8>, Vec<u8>))`: A tuple containing the CA certificate PEM bytes and CA private key PEM bytes.
/// * `Err(TLSError)`: An error occurred, such as:
///     - `TLSError::Io`: An I/O error during file operations.
///     - `TLSError::FileLockError`: Failed to acquire a file lock.
///     - `TLSError::Rcgen`: Error during certificate generation via `rcgen`.
///     - `TLSError::ConfigError`: Error converting timestamps for certificate validity.
// Note: The lock_target_path is ca_path. Its parent directory is created below.
// Parent directory for key_path is also created.
pub fn generate_or_load_ca(
    ca_path_str: &str,
    key_path_str: &str,
) -> NGResult<(Vec<u8>, Vec<u8>), TLSError> {
    let ca_path = Path::new(ca_path_str);
    let key_path = Path::new(key_path_str);

    // Ensure parent directories exist before attempting to create/lock/write files.
    if let Some(parent) = ca_path.parent() {
        fs::create_dir_all(parent).map_err(TLSError::Io)?;
    }
    if let Some(parent) = key_path.parent() {
        // Only create if different from ca_path's parent to avoid redundant call
        if ca_path.parent() != key_path.parent() {
            fs::create_dir_all(parent).map_err(TLSError::Io)?;
        }
    }

    // Use ca_path for locking, as it's the primary artifact (certificate).
    // Use a lock file rather than locking the target file itself to avoid issues on Windows
    let lock_path = ca_path.with_extension("lock");
    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&lock_path) // Use a separate lock file
        .map_err(TLSError::Io)?;

    // Try to lock, but continue if it fails (could be a permission issue on Windows)
    let lock_result = lock_file.lock_exclusive();
    if let Err(e) = &lock_result {
        eprintln!(
            "Warning: Could not acquire lock on {}: {:?}, continuing anyway",
            lock_path.display(),
            e
        );
    }

    let mut perform_generation = !ca_path.exists() || !key_path.exists();

    if !perform_generation {
        let cert_pem_vec = fs::read(ca_path).map_err(TLSError::Io)?;
        let key_pem_vec = fs::read(key_path).map_err(TLSError::Io)?;
        if cert_pem_vec.is_empty() || key_pem_vec.is_empty() {
            // If existing files are empty, remove them and trigger generation.
            let _ = fs::remove_file(ca_path);
            let _ = fs::remove_file(key_path);
            perform_generation = true;
        } else {
            // Files exist and are not empty, so load them.
            return Ok((cert_pem_vec, key_pem_vec));
        }
    }

    if perform_generation {
        // Parameters for the new CA certificate.
        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        params.key_usages = vec![
            rcgen::KeyUsagePurpose::KeyCertSign,
            rcgen::KeyUsagePurpose::CrlSign,
            rcgen::KeyUsagePurpose::DigitalSignature,
        ];
        params.distinguished_name = DistinguishedName::new();
        params.distinguished_name.push(DnType::CountryName, "CN");
        params
            .distinguished_name
            .push(DnType::OrganizationName, "NG");
        params
            .distinguished_name
            .push(DnType::OrganizationalUnitName, "NG Development CA");
        params
            .distinguished_name
            .push(DnType::StateOrProvinceName, "Hubei");
        params
            .distinguished_name
            .push(DnType::CommonName, "NG CA"); // Common Name for the CA
        params
            .distinguished_name
            .push(DnType::LocalityName, "Wuhan");

        // Validity period for the CA certificate (10 years).
        let now = Utc::now();
        params.not_before = OffsetDateTime::from_unix_timestamp(now.timestamp()).map_err(|e| {
            TLSError::ConfigError(format!("Failed to convert CA not_before time: {e}"))
        })?;
        let not_after_chrono = now + chrono::Duration::days(3650);
        params.not_after = OffsetDateTime::from_unix_timestamp(not_after_chrono.timestamp())
            .map_err(|e| {
                TLSError::ConfigError(format!("Failed to convert CA not_after time: {e}"))
            })?;

        let ca_key_pair = KeyPair::generate().map_err(TLSError::Rcgen)?;
        let ca_cert_obj = params.self_signed(&ca_key_pair).map_err(TLSError::Rcgen)?;
        let cert_pem_string = ca_cert_obj.pem();
        let key_pem_string = ca_key_pair.serialize_pem();

        fs::write(ca_path, cert_pem_string.as_bytes()).map_err(TLSError::Io)?;
        fs::write(key_path, key_pem_string.as_bytes()).map_err(TLSError::Io)?;
        Ok((cert_pem_string.into_bytes(), key_pem_string.into_bytes()))
    } else {
        // This path should ideally not be reached due to the logic above ensuring either loading or generation.
        Err(TLSError::ConfigError(
            "Failed to load or generate CA certificate and key logic error".to_string(),
        ))
    }
}

/// Generates a new server certificate and private key signed by a CA, or loads them if they exist.
///
/// This function is typically called when `is_auto` is true in `configure_rustls_server_config`.
/// It first ensures the CA certificate and key are available (using `generate_or_load_ca`).
/// Then, if the server certificate or key at `cert_path_str` or `key_path_str` do not exist
/// or are empty, it generates new ones signed by the loaded/generated CA.
///
/// File locking is performed on `cert_path_str` during this process.
/// The generated server certificate is valid for 1 year (365 days) and includes
/// "ng.local" and "localhost" as subject alternative names.
///
/// # Arguments
///
/// * `cert_path_str`: Path for the server certificate PEM file.
/// * `key_path_str`: Path for the server private key PEM file.
/// * `ca_cert_path_str`: Path to the CA certificate PEM file (used for signing).
/// * `ca_key_path_str`: Path to the CA private key PEM file (used for signing).
///
/// # Returns
///
/// Returns a `NGResult<(), TLSError>`:
/// * `Ok(())`: Indicates successful generation/loading of the server certificate and key.
/// * `Err(TLSError)`: An error occurred, such as:
///     - Errors propagated from `generate_or_load_ca`.
///     - `TLSError::ConfigError`: If loaded CA PEMs are empty or for timestamp conversion issues.
///     - `TLSError::Utf8Error`: If CA PEM content is not valid UTF-8.
///     - `TLSError::Rcgen`: Error during server certificate generation.
///     - `TLSError::Io`: An I/O error during file operations.
///     - `TLSError::FileLockError`: Failed to acquire a file lock.
fn generate_or_load_certificate(
    cert_path_str: &str,
    key_path_str: &str,
    ca_cert_path_str: &str,
    ca_key_path_str: &str,
) -> NGResult<(), TLSError> {
    let cert_path = Path::new(cert_path_str);
    let key_path = Path::new(key_path_str);

    // Parent directories for cert_path and key_path are created in the calling function
    // (configure_rustls_server_config) if is_auto is true, before this function is called.
    // So, no need to create them again here explicitly unless this function's contract changes.

    // Use cert_path for locking, as it's the primary artifact being managed here.
    // Use a lock file rather than locking the target file itself to avoid issues on Windows
    let lock_path = cert_path.with_extension("lock");
    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&lock_path) // Use a separate lock file
        .map_err(TLSError::Io)?;

    // Try to lock, but continue if it fails (could be a permission issue on Windows)
    let lock_result = lock_file.lock_exclusive();
    if let Err(e) = &lock_result {
        eprintln!(
            "Warning: Could not acquire lock on {}: {:?}, continuing anyway",
            lock_path.display(),
            e
        );
    }

    // Determine if server certificate generation is needed.
    let perform_server_cert_generation = if cert_path.exists() && key_path.exists() {
        // If both exist, check if they are empty. If so, generation is needed.
        fs::metadata(cert_path).map_err(TLSError::Io)?.len() == 0
            || fs::metadata(key_path).map_err(TLSError::Io)?.len() == 0
    } else {
        // If one or both do not exist, generation is needed.
        true
    };

    if !perform_server_cert_generation {
        // Files exist and are not empty, assume valid and return.
        return Ok(());
    }

    // Proceed with generation.
    let (ca_cert_pem_vec, ca_key_pem_vec) = generate_or_load_ca(ca_cert_path_str, ca_key_path_str)?;

    if ca_cert_pem_vec.is_empty() {
        return Err(TLSError::ConfigError(
            "Loaded CA certificate PEM is empty, cannot sign server certificate".to_string(),
        ));
    }
    if ca_key_pem_vec.is_empty() {
        return Err(TLSError::ConfigError(
            "Loaded CA key PEM is empty, cannot sign server certificate".to_string(),
        ));
    }

    let ca_key_pem_str = std::str::from_utf8(&ca_key_pem_vec).map_err(TLSError::Utf8Error)?;
    let ca_cert_pem_str = std::str::from_utf8(&ca_cert_pem_vec).map_err(TLSError::Utf8Error)?;

    let ca_key_pair_for_signing = KeyPair::from_pem(ca_key_pem_str).map_err(TLSError::Rcgen)?;
    // Create an Issuer from CA cert PEM and the CA's private key
    let ca_issuer = Issuer::from_ca_cert_pem(ca_cert_pem_str, ca_key_pair_for_signing)
        .map_err(TLSError::Rcgen)?;

    let server_generated_key_pair = KeyPair::generate().map_err(TLSError::Rcgen)?;

    // Parameters for the new server certificate.
    let mut server_params =
        CertificateParams::new(vec!["ng.local".to_string(), "localhost".to_string()])
            .map_err(TLSError::Rcgen)?; // Subject Alternative Names (SANs)

    server_params.distinguished_name = DistinguishedName::new();
    server_params
        .distinguished_name
        .push(DnType::CountryName, "CN");
    server_params
        .distinguished_name
        .push(DnType::OrganizationName, "NG");
    server_params
        .distinguished_name
        .push(DnType::OrganizationalUnitName, "NG Server");
    server_params
        .distinguished_name
        .push(DnType::StateOrProvinceName, "Hubei");
    server_params
        .distinguished_name
        .push(DnType::CommonName, "ng.local"); // Common Name for the server
    server_params
        .distinguished_name
        .push(DnType::LocalityName, "Wuhan");

    server_params.key_usages = vec![
        rcgen::KeyUsagePurpose::DigitalSignature,
        rcgen::KeyUsagePurpose::KeyEncipherment,
    ];
    server_params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];

    // Validity period for the server certificate (1 year).
    let now = Utc::now();
    server_params.not_before =
        OffsetDateTime::from_unix_timestamp(now.timestamp()).map_err(|e| {
            TLSError::ConfigError(format!("Failed to convert server not_before time: {e}"))
        })?;
    let not_after_chrono = now + chrono::Duration::days(365);
    server_params.not_after = OffsetDateTime::from_unix_timestamp(not_after_chrono.timestamp())
        .map_err(|e| {
            TLSError::ConfigError(format!("Failed to convert server not_after time: {e}"))
        })?;

    let server_cert_obj = server_params
        .signed_by(
            &server_generated_key_pair, // The server's public key to be signed
            &ca_issuer,                 // The CA issuer that will sign this server cert
        )
        .map_err(TLSError::Rcgen)?;

    let server_cert_pem_string = server_cert_obj.pem();
    let server_key_pem_string = server_generated_key_pair.serialize_pem();

    fs::write(cert_path, server_cert_pem_string.as_bytes()).map_err(TLSError::Io)?;
    fs::write(key_path, server_key_pem_string.as_bytes()).map_err(TLSError::Io)
}

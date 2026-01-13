//! JSON Web Token (JWT) utilities for encoding and decoding tokens.
use jsonwebtoken::{
    decode, encode, errors::Error as JwtError, Algorithm, DecodingKey, EncodingKey, Header,
    TokenData, Validation,
};
use serde::{de::DeserializeOwned, Serialize};

#[inline]
pub fn encode_jwt<T: Serialize>(
    claims: &T,
    secret: &[u8],
    algorithm: Option<Algorithm>,
) -> Result<String, JwtError> {
    let header = Header::new(algorithm.unwrap_or(Algorithm::HS256));
    encode(&header, claims, &EncodingKey::from_secret(secret))
}

#[inline]
pub fn decode_jwt<T: DeserializeOwned>(
    token: &str,
    secret: &[u8],
    validation: Option<Validation>,
) -> Result<TokenData<T>, JwtError> {
    let validation = validation.unwrap_or_default();
    decode::<T>(token, &DecodingKey::from_secret(secret), &validation)
}

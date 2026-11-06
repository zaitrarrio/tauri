// Copyright 2019-2024 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

//! JWT minting for the store APIs, with no extra JWT dependency:
//! ES256 (P-256 ECDSA, IEEE P1363 signature) for App Store Connect and
//! RS256 (RSA PKCS#1 v1.5 + SHA-256) for Google service accounts.

use crate::Result;
use base64::Engine;

fn b64url(data: &[u8]) -> String {
  base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
}

fn signing_input(header: &serde_json::Value, claims: &serde_json::Value) -> Result<String> {
  let encode = |v: &serde_json::Value| -> Result<String> {
    Ok(b64url(
      serde_json::to_string(v)
        .map_err(|e| crate::Error::GenericError(e.to_string()))?
        .as_bytes(),
    ))
  };
  Ok(format!("{}.{}", encode(header)?, encode(claims)?))
}

fn now_unix() -> u64 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|d| d.as_secs())
    .unwrap_or(0)
}

/// App Store Connect token: ES256, `kid` = key id, `iss` = issuer id,
/// `aud` = `appstoreconnect-v1`, 19-minute lifetime (Apple caps at 20).
pub fn app_store_connect_jwt(key_id: &str, issuer_id: &str, p8_pem: &str) -> Result<String> {
  use p256::ecdsa::{SigningKey, signature::Signer};
  use p256::pkcs8::DecodePrivateKey;

  let now = now_unix();
  let header = serde_json::json!({ "alg": "ES256", "kid": key_id, "typ": "JWT" });
  let claims = serde_json::json!({
    "iss": issuer_id,
    "iat": now,
    "exp": now + 19 * 60,
    "aud": "appstoreconnect-v1"
  });
  let input = signing_input(&header, &claims)?;

  let signing_key = SigningKey::from_pkcs8_pem(p8_pem)
    .map_err(|e| crate::Error::GenericError(format!("invalid App Store Connect .p8 key: {e}")))?;
  let signature: p256::ecdsa::Signature = signing_key.sign(input.as_bytes());
  Ok(format!("{input}.{}", b64url(&signature.to_bytes())))
}

/// Google service-account JWT (exchanged at the OAuth token endpoint):
/// RS256, `iss` = client email, `aud` = token URI,
/// `scope` = `https://www.googleapis.com/auth/androidpublisher`.
pub fn google_service_account_jwt(
  client_email: &str,
  pkcs8_pem: &str,
  token_uri: &str,
) -> Result<String> {
  use rsa::pkcs1v15::SigningKey;
  use rsa::pkcs8::DecodePrivateKey;
  use rsa::signature::{SignatureEncoding, Signer};
  use sha2::Sha256;

  let now = now_unix();
  let header = serde_json::json!({ "alg": "RS256", "typ": "JWT" });
  let claims = serde_json::json!({
    "iss": client_email,
    "scope": "https://www.googleapis.com/auth/androidpublisher",
    "aud": token_uri,
    "iat": now,
    "exp": now + 3600
  });
  let input = signing_input(&header, &claims)?;

  let private_key = rsa::RsaPrivateKey::from_pkcs8_pem(pkcs8_pem)
    .map_err(|e| crate::Error::GenericError(format!("invalid service-account key: {e}")))?;
  let signing_key = SigningKey::<Sha256>::new(private_key);
  let signature = signing_key.sign(input.as_bytes());
  Ok(format!("{input}.{}", b64url(&signature.to_bytes())))
}

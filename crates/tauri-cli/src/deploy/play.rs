// Copyright 2019-2024 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

//! Google Play submission through the Play Developer API — the Android
//! half of `tauri deploy submit` (what `fastlane supply` / `eas submit`
//! do): exchange a service-account JWT for an access token, open an
//! edit, upload the app bundle, assign it to a track, and commit.
//!
//! The service account needs the Play Console permissions to release to
//! the target track. Note that Play does not expose a track's tester
//! opt-in URL through the API — after the first release to a testing
//! track, the shareable link is shown in Play Console under
//! Testing → the track → Testers.

use super::{expect_ok, http_agent, http_json, jwt};
use crate::Result;

const API: &str = "https://androidpublisher.googleapis.com/androidpublisher/v3";
const UPLOAD_API: &str = "https://androidpublisher.googleapis.com/upload/androidpublisher/v3";

struct ServiceAccount {
  client_email: String,
  private_key: String,
  token_uri: String,
}

fn service_account() -> Result<ServiceAccount> {
  let raw = if let Ok(json) = std::env::var("GOOGLE_PLAY_SERVICE_ACCOUNT_JSON") {
    json
  } else if let Ok(path) = std::env::var("GOOGLE_PLAY_SERVICE_ACCOUNT") {
    std::fs::read_to_string(&path).map_err(|e| {
      crate::Error::GenericError(format!("failed to read service account file {path}: {e}"))
    })?
  } else {
    return super::err(
      "Google Play credentials missing. Set GOOGLE_PLAY_SERVICE_ACCOUNT (path to the \
       service-account JSON) or GOOGLE_PLAY_SERVICE_ACCOUNT_JSON (its contents).",
    );
  };
  let value: serde_json::Value = serde_json::from_str(&raw)
    .map_err(|e| crate::Error::GenericError(format!("invalid service-account JSON: {e}")))?;
  let get = |key: &str| -> Result<String> {
    value
      .get(key)
      .and_then(|v| v.as_str())
      .map(|s| s.to_string())
      .ok_or_else(|| crate::Error::GenericError(format!("service-account JSON is missing `{key}`")))
  };
  Ok(ServiceAccount {
    client_email: get("client_email")?,
    private_key: get("private_key")?,
    token_uri: get("token_uri")?,
  })
}

fn access_token(agent: &ureq::Agent, account: &ServiceAccount) -> Result<String> {
  let assertion = jwt::google_service_account_jwt(
    &account.client_email,
    &account.private_key,
    &account.token_uri,
  )?;
  // The JWT is already URL-safe; the grant type is percent-encoded by hand
  // to avoid pulling in a form-encoding dependency.
  let body = format!(
    "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion={assertion}"
  );
  let mut resp = agent
    .post(&account.token_uri)
    .header("Content-Type", "application/x-www-form-urlencoded")
    .send(body)
    .map_err(|e| crate::Error::GenericError(format!("token request failed: {e}")))?;
  let status = resp.status().as_u16();
  let text = resp.body_mut().read_to_string().unwrap_or_default();
  let json: serde_json::Value =
    serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text.clone()));
  expect_ok("token request", status, &json)?;
  json
    .get("access_token")
    .and_then(|t| t.as_str())
    .map(|s| s.to_string())
    .ok_or_else(|| crate::Error::GenericError("token response had no access_token".to_string()))
}

pub fn submit_aab(package: &str, aab_path: &std::path::Path, track: &str) -> Result<()> {
  let agent = http_agent();
  let account = service_account()?;
  let token = access_token(&agent, &account)?;

  // 1. Open an edit.
  let (status, json) = http_json(
    &agent,
    "POST",
    &format!("{API}/applications/{package}/edits"),
    &token,
    Some(&serde_json::json!({})),
  )?;
  expect_ok("create edit", status, &json)?;
  let edit_id = json["id"]
    .as_str()
    .ok_or_else(|| crate::Error::GenericError("edit response had no id".to_string()))?
    .to_string();

  let result = submit_into_edit(&agent, &token, package, &edit_id, aab_path, track);
  if result.is_err() {
    // Best-effort cleanup so a failed submission does not wedge the
    // app's edit slot.
    let _ = http_json(
      &agent,
      "DELETE",
      &format!("{API}/applications/{package}/edits/{edit_id}"),
      &token,
      None,
    );
  }
  result
}

fn submit_into_edit(
  agent: &ureq::Agent,
  token: &str,
  package: &str,
  edit_id: &str,
  aab_path: &std::path::Path,
  track: &str,
) -> Result<()> {
  // 2. Upload the bundle (media upload; bundles are read fully into
  // memory — acceptable for a release-time CLI operation).
  let bytes = std::fs::read(aab_path).map_err(|e| {
    crate::Error::GenericError(format!("failed to read bundle {}: {e}", aab_path.display()))
  })?;
  let mut resp = agent
    .post(&format!(
      "{UPLOAD_API}/applications/{package}/edits/{edit_id}/bundles?uploadType=media"
    ))
    .header("Authorization", format!("Bearer {token}"))
    .header("Content-Type", "application/octet-stream")
    .send(bytes)
    .map_err(|e| crate::Error::GenericError(format!("bundle upload failed: {e}")))?;
  let status = resp.status().as_u16();
  let text = resp.body_mut().read_to_string().unwrap_or_default();
  let json: serde_json::Value =
    serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text));
  expect_ok("bundle upload", status, &json)?;
  let version_code = json["versionCode"].clone();
  if version_code.is_null() {
    return super::err("bundle upload response had no versionCode");
  }
  log::info!("uploaded bundle versionCode {version_code}");

  // 3. Assign the bundle to the track as a completed release.
  let track_body = serde_json::json!({
    "releases": [{ "status": "completed", "versionCodes": [version_code] }]
  });
  let (status, json) = http_json(
    agent,
    "PUT",
    &format!("{API}/applications/{package}/edits/{edit_id}/tracks/{track}"),
    token,
    Some(&track_body),
  )?;
  expect_ok("assign track", status, &json)?;

  // 4. Commit the edit.
  let (status, json) = http_json(
    agent,
    "POST",
    &format!("{API}/applications/{package}/edits/{edit_id}:commit"),
    token,
    None,
  )?;
  expect_ok("commit edit", status, &json)?;

  println!("Released {package} (versionCode {version_code}) to the Play \"{track}\" track.");
  println!(
    "Testers install it from the track's opt-in link in Play Console (Testing → {track}); \
     Google does not expose that link through the API."
  );
  Ok(())
}

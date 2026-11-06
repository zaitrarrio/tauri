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

/// How a Play release should be published.
pub struct ReleaseOptions {
  /// completed | draft | halted | inProgress
  pub status: String,
  /// Rollout fraction for inProgress releases (0.0–1.0).
  pub user_fraction: Option<f64>,
  pub release_notes: Option<String>,
  pub locale: String,
}

impl Default for ReleaseOptions {
  fn default() -> Self {
    Self {
      status: "completed".to_string(),
      user_fraction: None,
      release_notes: None,
      locale: "en-US".to_string(),
    }
  }
}

/// Authenticated (agent, access token) pair for the Play API.
pub(crate) fn client() -> Result<(ureq::Agent, String)> {
  let agent = http_agent();
  let account = service_account()?;
  let token = access_token(&agent, &account)?;
  Ok((agent, token))
}

pub fn submit_aab(
  package: &str,
  aab_path: &std::path::Path,
  track: &str,
  release: &ReleaseOptions,
) -> Result<()> {
  let (agent, token) = client()?;

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

  let result = submit_into_edit(&agent, &token, package, &edit_id, aab_path, track, release);
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
  release_options: &ReleaseOptions,
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

  // 3. Assign the bundle to the track with the requested release shape:
  // completed (full rollout), draft, halted, or inProgress (staged
  // rollout at --user-fraction), plus optional localized release notes.
  let mut release = serde_json::json!({
    "status": release_options.status,
    "versionCodes": [version_code]
  });
  if release_options.status == "inProgress" {
    if let Some(fraction) = release_options.user_fraction {
      release["userFraction"] = serde_json::json!(fraction);
    }
  }
  if let Some(notes) = &release_options.release_notes {
    release["releaseNotes"] = serde_json::json!([
      { "language": release_options.locale, "text": notes }
    ]);
  }
  let track_body = serde_json::json!({ "releases": [release] });
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

// ------------------------------------------------------------- edits -----

fn create_edit(agent: &ureq::Agent, token: &str, package: &str) -> Result<String> {
  let (status, json) = http_json(
    agent,
    "POST",
    &format!("{API}/applications/{package}/edits"),
    token,
    Some(&serde_json::json!({})),
  )?;
  expect_ok("create edit", status, &json)?;
  json["id"]
    .as_str()
    .map(|s| s.to_string())
    .ok_or_else(|| crate::Error::GenericError("edit response had no id".to_string()))
}

fn delete_edit(agent: &ureq::Agent, token: &str, package: &str, edit_id: &str) {
  let _ = http_json(
    agent,
    "DELETE",
    &format!("{API}/applications/{package}/edits/{edit_id}"),
    token,
    None,
  );
}

fn get_track(
  agent: &ureq::Agent,
  token: &str,
  package: &str,
  edit_id: &str,
  track: &str,
) -> Result<serde_json::Value> {
  let (status, json) = http_json(
    agent,
    "GET",
    &format!("{API}/applications/{package}/edits/{edit_id}/tracks/{track}"),
    token,
    None,
  )?;
  expect_ok("get track", status, &json)?;
  Ok(json.clone())
}

fn put_track(
  agent: &ureq::Agent,
  token: &str,
  package: &str,
  edit_id: &str,
  track: &str,
  track_body: &serde_json::Value,
) -> Result<()> {
  let (status, json) = http_json(
    agent,
    "PUT",
    &format!("{API}/applications/{package}/edits/{edit_id}/tracks/{track}"),
    token,
    Some(track_body),
  )?;
  expect_ok("update track", status, &json)?;
  Ok(())
}

fn commit_edit(agent: &ureq::Agent, token: &str, package: &str, edit_id: &str) -> Result<()> {
  let (status, json) = http_json(
    agent,
    "POST",
    &format!("{API}/applications/{package}/edits/{edit_id}:commit"),
    token,
    None,
  )?;
  expect_ok("commit edit", status, &json)?;
  Ok(())
}

/// Run `f` inside a fresh edit, committing on success and deleting the
/// edit on failure.
fn with_edit<T>(package: &str, f: impl FnOnce(&ureq::Agent, &str, &str) -> Result<T>) -> Result<T> {
  let (agent, token) = client()?;
  let edit_id = create_edit(&agent, &token, package)?;
  match f(&agent, &token, &edit_id) {
    Ok(value) => {
      commit_edit(&agent, &token, package, &edit_id)?;
      Ok(value)
    }
    Err(e) => {
      delete_edit(&agent, &token, package, &edit_id);
      Err(e)
    }
  }
}

fn active_release(track_json: &serde_json::Value) -> Result<serde_json::Value> {
  track_json["releases"]
    .as_array()
    .and_then(|releases| {
      releases
        .iter()
        .find(|r| r["status"].as_str() != Some("draft"))
        .or_else(|| releases.first())
    })
    .cloned()
    .ok_or_else(|| crate::Error::GenericError("track has no releases to modify".to_string()))
}

/// Halt or resume the active release on a track (staged-rollout control).
pub fn set_release_status(
  package: &str,
  track: &str,
  status: &str,
  user_fraction: Option<f64>,
) -> Result<()> {
  with_edit(package, |agent, token, edit_id| {
    let track_json = get_track(agent, token, package, edit_id, track)?;
    let mut release = active_release(&track_json)?;
    release["status"] = serde_json::json!(status);
    if status == "inProgress" {
      if let Some(fraction) = user_fraction {
        release["userFraction"] = serde_json::json!(fraction);
      }
    } else {
      // userFraction is only valid on inProgress releases.
      if let Some(obj) = release.as_object_mut() {
        obj.remove("userFraction");
      }
    }
    put_track(
      agent,
      token,
      package,
      edit_id,
      track,
      &serde_json::json!({ "releases": [release] }),
    )
  })?;
  println!("Play track \"{track}\" release is now {status}.");
  Ok(())
}

/// Promote the active release from one track to another (e.g. internal →
/// production), the Play equivalent of moving a build through channels.
pub fn promote_release(package: &str, from_track: &str, to_track: &str) -> Result<()> {
  with_edit(package, |agent, token, edit_id| {
    let from = get_track(agent, token, package, edit_id, from_track)?;
    let source = active_release(&from)?;
    let version_codes = source["versionCodes"].clone();
    if version_codes.is_null() {
      return super::err("source track release has no versionCodes");
    }
    let release = serde_json::json!({
      "status": "completed",
      "versionCodes": version_codes
    });
    put_track(
      agent,
      token,
      package,
      edit_id,
      to_track,
      &serde_json::json!({ "releases": [release] }),
    )
  })?;
  println!("Promoted the \"{from_track}\" release to \"{to_track}\".");
  Ok(())
}

/// Upload an APK/AAB to Play **internal app sharing** and return the
/// shareable download URL — Android's closest equivalent of a TestFlight
/// public link (testers install without the app being on a track).
pub fn upload_internal_app_sharing(package: &str, artifact: &std::path::Path) -> Result<String> {
  let (agent, token) = client()?;
  let kind = match artifact.extension().and_then(|e| e.to_str()) {
    Some("aab") => "bundle",
    Some("apk") => "apk",
    _ => return super::err("internal app sharing needs an .apk or .aab artifact"),
  };
  let bytes = std::fs::read(artifact).map_err(|e| {
    crate::Error::GenericError(format!("failed to read {}: {e}", artifact.display()))
  })?;
  let mut resp = agent
    .post(&format!(
      "{UPLOAD_API}/applications/{package}/internalappsharing/artifacts/{kind}?uploadType=media"
    ))
    .header("Authorization", format!("Bearer {token}"))
    .header("Content-Type", "application/octet-stream")
    .send(bytes)
    .map_err(|e| crate::Error::GenericError(format!("internal app sharing upload failed: {e}")))?;
  let status = resp.status().as_u16();
  let text = resp.body_mut().read_to_string().unwrap_or_default();
  let json: serde_json::Value =
    serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text));
  expect_ok("internal app sharing upload", status, &json)?;
  json["downloadUrl"]
    .as_str()
    .map(|s| s.to_string())
    .ok_or_else(|| {
      crate::Error::GenericError("internal app sharing response had no downloadUrl".to_string())
    })
}

/// Upsert one locale of the Play store listing.
pub fn upsert_listing(
  package: &str,
  locale: &str,
  title: &str,
  short_description: &str,
  full_description: &str,
) -> Result<()> {
  with_edit(package, |agent, token, edit_id| {
    let body = serde_json::json!({
      "title": title,
      "shortDescription": short_description,
      "fullDescription": full_description
    });
    let (status, json) = http_json(
      agent,
      "PUT",
      &format!("{API}/applications/{package}/edits/{edit_id}/listings/{locale}"),
      token,
      Some(&body),
    )?;
    expect_ok("upsert listing", status, &json)?;
    Ok(())
  })
}

// Copyright 2019-2024 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

//! App Store Connect API client for the TestFlight half of `tauri deploy`:
//! find the app, list builds, create/patch beta groups, assign builds,
//! submit for Beta App Review, and enable + read a group's **public link**
//! (`https://testflight.apple.com/join/…`).
//!
//! Note Apple's rule: a public link admits testers only after the group's
//! first build has passed Beta App Review. Until then the group can be
//! created and the link enabled, but Apple does not populate the link
//! value — the commands below report that state instead of failing.

use super::{config, expect_ok, http_agent, http_json, jwt};
use crate::Result;
use base64::Engine;
use clap::Parser;
use std::path::PathBuf;

const BASE: &str = "https://api.appstoreconnect.apple.com/v1";

#[derive(Debug, Parser)]
pub struct LinkOptions {
  /// Deploy profile (deploy.json submit config) for group + limit defaults
  #[clap(long, default_value = "preview")]
  pub profile: String,
  /// External beta group name (default: from the profile, e.g. "Preview")
  #[clap(long)]
  pub group: Option<String>,
  /// Cap on testers who may join through the public link (1–10000)
  #[clap(long)]
  pub limit: Option<u32>,
  /// Build id to assign to the group before enabling the link
  #[clap(long)]
  pub build_id: Option<String>,
  /// Assign the newest fully-processed (VALID) build instead of a build id
  #[clap(long)]
  pub latest: bool,
  /// Submit the assigned build for Beta App Review (required before a
  /// public link admits testers)
  #[clap(long)]
  pub submit_review: bool,
  /// Path to the Tauri directory (where tauri.conf.json lives)
  #[clap(long, default_value = "src-tauri")]
  pub tauri_dir: PathBuf,
}

#[derive(Debug, Parser)]
pub struct StatusOptions {
  /// Path to the Tauri directory (where tauri.conf.json lives)
  #[clap(long, default_value = "src-tauri")]
  pub tauri_dir: PathBuf,
}

struct Credentials {
  key_id: String,
  issuer_id: String,
  p8_pem: String,
}

fn credentials() -> Result<Credentials> {
  let key_id = std::env::var("APPLE_API_KEY")
    .or_else(|_| std::env::var("APP_STORE_CONNECT_KEY_ID"))
    .unwrap_or_default();
  let issuer_id = std::env::var("APPLE_API_ISSUER")
    .or_else(|_| std::env::var("APP_STORE_CONNECT_ISSUER_ID"))
    .unwrap_or_default();

  let mut p8_pem = String::new();
  if let Ok(path) = std::env::var("APPLE_API_KEY_PATH") {
    if let Ok(contents) = std::fs::read_to_string(&path) {
      p8_pem = contents;
    }
  }
  if p8_pem.is_empty() && !key_id.is_empty() {
    // altool's conventional location
    if let Some(home) = dirs::home_dir() {
      let path = home
        .join("private_keys")
        .join(format!("AuthKey_{key_id}.p8"));
      if let Ok(contents) = std::fs::read_to_string(&path) {
        p8_pem = contents;
      }
    }
  }
  if p8_pem.is_empty() {
    if let Ok(b64) = std::env::var("APP_STORE_CONNECT_API_KEY_BASE64") {
      if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64.trim()) {
        p8_pem = String::from_utf8_lossy(&bytes).into_owned();
      }
    }
  }

  if key_id.is_empty() || issuer_id.is_empty() || p8_pem.is_empty() {
    return super::err(
      "App Store Connect credentials missing. Set APPLE_API_KEY, APPLE_API_ISSUER and \
       APPLE_API_KEY_PATH (or APP_STORE_CONNECT_API_KEY_BASE64) — see `tauri deploy` docs.",
    );
  }
  Ok(Credentials {
    key_id,
    issuer_id,
    p8_pem,
  })
}

pub struct Client {
  agent: ureq::Agent,
  token: String,
}

impl Client {
  pub(crate) fn new() -> Result<Self> {
    let creds = credentials()?;
    let token = jwt::app_store_connect_jwt(&creds.key_id, &creds.issuer_id, &creds.p8_pem)?;
    Ok(Self {
      agent: http_agent(),
      token,
    })
  }

  fn call(
    &self,
    method: &str,
    path: &str,
    body: Option<&serde_json::Value>,
  ) -> Result<serde_json::Value> {
    let (status, json) = http_json(
      &self.agent,
      method,
      &format!("{BASE}{path}"),
      &self.token,
      body,
    )?;
    let json = expect_ok(&format!("{method} {path}"), status, &json)?;
    Ok(json.clone())
  }

  pub(crate) fn find_app(&self, bundle_id: &str) -> Result<serde_json::Value> {
    if let Ok(id) = std::env::var("ASC_APP_ID") {
      if !id.is_empty() {
        return Ok(serde_json::json!({ "id": id }));
      }
    }
    let json = self.call(
      "GET",
      &format!("/apps?filter[bundleId]={bundle_id}&limit=1"),
      None,
    )?;
    json
      .get("data")
      .and_then(|d| d.as_array())
      .and_then(|a| a.first())
      .cloned()
      .ok_or_else(|| {
        crate::Error::GenericError(format!(
          "no App Store Connect app found for bundle id {bundle_id}; create the app record first"
        ))
      })
  }

  pub(crate) fn list_builds(&self, app_id: &str) -> Result<Vec<serde_json::Value>> {
    let json = self.call(
      "GET",
      &format!("/builds?filter[app]={app_id}&sort=-uploadedDate&limit=10"),
      None,
    )?;
    Ok(
      json
        .get("data")
        .and_then(|d| d.as_array())
        .cloned()
        .unwrap_or_default(),
    )
  }

  fn list_beta_groups(&self, app_id: &str) -> Result<Vec<serde_json::Value>> {
    let json = self.call("GET", &format!("/apps/{app_id}/betaGroups?limit=200"), None)?;
    Ok(
      json
        .get("data")
        .and_then(|d| d.as_array())
        .cloned()
        .unwrap_or_default(),
    )
  }

  fn create_beta_group(
    &self,
    app_id: &str,
    name: &str,
    internal: bool,
  ) -> Result<serde_json::Value> {
    let body = serde_json::json!({
      "data": {
        "type": "betaGroups",
        "attributes": { "name": name, "isInternalGroup": internal },
        "relationships": { "app": { "data": { "type": "apps", "id": app_id } } }
      }
    });
    let json = self.call("POST", "/betaGroups", Some(&body))?;
    Ok(json["data"].clone())
  }

  fn patch_beta_group(
    &self,
    group_id: &str,
    attributes: serde_json::Value,
  ) -> Result<serde_json::Value> {
    let body = serde_json::json!({
      "data": { "type": "betaGroups", "id": group_id, "attributes": attributes }
    });
    let json = self.call("PATCH", &format!("/betaGroups/{group_id}"), Some(&body))?;
    Ok(json["data"].clone())
  }

  fn add_build_to_group(&self, group_id: &str, build_id: &str) -> Result<()> {
    let body = serde_json::json!({ "data": [{ "type": "builds", "id": build_id }] });
    self.call(
      "POST",
      &format!("/betaGroups/{group_id}/relationships/builds"),
      Some(&body),
    )?;
    Ok(())
  }

  fn submit_beta_review(&self, build_id: &str) -> Result<()> {
    let body = serde_json::json!({
      "data": {
        "type": "betaAppReviewSubmissions",
        "relationships": { "build": { "data": { "type": "builds", "id": build_id } } }
      }
    });
    match self.call("POST", "/betaAppReviewSubmissions", Some(&body)) {
      Ok(_) => Ok(()),
      Err(e) => {
        let msg = e.to_string();
        // Re-submitting an already-submitted/approved build is not an error.
        if msg.contains("already") || msg.contains("DUPLICATE") || msg.contains("STATE") {
          Ok(())
        } else {
          Err(e)
        }
      }
    }
  }

  fn raw_call(
    &self,
    method: &str,
    path: &str,
    body: Option<&serde_json::Value>,
  ) -> Result<(u16, serde_json::Value)> {
    http_json(
      &self.agent,
      method,
      &format!("{BASE}{path}"),
      &self.token,
      body,
    )
  }

  /// The newest build whose processing finished (state VALID), if any.
  pub(crate) fn newest_valid_build(&self, app_id: &str) -> Result<Option<serde_json::Value>> {
    let builds = self.list_builds(app_id)?;
    Ok(
      builds
        .into_iter()
        .find(|b| b["attributes"]["processingState"].as_str() == Some("VALID")),
    )
  }

  /// Ensure the app's Beta App Review detail matches the profile config.
  pub(crate) fn ensure_review_detail(
    &self,
    app_id: &str,
    review: &config::ReviewSection,
  ) -> Result<()> {
    let attrs = review.to_attributes();
    let (status, json) =
      self.raw_call("GET", &format!("/apps/{app_id}/betaAppReviewDetail"), None)?;
    if status == 200 {
      if let Some(id) = json["data"]["id"].as_str() {
        let body = serde_json::json!({
          "data": { "type": "betaAppReviewDetails", "id": id, "attributes": attrs }
        });
        self.call("PATCH", &format!("/betaAppReviewDetails/{id}"), Some(&body))?;
        return Ok(());
      }
    }
    let body = serde_json::json!({
      "data": {
        "type": "betaAppReviewDetails",
        "attributes": attrs,
        "relationships": { "app": { "data": { "type": "apps", "id": app_id } } }
      }
    });
    self.call("POST", "/betaAppReviewDetails", Some(&body))?;
    Ok(())
  }

  /// Set a build's "What to Test" text for a locale (create or update).
  pub(crate) fn upsert_build_localization(
    &self,
    build_id: &str,
    locale: &str,
    whats_new: &str,
  ) -> Result<()> {
    let json = self.call(
      "GET",
      &format!("/builds/{build_id}/betaBuildLocalizations?limit=200"),
      None,
    )?;
    let existing = json["data"].as_array().and_then(|arr| {
      arr
        .iter()
        .find(|l| l["attributes"]["locale"].as_str() == Some(locale))
    });
    if let Some(loc) = existing {
      let id = loc["id"].as_str().unwrap_or_default();
      let body = serde_json::json!({
        "data": {
          "type": "betaBuildLocalizations",
          "id": id,
          "attributes": { "whatsNew": whats_new }
        }
      });
      self.call(
        "PATCH",
        &format!("/betaBuildLocalizations/{id}"),
        Some(&body),
      )?;
    } else {
      let body = serde_json::json!({
        "data": {
          "type": "betaBuildLocalizations",
          "attributes": { "locale": locale, "whatsNew": whats_new },
          "relationships": { "build": { "data": { "type": "builds", "id": build_id } } }
        }
      });
      self.call("POST", "/betaBuildLocalizations", Some(&body))?;
    }
    Ok(())
  }

  /// Create or update the app's TestFlight localization for a locale
  /// (description, feedback email, marketing / privacy URLs).
  pub(crate) fn upsert_app_localization(
    &self,
    app_id: &str,
    locale: &str,
    attributes: serde_json::Value,
  ) -> Result<()> {
    let json = self.call(
      "GET",
      &format!("/apps/{app_id}/betaAppLocalizations?limit=200"),
      None,
    )?;
    let existing = json["data"].as_array().and_then(|arr| {
      arr
        .iter()
        .find(|l| l["attributes"]["locale"].as_str() == Some(locale))
    });
    if let Some(loc) = existing {
      let id = loc["id"].as_str().unwrap_or_default();
      let body = serde_json::json!({
        "data": { "type": "betaAppLocalizations", "id": id, "attributes": attributes }
      });
      self.call("PATCH", &format!("/betaAppLocalizations/{id}"), Some(&body))?;
    } else {
      let mut attrs = attributes;
      attrs["locale"] = serde_json::json!(locale);
      let body = serde_json::json!({
        "data": {
          "type": "betaAppLocalizations",
          "attributes": attrs,
          "relationships": { "app": { "data": { "type": "apps", "id": app_id } } }
        }
      });
      self.call("POST", "/betaAppLocalizations", Some(&body))?;
    }
    Ok(())
  }

  pub(crate) fn list_beta_testers(&self, app_id: &str) -> Result<Vec<serde_json::Value>> {
    let json = self.call(
      "GET",
      &format!("/apps/{app_id}/betaTesters?limit=200"),
      None,
    )?;
    Ok(json["data"].as_array().cloned().unwrap_or_default())
  }

  pub(crate) fn create_beta_tester(
    &self,
    email: &str,
    first_name: &str,
    last_name: &str,
    group_id: Option<&str>,
  ) -> Result<serde_json::Value> {
    let mut data = serde_json::json!({
      "type": "betaTesters",
      "attributes": { "firstName": first_name, "lastName": last_name, "email": email }
    });
    if let Some(group_id) = group_id {
      data["relationships"] = serde_json::json!({
        "betaGroups": { "data": [{ "type": "betaGroups", "id": group_id }] }
      });
    }
    let json = self.call(
      "POST",
      "/betaTesters",
      Some(&serde_json::json!({ "data": data })),
    )?;
    Ok(json["data"].clone())
  }

  pub(crate) fn find_beta_group(
    &self,
    app_id: &str,
    name: &str,
  ) -> Result<Option<serde_json::Value>> {
    let groups = self.list_beta_groups(app_id)?;
    Ok(
      groups
        .into_iter()
        .find(|g| g["attributes"]["name"].as_str() == Some(name)),
    )
  }

  pub(crate) fn list_feedback(&self, app_id: &str) -> Result<Vec<serde_json::Value>> {
    let json = self.call(
      "GET",
      &format!("/apps/{app_id}/betaFeedbackSubmissions?limit=50"),
      None,
    )?;
    Ok(json["data"].as_array().cloned().unwrap_or_default())
  }

  /// The newest App Store version in a state that can be submitted for
  /// review (Prepare for Submission and the rejected states).
  pub(crate) fn find_editable_appstore_version(
    &self,
    app_id: &str,
  ) -> Result<Option<serde_json::Value>> {
    let json = self.call(
      "GET",
      &format!("/apps/{app_id}/appStoreVersions?filter[platform]=IOS&limit=50"),
      None,
    )?;
    let editable = [
      "PREPARE_FOR_SUBMISSION",
      "DEVELOPER_REJECTED",
      "REJECTED",
      "METADATA_REJECTED",
      "INVALID_BINARY",
    ];
    Ok(json["data"].as_array().and_then(|arr| {
      arr
        .iter()
        .find(|v| {
          editable.contains(
            &v["attributes"]["appStoreState"]
              .as_str()
              .unwrap_or_default(),
          )
        })
        .cloned()
    }))
  }

  pub(crate) fn submit_appstore_version(&self, version_id: &str) -> Result<()> {
    let body = serde_json::json!({
      "data": {
        "type": "appStoreVersionSubmissions",
        "relationships": {
          "appStoreVersion": { "data": { "type": "appStoreVersions", "id": version_id } }
        }
      }
    });
    self.call("POST", "/appStoreVersionSubmissions", Some(&body))?;
    Ok(())
  }
}

/// Poll App Store Connect until the newest build finishes processing
/// (state VALID), returning its id. Used by the one-shot submit pipeline.
pub fn wait_for_newest_valid_build(bundle_id: &str, timeout_minutes: u64) -> Result<String> {
  let client = Client::new()?;
  let app = client.find_app(bundle_id)?;
  let app_id = app["id"].as_str().unwrap_or_default().to_string();
  let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_minutes * 60);
  loop {
    if let Some(build) = client.newest_valid_build(&app_id)? {
      let id = build["id"].as_str().unwrap_or_default().to_string();
      log::info!(
        "build {} ({}) finished processing",
        build["attributes"]["version"],
        id
      );
      return Ok(id);
    }
    if std::time::Instant::now() > deadline {
      return super::err(format!(
        "timed out after {timeout_minutes} min waiting for a build to finish processing"
      ));
    }
    log::info!("waiting for App Store Connect processing… (poll every 60s)");
    std::thread::sleep(std::time::Duration::from_secs(60));
  }
}

pub fn link_command(options: LinkOptions) -> Result<()> {
  let resolved = config::load(&options.tauri_dir)?;
  let bundle_id = config::require_bundle_id(&resolved)?;
  let group_name = options
    .group
    .clone()
    .unwrap_or_else(|| resolved.ios_group(&options.profile));
  let limit = options
    .limit
    .or_else(|| resolved.ios_public_link_limit(&options.profile));

  let client = Client::new()?;
  let app = client.find_app(&bundle_id)?;
  let app_id = app["id"].as_str().unwrap_or_default().to_string();

  let groups = client.list_beta_groups(&app_id)?;
  let mut group = groups
    .iter()
    .find(|g| {
      g["attributes"]["name"].as_str() == Some(group_name.as_str())
        && g["attributes"]["isInternalGroup"].as_bool() != Some(true)
    })
    .cloned();
  if group.is_none() {
    group = Some(client.create_beta_group(&app_id, &group_name, false)?);
    log::info!("created external beta group \"{group_name}\"");
  }
  let group = group.unwrap();
  let group_id = group["id"].as_str().unwrap_or_default().to_string();

  let effective_build_id = match (&options.build_id, options.latest) {
    (Some(id), _) => Some(id.clone()),
    (None, true) => Some(
      client
        .newest_valid_build(&app_id)?
        .ok_or_else(|| {
          crate::Error::GenericError(
            "no fully-processed (VALID) build found yet — Apple processing takes 5–30 minutes after upload"
              .to_string(),
          )
        })?["id"]
        .as_str()
        .unwrap_or_default()
        .to_string(),
    ),
    (None, false) => None,
  };

  if let Some(build_id) = &effective_build_id {
    client.add_build_to_group(&group_id, build_id)?;
    log::info!("assigned build {build_id} to group \"{group_name}\"");
    if options.submit_review {
      // A first external submission bounces without review details;
      // apply the profile's when configured, plus its "What to Test".
      if let Some(review) = resolved.ios_review(&options.profile) {
        client.ensure_review_detail(&app_id, review)?;
      }
      if let Some(whats_new) = resolved.ios_whats_new(&options.profile) {
        let _ = client.upsert_build_localization(build_id, "en-US", &whats_new);
      }
      client.submit_beta_review(build_id)?;
      log::info!("submitted build {build_id} for Beta App Review");
    }
  }

  let mut attributes = serde_json::json!({ "publicLinkEnabled": true });
  if let Some(limit) = limit {
    attributes["publicLinkLimitEnabled"] = serde_json::json!(true);
    attributes["publicLinkLimit"] = serde_json::json!(limit);
  }
  let patched = client.patch_beta_group(&group_id, attributes)?;
  let link = patched["attributes"]["publicLink"]
    .as_str()
    .or_else(|| group["attributes"]["publicLink"].as_str());

  match link {
    Some(link) => {
      println!("TestFlight public link for group \"{group_name}\": {link}");
      println!(
        "Anyone with this link can install the beta from TestFlight (subject to the group's tester limit)."
      );
    }
    None => {
      println!(
        "Public link enabled for group \"{group_name}\", but Apple has not populated the link yet."
      );
      println!(
        "Apple fills it in once the group has a build that passed Beta App Review. \
         Assign a build with --build-id --submit-review, wait for approval, then re-run this command."
      );
    }
  }
  Ok(())
}

pub fn status_command(options: StatusOptions) -> Result<()> {
  let resolved = config::load(&options.tauri_dir)?;
  let bundle_id = config::require_bundle_id(&resolved)?;
  let client = Client::new()?;
  let app = client.find_app(&bundle_id)?;
  let app_id = app["id"].as_str().unwrap_or_default().to_string();

  let builds = client.list_builds(&app_id)?;
  let groups = client.list_beta_groups(&app_id)?;
  let out = serde_json::json!({
    "app": { "id": app_id, "bundleId": bundle_id },
    "builds": builds.iter().map(|b| serde_json::json!({
      "id": b["id"],
      "version": b["attributes"]["version"],
      "processingState": b["attributes"]["processingState"],
      "uploadedDate": b["attributes"]["uploadedDate"],
      "expired": b["attributes"]["expired"],
    })).collect::<Vec<_>>(),
    "groups": groups.iter().map(|g| serde_json::json!({
      "id": g["id"],
      "name": g["attributes"]["name"],
      "internal": g["attributes"]["isInternalGroup"],
      "publicLinkEnabled": g["attributes"]["publicLinkEnabled"],
      "publicLink": g["attributes"]["publicLink"],
    })).collect::<Vec<_>>(),
  });
  println!(
    "{}",
    serde_json::to_string_pretty(&out).map_err(|e| crate::Error::GenericError(e.to_string()))?
  );
  Ok(())
}

// ------------------------------------------------------------ review -----

#[derive(Debug, Parser)]
pub struct ReviewOptions {
  /// Deploy profile whose review details + whatsNew to apply
  #[clap(long, default_value = "preview")]
  pub profile: String,
  /// Build id to set "What to Test" on and submit (default: newest VALID)
  #[clap(long)]
  pub build_id: Option<String>,
  /// Locale for the "What to Test" text
  #[clap(long, default_value = "en-US")]
  pub locale: String,
  /// "What to Test" override (default: the profile's whatsNew)
  #[clap(long)]
  pub whats_new: Option<String>,
  /// Only prepare the review detail + localization; do not submit
  #[clap(long)]
  pub prepare_only: bool,
  /// Path to the Tauri directory (where tauri.conf.json lives)
  #[clap(long, default_value = "src-tauri")]
  pub tauri_dir: PathBuf,
}

/// Apply the profile's Beta App Review details, set the build's
/// "What to Test", and (unless --prepare-only) submit it for Beta App
/// Review. Without the review detail, App Store Connect rejects external
/// submissions — this is the step that makes `testflight link` usable for
/// a first external group.
pub fn review_command(options: ReviewOptions) -> Result<()> {
  let resolved = config::load(&options.tauri_dir)?;
  let bundle_id = config::require_bundle_id(&resolved)?;
  let review = resolved.ios_review(&options.profile).ok_or_else(|| {
    crate::Error::GenericError(format!(
      "no review details for profile \"{}\": add submit.{}.ios.review \
       (contactFirstName, contactLastName, contactEmail, contactPhone, …) to deploy.json",
      options.profile, options.profile
    ))
  })?;

  let client = Client::new()?;
  let app = client.find_app(&bundle_id)?;
  let app_id = app["id"].as_str().unwrap_or_default().to_string();

  client.ensure_review_detail(&app_id, review)?;
  log::info!("beta review details saved");

  let build_id = match &options.build_id {
    Some(id) => id.clone(),
    None => client.newest_valid_build(&app_id)?.ok_or_else(|| {
      crate::Error::GenericError("no fully-processed (VALID) build found to prepare".to_string())
    })?["id"]
      .as_str()
      .unwrap_or_default()
      .to_string(),
  };
  let whats_new = options
    .whats_new
    .clone()
    .or_else(|| resolved.ios_whats_new(&options.profile))
    .unwrap_or_else(|| "Bug fixes and improvements.".to_string());
  client.upsert_build_localization(&build_id, &options.locale, &whats_new)?;
  log::info!(
    "\"What to Test\" set for build {build_id} ({})",
    options.locale
  );

  if !options.prepare_only {
    client.submit_beta_review(&build_id)?;
    println!("Build {build_id} submitted for Beta App Review with the profile's review details.");
  } else {
    println!("Review details + \"What to Test\" prepared for build {build_id} (not submitted).");
  }
  Ok(())
}

// ------------------------------------------------------------ testers ----

#[derive(Debug, Parser)]
pub struct TestersListOptions {
  /// Path to the Tauri directory (where tauri.conf.json lives)
  #[clap(long, default_value = "src-tauri")]
  pub tauri_dir: PathBuf,
}

#[derive(Debug, Parser)]
pub struct TesterAddOptions {
  #[clap(long)]
  pub email: String,
  #[clap(long)]
  pub first_name: String,
  #[clap(long)]
  pub last_name: String,
  /// Beta group name to add the tester to (optional)
  #[clap(long)]
  pub group: Option<String>,
  /// Path to the Tauri directory (where tauri.conf.json lives)
  #[clap(long, default_value = "src-tauri")]
  pub tauri_dir: PathBuf,
}

pub fn testers_list_command(options: TestersListOptions) -> Result<()> {
  let resolved = config::load(&options.tauri_dir)?;
  let bundle_id = config::require_bundle_id(&resolved)?;
  let client = Client::new()?;
  let app = client.find_app(&bundle_id)?;
  let app_id = app["id"].as_str().unwrap_or_default();
  let testers = client.list_beta_testers(app_id)?;
  println!(
    "{}",
    serde_json::to_string_pretty(&serde_json::json!(
      testers
        .iter()
        .map(|t| serde_json::json!({
          "id": t["id"],
          "email": t["attributes"]["email"],
          "firstName": t["attributes"]["firstName"],
          "lastName": t["attributes"]["lastName"],
          "inviteType": t["attributes"]["inviteType"],
          "state": t["attributes"]["state"],
        }))
        .collect::<Vec<_>>()
    ))
    .map_err(|e| crate::Error::GenericError(e.to_string()))?
  );
  Ok(())
}

pub fn tester_add_command(options: TesterAddOptions) -> Result<()> {
  let resolved = config::load(&options.tauri_dir)?;
  let bundle_id = config::require_bundle_id(&resolved)?;
  let client = Client::new()?;
  let app = client.find_app(&bundle_id)?;
  let app_id = app["id"].as_str().unwrap_or_default().to_string();
  let group_id = match &options.group {
    Some(name) => Some(
      client
        .find_beta_group(&app_id, name)?
        .ok_or_else(|| crate::Error::GenericError(format!("no beta group named \"{name}\"")))?["id"]
        .as_str()
        .unwrap_or_default()
        .to_string(),
    ),
    None => None,
  };
  let tester = client.create_beta_tester(
    &options.email,
    &options.first_name,
    &options.last_name,
    group_id.as_deref(),
  )?;
  println!(
    "Invited {} (tester id {}).",
    options.email,
    tester["id"].as_str().unwrap_or_default()
  );
  Ok(())
}

// ------------------------------------------------------------ feedback ---

#[derive(Debug, Parser)]
pub struct FeedbackOptions {
  /// Path to the Tauri directory (where tauri.conf.json lives)
  #[clap(long, default_value = "src-tauri")]
  pub tauri_dir: PathBuf,
}

pub fn feedback_command(options: FeedbackOptions) -> Result<()> {
  let resolved = config::load(&options.tauri_dir)?;
  let bundle_id = config::require_bundle_id(&resolved)?;
  let client = Client::new()?;
  let app = client.find_app(&bundle_id)?;
  let app_id = app["id"].as_str().unwrap_or_default();
  let feedback = client.list_feedback(app_id)?;
  println!(
    "{}",
    serde_json::to_string_pretty(&serde_json::json!(
      feedback
        .iter()
        .map(|f| serde_json::json!({
          "id": f["id"],
          "createdDate": f["attributes"]["createdDate"],
          "comment": f["attributes"]["comment"],
          "email": f["attributes"]["email"],
          "deviceModel": f["attributes"]["deviceModel"],
          "osVersion": f["attributes"]["osVersion"],
          "appPlatform": f["attributes"]["appPlatform"],
        }))
        .collect::<Vec<_>>()
    ))
    .map_err(|e| crate::Error::GenericError(e.to_string()))?
  );
  Ok(())
}

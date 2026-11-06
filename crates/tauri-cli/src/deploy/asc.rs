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
  fn new() -> Result<Self> {
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

  fn find_app(&self, bundle_id: &str) -> Result<serde_json::Value> {
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

  fn list_builds(&self, app_id: &str) -> Result<Vec<serde_json::Value>> {
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

  if let Some(build_id) = &options.build_id {
    client.add_build_to_group(&group_id, build_id)?;
    log::info!("assigned build {build_id} to group \"{group_name}\"");
    if options.submit_review {
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

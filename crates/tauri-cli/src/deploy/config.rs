// Copyright 2019-2024 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

//! Deploy configuration resolution.
//!
//! Reads an optional `deploy.json` in the Tauri directory (same directory
//! as `tauri.conf.json`), with EAS-style build profiles and per-profile
//! submit targets:
//!
//! ```json
//! {
//!   "app": { "bundleId": "com.example.app", "androidPackage": "com.example.app" },
//!   "submit": {
//!     "preview":    { "ios": { "group": "Preview" },     "android": { "track": "internal" } },
//!     "production": { "ios": { "group": "Public Beta" }, "android": { "track": "production" } }
//!   }
//! }
//! ```
//!
//! Anything omitted falls back to the app `identifier` from the Tauri
//! config and to the defaults: preview → TestFlight group `Preview` /
//! Play track `internal`; production → group `Public Beta` / track
//! `production`.

use crate::Result;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeployFile {
  #[serde(default)]
  pub app: AppSection,
  #[serde(default)]
  pub submit: std::collections::HashMap<String, ProfileSubmit>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSection {
  pub bundle_id: Option<String>,
  pub android_package: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileSubmit {
  #[serde(default)]
  pub ios: IosSubmit,
  #[serde(default)]
  pub android: AndroidSubmit,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IosSubmit {
  pub group: Option<String>,
  pub public_link_limit: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AndroidSubmit {
  pub track: Option<String>,
}

#[derive(Debug)]
pub struct Resolved {
  /// iOS bundle id / Android package (defaults to the Tauri identifier).
  pub bundle_id: Option<String>,
  pub android_package: Option<String>,
  file: DeployFile,
}

impl Resolved {
  pub fn ios_group(&self, profile: &str) -> String {
    self
      .file
      .submit
      .get(profile)
      .and_then(|p| p.ios.group.clone())
      .unwrap_or_else(|| {
        if profile == "production" {
          "Public Beta".to_string()
        } else {
          "Preview".to_string()
        }
      })
  }

  pub fn ios_public_link_limit(&self, profile: &str) -> Option<u32> {
    self
      .file
      .submit
      .get(profile)
      .and_then(|p| p.ios.public_link_limit)
  }

  pub fn android_track(&self, profile: &str) -> String {
    self
      .file
      .submit
      .get(profile)
      .and_then(|p| p.android.track.clone())
      .unwrap_or_else(|| {
        if profile == "production" {
          "production".to_string()
        } else {
          "internal".to_string()
        }
      })
  }
}

pub fn load(tauri_dir: &Path) -> Result<Resolved> {
  let deploy_path = tauri_dir.join("deploy.json");
  let file: DeployFile = if deploy_path.exists() {
    let raw = std::fs::read_to_string(&deploy_path).map_err(|e| {
      crate::Error::GenericError(format!("failed to read {}: {e}", deploy_path.display()))
    })?;
    serde_json::from_str(&raw).map_err(|e| {
      crate::Error::GenericError(format!("failed to parse {}: {e}", deploy_path.display()))
    })?
  } else {
    DeployFile::default()
  };

  // Identifier fallback from tauri.conf.json (JSON or JSON5; TOML configs
  // can set the ids explicitly in deploy.json instead).
  let identifier = read_tauri_identifier(tauri_dir);

  Ok(Resolved {
    bundle_id: file.app.bundle_id.clone().or_else(|| identifier.clone()),
    android_package: file.app.android_package.clone().or(identifier),
    file,
  })
}

fn read_tauri_identifier(tauri_dir: &Path) -> Option<String> {
  for name in ["tauri.conf.json", "tauri.conf.json5"] {
    let path = tauri_dir.join(name);
    if let Ok(raw) = std::fs::read_to_string(&path) {
      if let Ok(value) = json5::from_str::<serde_json::Value>(&raw) {
        if let Some(id) = value.get("identifier").and_then(|v| v.as_str()) {
          return Some(id.to_string());
        }
      }
    }
  }
  None
}

/// Find the first file with the given extension under `dir`, preferring
/// paths that contain `prefer` (e.g. "release").
pub fn find_artifact(dir: &Path, extension: &str, prefer: &str) -> Option<PathBuf> {
  let mut fallback = None;
  let mut stack = vec![dir.to_path_buf()];
  while let Some(current) = stack.pop() {
    let Ok(entries) = std::fs::read_dir(&current) else {
      continue;
    };
    for entry in entries.flatten() {
      let path = entry.path();
      if path.is_dir() {
        stack.push(path);
      } else if path.extension().and_then(|e| e.to_str()) == Some(extension) {
        if path.to_string_lossy().contains(prefer) {
          return Some(path);
        }
        fallback.get_or_insert(path);
      }
    }
  }
  fallback
}

pub fn require_bundle_id(resolved: &Resolved) -> Result<String> {
  resolved.bundle_id.clone().ok_or_else(|| {
    crate::Error::GenericError(
      "no iOS bundle id: set `identifier` in tauri.conf.json or `app.bundleId` in deploy.json"
        .to_string(),
    )
  })
}

pub fn require_android_package(resolved: &Resolved) -> Result<String> {
  resolved.android_package.clone().ok_or_else(|| {
    crate::Error::GenericError(
      "no Android package: set `identifier` in tauri.conf.json or `app.androidPackage` in deploy.json"
        .to_string(),
    )
  })
}

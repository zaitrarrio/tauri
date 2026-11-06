// Copyright 2019-2024 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

//! `tauri deploy metadata` — sync store listing text from a
//! `metadata.json` next to `tauri.conf.json` (the `eas metadata`
//! equivalent, scoped to what the store APIs allow without an editable
//! store version):
//!
//! ```json
//! {
//!   "ios": {
//!     "locale": "en-US",
//!     "description": "…", "feedbackEmail": "beta@example.com",
//!     "marketingUrl": "https://…", "privacyPolicyUrl": "https://…"
//!   },
//!   "android": {
//!     "listings": {
//!       "en-US": { "title": "…", "shortDescription": "…", "fullDescription": "…" }
//!     }
//!   }
//! }
//! ```
//!
//! The iOS side writes the TestFlight (beta) app localization. Full App
//! Store listing text lives on an app-store version's localizations and
//! is edited in App Store Connect; Play listings sync in full here.

use super::{Platform, asc, config, play};
use crate::Result;
use clap::Parser;
use std::path::PathBuf;

#[derive(Debug, Parser)]
pub struct Options {
  /// Platform to sync (default: both that are present in metadata.json)
  #[clap(value_enum, long)]
  pub platform: Option<Platform>,
  /// Path to the Tauri directory (where tauri.conf.json lives)
  #[clap(long, default_value = "src-tauri")]
  pub tauri_dir: PathBuf,
}

fn load_metadata(tauri_dir: &std::path::Path) -> Result<serde_json::Value> {
  let path = tauri_dir.join("metadata.json");
  let raw = std::fs::read_to_string(&path).map_err(|e| {
    crate::Error::GenericError(format!(
      "failed to read {}: {e} — create it with `tauri deploy init`",
      path.display()
    ))
  })?;
  serde_json::from_str(&raw)
    .map_err(|e| crate::Error::GenericError(format!("failed to parse {}: {e}", path.display())))
}

pub fn command(options: Options) -> Result<()> {
  let metadata = load_metadata(&options.tauri_dir)?;
  let resolved = config::load(&options.tauri_dir)?;
  let want = |p: Platform| match options.platform {
    Some(Platform::Ios) => matches!(p, Platform::Ios),
    Some(Platform::Android) => matches!(p, Platform::Android),
    None => true,
  };

  if want(Platform::Ios) {
    if let Some(ios) = metadata.get("ios") {
      let bundle_id = config::require_bundle_id(&resolved)?;
      let locale = ios["locale"].as_str().unwrap_or("en-US");
      let mut attrs = serde_json::Map::new();
      for key in [
        "description",
        "feedbackEmail",
        "marketingUrl",
        "privacyPolicyUrl",
      ] {
        if let Some(v) = ios.get(key) {
          attrs.insert(key.to_string(), v.clone());
        }
      }
      let client = asc::Client::new()?;
      let app = client.find_app(&bundle_id)?;
      let app_id = app["id"].as_str().unwrap_or_default();
      client.upsert_app_localization(app_id, locale, serde_json::Value::Object(attrs))?;
      println!("iOS TestFlight localization ({locale}) synced.");
    } else if options.platform.is_some() {
      return super::err("metadata.json has no `ios` section");
    }
  }

  if want(Platform::Android) {
    if let Some(listings) = metadata
      .get("android")
      .and_then(|a| a.get("listings"))
      .and_then(|l| l.as_object())
    {
      let package = config::require_android_package(&resolved)?;
      for (locale, listing) in listings {
        let get = |k: &str| listing[k].as_str().unwrap_or_default();
        play::upsert_listing(
          &package,
          locale,
          get("title"),
          get("shortDescription"),
          get("fullDescription"),
        )?;
        println!("Play listing ({locale}) synced.");
      }
    } else if options.platform.is_some() {
      return super::err("metadata.json has no `android.listings` section");
    }
  }

  Ok(())
}

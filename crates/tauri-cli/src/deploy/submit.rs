// Copyright 2019-2024 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

//! `tauri deploy submit` — dispatch to the platform uploaders.

use super::{Platform, config, play};
use crate::Result;
use clap::Parser;
use std::path::PathBuf;

#[derive(Debug, Parser)]
pub struct Options {
  /// Platform to submit
  #[clap(value_enum, long)]
  pub platform: Platform,
  /// Deploy profile (deploy.json submit config): preview, production, …
  #[clap(long, default_value = "preview")]
  pub profile: String,
  /// Artifact to upload (.ipa for iOS, .aab for Android).
  /// Default: the newest matching artifact under the generated project
  /// (`gen/apple/build` for iOS, `gen/android` for Android).
  #[clap(long)]
  pub artifact: Option<PathBuf>,
  /// Play track override (Android only; default from the profile)
  #[clap(long)]
  pub track: Option<String>,
  /// After uploading (iOS only), also ensure the profile's external
  /// TestFlight group public link and print it
  #[clap(long)]
  pub link: bool,
  /// Path to the Tauri directory (where tauri.conf.json lives)
  #[clap(long, default_value = "src-tauri")]
  pub tauri_dir: PathBuf,
}

pub fn command(options: Options) -> Result<()> {
  let resolved = config::load(&options.tauri_dir)?;
  match options.platform {
    Platform::Ios => submit_ios(&options, &resolved),
    Platform::Android => {
      let package = config::require_android_package(&resolved)?;
      let aab = match &options.artifact {
        Some(path) => path.clone(),
        None => config::find_artifact(&options.tauri_dir.join("gen/android"), "aab", "release")
          .ok_or_else(|| {
            crate::Error::GenericError(
              "no .aab found under src-tauri/gen/android — run `tauri android build` first, or pass --artifact"
                .to_string(),
            )
          })?,
      };
      let track = options
        .track
        .clone()
        .unwrap_or_else(|| resolved.android_track(&options.profile));
      play::submit_aab(&package, &aab, &track)
    }
  }
}

fn submit_ios(options: &Options, resolved: &config::Resolved) -> Result<()> {
  let _ = config::require_bundle_id(resolved)?;
  let ipa = match &options.artifact {
    Some(path) => path.clone(),
    None => config::find_artifact(&options.tauri_dir.join("gen/apple/build"), "ipa", "")
      .ok_or_else(|| {
        crate::Error::GenericError(
          "no .ipa found under src-tauri/gen/apple/build — run `tauri ios build --export-method app-store-connect` first, or pass --artifact"
            .to_string(),
        )
      })?,
  };

  if !cfg!(target_os = "macos") {
    return super::err(
      "iOS submission requires macOS (`xcrun altool`). From Linux/Windows, build and submit \
       through a macOS CI runner instead — the API-based steps (`tauri deploy testflight …`) \
       work on any OS.",
    );
  }

  upload_ipa_altool(&ipa)?;

  println!(
    "Uploaded {} to App Store Connect. Processing typically takes 5–30 minutes \
     before the build can be assigned in TestFlight.",
    ipa.display()
  );

  if options.link {
    super::asc::link_command(super::asc::LinkOptions {
      profile: options.profile.clone(),
      group: None,
      limit: None,
      build_id: None,
      submit_review: false,
      tauri_dir: options.tauri_dir.clone(),
    })?;
  }
  Ok(())
}

/// Upload an IPA with `xcrun altool`, Tauri's documented upload path.
/// altool locates the API key in `~/private_keys/AuthKey_<key id>.p8`;
/// when the key is provided base64-encoded or by explicit path, stage it
/// there first.
#[cfg(target_os = "macos")]
fn upload_ipa_altool(ipa: &std::path::Path) -> Result<()> {
  use base64::Engine;

  let key_id = std::env::var("APPLE_API_KEY")
    .or_else(|_| std::env::var("APP_STORE_CONNECT_KEY_ID"))
    .unwrap_or_default();
  let issuer = std::env::var("APPLE_API_ISSUER")
    .or_else(|_| std::env::var("APP_STORE_CONNECT_ISSUER_ID"))
    .unwrap_or_default();
  if key_id.is_empty() || issuer.is_empty() {
    return super::err(
      "set APPLE_API_KEY and APPLE_API_ISSUER (App Store Connect API key) to upload",
    );
  }

  if let Some(home) = dirs::home_dir() {
    let keys_dir = home.join("private_keys");
    let staged = keys_dir.join(format!("AuthKey_{key_id}.p8"));
    if !staged.exists() {
      let contents = if let Ok(path) = std::env::var("APPLE_API_KEY_PATH") {
        std::fs::read(&path).ok()
      } else if let Ok(b64) = std::env::var("APP_STORE_CONNECT_API_KEY_BASE64") {
        base64::engine::general_purpose::STANDARD
          .decode(b64.trim())
          .ok()
      } else {
        None
      };
      if let Some(contents) = contents {
        let _ = std::fs::create_dir_all(&keys_dir);
        let _ = std::fs::write(&staged, contents);
      }
    }
  }

  let status = std::process::Command::new("xcrun")
    .args(["altool", "--upload-app", "--type", "ios", "--file"])
    .arg(ipa)
    .args(["--apiKey", &key_id, "--apiIssuer", &issuer])
    .status()
    .map_err(|e| crate::Error::GenericError(format!("failed to run xcrun altool: {e}")))?;
  if !status.success() {
    return super::err(format!("xcrun altool exited with {status}"));
  }
  Ok(())
}

#[cfg(not(target_os = "macos"))]
fn upload_ipa_altool(_ipa: &std::path::Path) -> Result<()> {
  super::err("iOS submission requires macOS")
}

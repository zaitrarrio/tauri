// Copyright 2019-2024 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

//! `tauri deploy share` — get a shareable install link for a build.
//!
//! Android: uploads the artifact to Play internal app sharing, which
//! returns a real download URL (the closest Play equivalent of a
//! TestFlight public link).
//! iOS: there is no anonymous IPA install — sharing means the TestFlight
//! public link, so this delegates to `testflight link`.

use super::{Platform, asc, config, play};
use crate::Result;
use clap::Parser;
use std::path::PathBuf;

#[derive(Debug, Parser)]
pub struct Options {
  /// Platform to share
  #[clap(value_enum, long)]
  pub platform: Platform,
  /// Artifact for Android internal app sharing (.apk or .aab)
  #[clap(long)]
  pub artifact: Option<PathBuf>,
  /// Deploy profile (iOS: TestFlight group defaults; default: preview)
  #[clap(long, default_value = "preview")]
  pub profile: String,
  /// Path to the Tauri directory (where tauri.conf.json lives)
  #[clap(long, default_value = "src-tauri")]
  pub tauri_dir: PathBuf,
}

pub fn command(options: Options) -> Result<()> {
  let resolved = config::load(&options.tauri_dir)?;
  match options.platform {
    Platform::Android => {
      let package = config::require_android_package(&resolved)?;
      let artifact = match &options.artifact {
        Some(path) => path.clone(),
        None => config::find_artifact(&options.tauri_dir.join("gen/android"), "aab", "release")
          .or_else(|| config::find_artifact(&options.tauri_dir.join("gen/android"), "apk", "release"))
          .ok_or_else(|| {
            crate::Error::GenericError(
              "no .aab/.apk found under src-tauri/gen/android — run `tauri android build` first, or pass --artifact"
                .to_string(),
            )
          })?,
      };
      let url = play::upload_internal_app_sharing(&package, &artifact)?;
      println!("Internal app sharing link (installs directly, no track needed): {url}");
      Ok(())
    }
    Platform::Ios => asc::link_command(asc::LinkOptions {
      profile: options.profile.clone(),
      group: None,
      limit: None,
      build_id: None,
      latest: true,
      submit_review: false,
      tauri_dir: options.tauri_dir.clone(),
    }),
  }
}

// Copyright 2019-2024 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

//! `tauri deploy release` — production release operations:
//! submit an App Store version for review (iOS), promote a Play release
//! between tracks, and halt/resume a Play rollout.

use super::{asc, config, play};
use crate::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[clap(subcommand_required(true), arg_required_else_help(true))]
pub struct Cli {
  #[clap(subcommand)]
  command: Commands,
}

#[derive(Subcommand)]
enum Commands {
  /// Submit the newest editable App Store version for App Store review
  Ios(IosOptions),
  /// Promote the active release from one Play track to another
  Android(AndroidOptions),
  /// Halt the active Play rollout on a track
  Halt(TrackOptions),
  /// Resume the active Play rollout on a track, optionally at a new fraction
  Resume(TrackOptions),
}

#[derive(Debug, Parser)]
pub struct IosOptions {
  /// Path to the Tauri directory (where tauri.conf.json lives)
  #[clap(long, default_value = "src-tauri")]
  pub tauri_dir: PathBuf,
}

#[derive(Debug, Parser)]
pub struct AndroidOptions {
  /// Track to promote from
  #[clap(long, default_value = "internal")]
  pub from: String,
  /// Track to promote to
  #[clap(long, default_value = "production")]
  pub to: String,
  /// Path to the Tauri directory (where tauri.conf.json lives)
  #[clap(long, default_value = "src-tauri")]
  pub tauri_dir: PathBuf,
}

#[derive(Debug, Parser)]
pub struct TrackOptions {
  /// Play track whose active release to halt/resume
  #[clap(long, default_value = "production")]
  pub track: String,
  /// Rollout fraction when resuming as a staged rollout (0.0–1.0);
  /// omit to resume at full rollout
  #[clap(long)]
  pub user_fraction: Option<f64>,
  /// Path to the Tauri directory (where tauri.conf.json lives)
  #[clap(long, default_value = "src-tauri")]
  pub tauri_dir: PathBuf,
}

pub fn command(cli: Cli) -> Result<()> {
  match cli.command {
    Commands::Ios(options) => {
      let resolved = config::load(&options.tauri_dir)?;
      let bundle_id = config::require_bundle_id(&resolved)?;
      let client = asc::Client::new()?;
      let app = client.find_app(&bundle_id)?;
      let app_id = app["id"].as_str().unwrap_or_default();
      let version = client
        .find_editable_appstore_version(app_id)?
        .ok_or_else(|| {
          crate::Error::GenericError(
            "no App Store version in a submittable state (Prepare for Submission / rejected states). \
             Create the version and its listing in App Store Connect first."
              .to_string(),
          )
        })?;
      let version_id = version["id"].as_str().unwrap_or_default();
      let version_string = version["attributes"]["versionString"]
        .as_str()
        .unwrap_or_default()
        .to_string();
      client.submit_appstore_version(version_id)?;
      println!("App Store version {version_string} submitted for review.");
      Ok(())
    }
    Commands::Android(options) => {
      let resolved = config::load(&options.tauri_dir)?;
      let package = config::require_android_package(&resolved)?;
      play::promote_release(&package, &options.from, &options.to)
    }
    Commands::Halt(options) => {
      let resolved = config::load(&options.tauri_dir)?;
      let package = config::require_android_package(&resolved)?;
      play::set_release_status(&package, &options.track, "halted", None)
    }
    Commands::Resume(options) => {
      let resolved = config::load(&options.tauri_dir)?;
      let package = config::require_android_package(&resolved)?;
      let status = if options.user_fraction.is_some() {
        "inProgress"
      } else {
        "completed"
      };
      play::set_release_status(&package, &options.track, status, options.user_fraction)
    }
  }
}

// Copyright 2019-2024 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

//! `tauri deploy version` — show or set the app version in
//! `tauri.conf.json` (the single source both mobile builds derive their
//! version from), and optionally the iOS bundle build number
//! (`bundle.iOS.bundleVersion`). EAS exposes the same as remote/local
//! version management; here it is a small, explicit local edit.

use crate::Result;
use clap::Parser;
use std::path::PathBuf;

#[derive(Debug, Parser)]
pub struct Options {
  /// Set an exact version (semver, e.g. 1.2.3)
  #[clap(long, conflicts_with = "bump")]
  pub set: Option<String>,
  /// Bump one component of the current version
  #[clap(long, value_enum)]
  pub bump: Option<Bump>,
  /// Set the iOS build number (bundle.iOS.bundleVersion)
  #[clap(long)]
  pub build_number: Option<String>,
  /// Path to the Tauri directory (where tauri.conf.json lives)
  #[clap(long, default_value = "src-tauri")]
  pub tauri_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum Bump {
  Patch,
  Minor,
  Major,
}

pub fn command(options: Options) -> Result<()> {
  let path = options.tauri_dir.join("tauri.conf.json");
  let raw = std::fs::read_to_string(&path)
    .map_err(|e| crate::Error::GenericError(format!("failed to read {}: {e}", path.display())))?;
  let mut root: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
    crate::Error::GenericError(format!(
      "{} is not plain JSON ({e}); version editing supports tauri.conf.json only",
      path.display()
    ))
  })?;

  let current = root["version"].as_str().map(|s| s.to_string());
  let Some(current) = current else {
    println!(
      "version: (not a plain string in tauri.conf.json — workspace/object form is not edited here)"
    );
    if options.set.is_some() || options.bump.is_some() || options.build_number.is_some() {
      return super::err("cannot edit this version form automatically");
    }
    return Ok(());
  };

  let new_version = match (&options.set, &options.bump) {
    (Some(v), _) => Some(v.clone()),
    (_, Some(bump)) => {
      let mut parts: Vec<u64> = current.split('.').map(|p| p.parse().unwrap_or(0)).collect();
      while parts.len() < 3 {
        parts.push(0);
      }
      match bump {
        Bump::Major => {
          parts[0] += 1;
          parts[1] = 0;
          parts[2] = 0;
        }
        Bump::Minor => {
          parts[1] += 1;
          parts[2] = 0;
        }
        Bump::Patch => parts[2] += 1,
      }
      Some(format!("{}.{}.{}", parts[0], parts[1], parts[2]))
    }
    _ => None,
  };

  let mut changed = false;
  if let Some(v) = &new_version {
    root["version"] = serde_json::json!(v);
    changed = true;
  }
  if let Some(build) = &options.build_number {
    let bundle = root
      .as_object_mut()
      .ok_or_else(|| {
        crate::Error::GenericError("tauri.conf.json root is not an object".to_string())
      })?
      .entry("bundle")
      .or_insert_with(|| serde_json::json!({}));
    let ios = bundle
      .as_object_mut()
      .ok_or_else(|| crate::Error::GenericError("`bundle` is not an object".to_string()))?
      .entry("iOS")
      .or_insert_with(|| serde_json::json!({}));
    ios
      .as_object_mut()
      .ok_or_else(|| crate::Error::GenericError("`bundle.iOS` is not an object".to_string()))?
      .insert("bundleVersion".to_string(), serde_json::json!(build));
    changed = true;
  }

  if changed {
    let out =
      serde_json::to_string_pretty(&root).map_err(|e| crate::Error::GenericError(e.to_string()))?;
    std::fs::write(&path, format!("{out}\n")).map_err(|e| {
      crate::Error::GenericError(format!("failed to write {}: {e}", path.display()))
    })?;
    println!(
      "version: {current} → {}",
      new_version.as_deref().unwrap_or(&current)
    );
    if let Some(build) = &options.build_number {
      println!("iOS bundleVersion (build number): {build}");
    }
    println!(
      "Note: Android versionCode is derived from the version by the bundler; \
       already-generated native projects may need `tauri android init` re-sync on some setups."
    );
  } else {
    println!("version: {current}");
    if let Some(build) = root["bundle"]["iOS"]["bundleVersion"].as_str() {
      println!("iOS bundleVersion (build number): {build}");
    }
  }
  Ok(())
}

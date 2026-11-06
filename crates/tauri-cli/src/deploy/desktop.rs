// Copyright 2019-2024 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

//! `tauri deploy desktop` — publish desktop updater artifacts: scan a
//! directory of signed updater bundles (produced by
//! `tauri build` with `createUpdaterArtifacts`), build the updater
//! manifest (`latest.json`) the Tauri updater plugin consumes, and
//! optionally upload artifacts + manifest to a GitHub Release.
//!
//! Artifacts are expected to already carry their `.sig` signatures
//! (from `tauri signer` / the build's signing step); this command
//! refuses unsigned artifacts rather than publishing something the
//! updater would reject anyway.

use super::http_agent;
use crate::Result;
use clap::Parser;
use std::path::PathBuf;

#[derive(Debug, Parser)]
pub struct Options {
  /// Directory containing the updater artifacts and their .sig files
  #[clap(long)]
  pub artifacts_dir: PathBuf,
  /// Release version (default: from tauri.conf.json)
  #[clap(long)]
  pub release_version: Option<String>,
  /// Release notes
  #[clap(long, default_value = "")]
  pub notes: String,
  /// GitHub repo (owner/name) the artifacts are (or will be) hosted on
  #[clap(long)]
  pub repo: String,
  /// Also create the GitHub release (tag v<version>) and upload the
  /// artifacts + latest.json to it. Needs GH_TOKEN in the environment.
  #[clap(long)]
  pub upload: bool,
  /// Path to the Tauri directory (where tauri.conf.json lives)
  #[clap(long, default_value = "src-tauri")]
  pub tauri_dir: PathBuf,
}

struct Artifact {
  platform_key: String,
  file_name: String,
  path: PathBuf,
  signature: String,
}

fn platform_key_for(file_name: &str) -> Option<&'static str> {
  let lower = file_name.to_lowercase();
  let arch = if lower.contains("aarch64") || lower.contains("arm64") {
    "aarch64"
  } else if lower.contains("x86_64") || lower.contains("x64") {
    "x86_64"
  } else {
    "x86_64"
  };
  if lower.ends_with(".app.tar.gz") {
    Some(if arch == "aarch64" {
      "darwin-aarch64"
    } else {
      "darwin-x86_64"
    })
  } else if lower.ends_with(".msi.zip") || lower.ends_with(".nsis.zip") {
    Some(if arch == "aarch64" {
      "windows-aarch64"
    } else {
      "windows-x86_64"
    })
  } else if lower.ends_with(".appimage.tar.gz")
    || lower.ends_with(".deb.tar.gz")
    || lower.ends_with(".rpm.tar.gz")
  {
    Some(if arch == "aarch64" {
      "linux-aarch64"
    } else {
      "linux-x86_64"
    })
  } else {
    None
  }
}

fn collect(dir: &std::path::Path) -> Result<Vec<Artifact>> {
  let mut artifacts = Vec::new();
  let entries = std::fs::read_dir(dir)
    .map_err(|e| crate::Error::GenericError(format!("failed to read {}: {e}", dir.display())))?;
  for entry in entries.flatten() {
    let path = entry.path();
    let Some(name) = path
      .file_name()
      .and_then(|n| n.to_str().map(|s| s.to_string()))
    else {
      continue;
    };
    let Some(platform_key) = platform_key_for(&name) else {
      continue;
    };
    // The .sig sits next to the full file name (e.g. `app.tar.gz.sig`).
    let sig_path = {
      let mut p = path.clone().into_os_string();
      p.push(".sig");
      PathBuf::from(p)
    };
    let signature = std::fs::read_to_string(&sig_path).map_err(|_| {
      crate::Error::GenericError(format!(
        "missing signature for {name} (expected {}) — build with updater signing enabled",
        sig_path.display()
      ))
    })?;
    artifacts.push(Artifact {
      platform_key: platform_key.to_string(),
      file_name: name,
      path,
      signature: signature.trim().to_string(),
    });
  }
  if artifacts.is_empty() {
    return super::err(format!(
      "no updater artifacts found in {} (looked for .app.tar.gz / .msi.zip / .nsis.zip / .AppImage.tar.gz with .sig files)",
      dir.display()
    ));
  }
  Ok(artifacts)
}

fn rfc3339_now() -> String {
  // Seconds since epoch → RFC3339 via a tiny civil-date conversion, to
  // avoid a date dependency.
  let secs = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|d| d.as_secs())
    .unwrap_or(0);
  let days = (secs / 86400) as i64;
  let rem = secs % 86400;
  let (h, m, sec) = (rem / 3600, (rem % 3600) / 60, rem % 60);
  let z = days + 719468;
  let era = if z >= 0 { z } else { z - 146096 } / 146097;
  let doe = (z - era * 146097) as u64;
  let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
  let y = yoe as i64 + era * 400;
  let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
  let mp = (5 * doy + 2) / 153;
  let d = doy - (153 * mp + 2) / 5 + 1;
  let month = if mp < 10 { mp + 3 } else { mp - 9 };
  let year = if month <= 2 { y + 1 } else { y };
  format!("{year:04}-{month:02}-{d:02}T{h:02}:{m:02}:{sec:02}Z")
}

pub fn command(options: Options) -> Result<()> {
  let version = match &options.release_version {
    Some(v) => v.clone(),
    None => {
      let resolved = super::config::load(&options.tauri_dir)?;
      let _ = resolved;
      let conf = std::fs::read_to_string(options.tauri_dir.join("tauri.conf.json"))
        .map_err(|e| crate::Error::GenericError(format!("failed to read tauri.conf.json: {e}")))?;
      let value: serde_json::Value = serde_json::from_str(&conf).map_err(|e| {
        crate::Error::GenericError(format!("tauri.conf.json is not plain JSON: {e}"))
      })?;
      value["version"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| {
          crate::Error::GenericError(
            "no string `version` in tauri.conf.json; pass --version".to_string(),
          )
        })?
    }
  };

  let artifacts = collect(&options.artifacts_dir)?;
  let mut platforms = serde_json::Map::new();
  for artifact in &artifacts {
    platforms.insert(
      artifact.platform_key.clone(),
      serde_json::json!({
        "signature": artifact.signature,
        "url": format!(
          "https://github.com/{}/releases/download/v{}/{}",
          options.repo, version, artifact.file_name
        )
      }),
    );
  }
  let manifest = serde_json::json!({
    "version": version,
    "notes": options.notes,
    "pub_date": rfc3339_now(),
    "platforms": platforms
  });
  let manifest_text = serde_json::to_string_pretty(&manifest)
    .map_err(|e| crate::Error::GenericError(e.to_string()))?;
  let manifest_path = options.artifacts_dir.join("latest.json");
  std::fs::write(&manifest_path, format!("{manifest_text}\n")).map_err(|e| {
    crate::Error::GenericError(format!("failed to write {}: {e}", manifest_path.display()))
  })?;
  println!("wrote updater manifest: {}", manifest_path.display());
  for artifact in &artifacts {
    println!("  {} → {}", artifact.platform_key, artifact.file_name);
  }

  if options.upload {
    upload_to_github(&options, &version, &artifacts, &manifest_path)?;
  }
  Ok(())
}

fn upload_to_github(
  options: &Options,
  version: &str,
  artifacts: &[Artifact],
  manifest_path: &std::path::Path,
) -> Result<()> {
  let token = std::env::var("GH_TOKEN").map_err(|_| {
    crate::Error::GenericError("GH_TOKEN is not set — needed for --upload".to_string())
  })?;
  let agent = http_agent();
  let tag = format!("v{version}");

  // Create the release if the tag has none yet.
  let create_body = serde_json::json!({
    "tag_name": tag,
    "name": tag,
    "body": options.notes,
    "draft": false,
    "prerelease": false
  });
  let (status, json) = super::http_json(
    &agent,
    "POST",
    &format!("https://api.github.com/repos/{}/releases", options.repo),
    &token,
    Some(&create_body),
  )?;
  let release = if (200..300).contains(&status) {
    json
  } else {
    // Likely "release already exists" — look it up by tag.
    let (status, json) = super::http_json(
      &agent,
      "GET",
      &format!(
        "https://api.github.com/repos/{}/releases/tags/{}",
        options.repo, tag
      ),
      &token,
      None,
    )?;
    super::expect_ok("get release by tag", status, &json)?.clone()
  };
  let upload_url = release["upload_url"]
    .as_str()
    .ok_or_else(|| crate::Error::GenericError("release response had no upload_url".to_string()))?
    .split('{')
    .next()
    .unwrap_or_default()
    .to_string();

  let upload_file = |path: &std::path::Path, name: &str| -> Result<()> {
    let bytes = std::fs::read(path)
      .map_err(|e| crate::Error::GenericError(format!("failed to read {}: {e}", path.display())))?;
    let mut resp = agent
      .post(&format!("{upload_url}?name={name}"))
      .header("Authorization", format!("Bearer {token}"))
      .header("Content-Type", "application/octet-stream")
      .send(bytes)
      .map_err(|e| crate::Error::GenericError(format!("upload of {name} failed: {e}")))?;
    let status = resp.status().as_u16();
    let _ = resp.body_mut().read_to_string();
    if (200..300).contains(&status) {
      println!("uploaded {name}");
      Ok(())
    } else {
      super::err(format!("upload of {name} failed (HTTP {status})"))
    }
  };

  for artifact in artifacts {
    upload_file(&artifact.path, &artifact.file_name)?;
    let sig_name = format!("{}.sig", artifact.file_name);
    let sig_path = {
      let mut p = artifact.path.clone().into_os_string();
      p.push(".sig");
      PathBuf::from(p)
    };
    upload_file(&sig_path, &sig_name)?;
  }
  upload_file(manifest_path, "latest.json")?;
  println!(
    "release: https://github.com/{}/releases/tag/{}",
    options.repo, tag
  );
  Ok(())
}

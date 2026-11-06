// Copyright 2019-2024 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

//! `tauri deploy doctor` — preflight checks for the problems that
//! actually bounce store submissions: missing identifier, a store icon
//! that is not 1024×1024, no privacy manifest, the export-compliance key
//! unset, and missing store credentials. Read-only; `--strict` turns
//! warnings into a failing exit code.

use super::config;
use crate::Result;
use clap::Parser;
use std::path::{Path, PathBuf};

#[derive(Debug, Parser)]
pub struct Options {
  /// Path to the Tauri directory (where tauri.conf.json lives)
  #[clap(long, default_value = "src-tauri")]
  pub tauri_dir: PathBuf,
  /// Fail (exit non-zero) when any check warns, not only on failures
  #[clap(long)]
  pub strict: bool,
}

#[derive(PartialEq)]
enum Level {
  Pass,
  Warn,
  Fail,
}

struct Check {
  name: &'static str,
  level: Level,
  message: String,
}

fn png_dimensions(path: &Path) -> Option<(u32, u32)> {
  let bytes = std::fs::read(path).ok()?;
  if bytes.len() < 24 || &bytes[1..4] != b"PNG" {
    return None;
  }
  let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
  let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
  Some((w, h))
}

fn find_file(dir: &Path, file_name: &str) -> Option<PathBuf> {
  let mut stack = vec![dir.to_path_buf()];
  while let Some(current) = stack.pop() {
    let Ok(entries) = std::fs::read_dir(&current) else {
      continue;
    };
    for entry in entries.flatten() {
      let path = entry.path();
      if path.is_dir() {
        stack.push(path);
      } else if path.file_name().and_then(|n| n.to_str()) == Some(file_name) {
        return Some(path);
      }
    }
  }
  None
}

fn env_present(names: &[&str]) -> bool {
  names
    .iter()
    .any(|n| std::env::var(n).is_ok_and(|v| !v.is_empty()))
}

pub fn command(options: Options) -> Result<()> {
  let dir = &options.tauri_dir;
  let mut checks: Vec<Check> = Vec::new();
  let mut push = |name, level, message: String| {
    checks.push(Check {
      name,
      level,
      message,
    })
  };

  // Tauri config + identifier
  match config::load(dir) {
    Ok(resolved) => {
      if resolved.bundle_id.is_some() {
        push(
          "identifier",
          Level::Pass,
          format!(
            "bundle id / package: {}",
            resolved.bundle_id.clone().unwrap_or_default()
          ),
        );
      } else {
        push(
          "identifier",
          Level::Fail,
          "no identifier in tauri.conf.json and no app.bundleId in deploy.json".to_string(),
        );
      }
    }
    Err(e) => push("tauri config", Level::Fail, e.to_string()),
  }

  // deploy.json (optional)
  let deploy_path = dir.join("deploy.json");
  if deploy_path.exists() {
    match std::fs::read_to_string(&deploy_path)
      .map_err(|e| e.to_string())
      .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).map_err(|e| e.to_string()))
    {
      Ok(_) => push(
        "deploy.json",
        Level::Pass,
        "present and valid JSON".to_string(),
      ),
      Err(e) => push("deploy.json", Level::Fail, format!("invalid: {e}")),
    }
  } else {
    push(
      "deploy.json",
      Level::Warn,
      "not found — deploy commands will use defaults; `tauri deploy init` creates one".to_string(),
    );
  }

  // Store icon: largest icon must be 1024×1024 for App Store / Play.
  let icon = dir.join("icons/icon.png");
  match png_dimensions(&icon) {
    Some((1024, 1024)) => push(
      "store icon",
      Level::Pass,
      "icons/icon.png is 1024×1024".to_string(),
    ),
    Some((w, h)) => push(
      "store icon",
      Level::Warn,
      format!(
        "icons/icon.png is {w}×{h}; stores expect a 1024×1024 source icon (`tauri icon` regenerates the set)"
      ),
    ),
    None => push(
      "store icon",
      Level::Warn,
      "icons/icon.png missing or unreadable".to_string(),
    ),
  }

  // Privacy manifest (required by Apple for apps using required-reason APIs)
  if find_file(dir, "PrivacyInfo.xcprivacy").is_some() {
    push(
      "privacy manifest",
      Level::Pass,
      "PrivacyInfo.xcprivacy found".to_string(),
    );
  } else {
    push("privacy manifest", Level::Warn, "no PrivacyInfo.xcprivacy under the Tauri dir — App Store submissions that use required-reason APIs are rejected without one".to_string());
  }

  // Export compliance in the generated iOS project, when it exists
  let apple_dir = dir.join("gen/apple");
  if apple_dir.exists() {
    let mut found = false;
    if let Some(plist) = find_file(&apple_dir, "Info.plist") {
      if let Ok(contents) = std::fs::read_to_string(&plist) {
        found = contents.contains("ITSAppUsesNonExemptEncryption");
      }
    }
    if found {
      push(
        "export compliance",
        Level::Pass,
        "ITSAppUsesNonExemptEncryption is set in the iOS Info.plist".to_string(),
      );
    } else {
      push("export compliance", Level::Warn, "ITSAppUsesNonExemptEncryption not found in the generated iOS Info.plist — App Store Connect will ask about encryption on every upload until it is".to_string());
    }
  } else {
    push(
      "export compliance",
      Level::Warn,
      "no generated iOS project yet (gen/apple) — run `tauri ios init` on macOS, then re-check"
        .to_string(),
    );
  }

  // Credentials (presence only)
  if env_present(&["APPLE_API_KEY", "APP_STORE_CONNECT_KEY_ID"])
    && env_present(&["APPLE_API_ISSUER", "APP_STORE_CONNECT_ISSUER_ID"])
  {
    push(
      "App Store Connect credentials",
      Level::Pass,
      "API key id + issuer present in the environment".to_string(),
    );
  } else {
    push(
      "App Store Connect credentials",
      Level::Warn,
      "APPLE_API_KEY / APPLE_API_ISSUER not set — TestFlight commands cannot authenticate"
        .to_string(),
    );
  }
  if env_present(&[
    "GOOGLE_PLAY_SERVICE_ACCOUNT",
    "GOOGLE_PLAY_SERVICE_ACCOUNT_JSON",
  ]) {
    push(
      "Google Play credentials",
      Level::Pass,
      "service account configured".to_string(),
    );
  } else {
    push(
      "Google Play credentials",
      Level::Warn,
      "GOOGLE_PLAY_SERVICE_ACCOUNT not set — Android submit cannot authenticate".to_string(),
    );
  }
  if env_present(&["ANDROID_KEYSTORE_PATH"])
    || dir.join("gen/android/keystore.properties").exists()
    || dir.join("gen/android/app/key.jks").exists()
  {
    push(
      "Android signing",
      Level::Pass,
      "release keystore configured".to_string(),
    );
  } else {
    push("Android signing", Level::Warn, "no release keystore found (ANDROID_KEYSTORE_PATH / gen/android) — production AABs cannot be signed".to_string());
  }

  let (mut fails, mut warns) = (0, 0);
  for check in &checks {
    let (mark, label) = match check.level {
      Level::Pass => ("✓", "pass"),
      Level::Warn => {
        warns += 1;
        ("!", "warn")
      }
      Level::Fail => {
        fails += 1;
        ("✗", "FAIL")
      }
    };
    println!("{mark} [{label}] {} — {}", check.name, check.message);
  }
  println!(
    "\n{} passed, {} warnings, {} failures",
    checks.len() - fails - warns,
    warns,
    fails
  );
  if fails > 0 || (options.strict && warns > 0) {
    return super::err("deploy doctor found blocking problems (see above)");
  }
  Ok(())
}

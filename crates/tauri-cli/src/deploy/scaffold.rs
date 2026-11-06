// Copyright 2019-2024 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

//! `tauri deploy init` — scaffold the deploy layer into a project:
//! `deploy.json` (profiles), `metadata.json` (store text),
//! `.env.deploy.example` (credential names, no values), and a GitHub
//! Actions workflow that builds and submits on demand. Existing files
//! are never overwritten.

use crate::Result;
use clap::Parser;
use std::path::PathBuf;

#[derive(Debug, Parser)]
pub struct Options {
  /// Path to the Tauri directory (where tauri.conf.json lives)
  #[clap(long, default_value = "src-tauri")]
  pub tauri_dir: PathBuf,
}

const DEPLOY_JSON: &str = r#"{
  "app": { "bundleId": null, "androidPackage": null },
  "submit": {
    "preview": {
      "ios": {
        "group": "Preview",
        "publicLinkLimit": 200,
        "whatsNew": "Thanks for testing! Please send feedback from inside TestFlight.",
        "review": {
          "contactFirstName": "",
          "contactLastName": "",
          "contactEmail": "",
          "contactPhone": "",
          "demoAccountRequired": false,
          "notes": ""
        }
      },
      "android": { "track": "internal" }
    },
    "production": {
      "ios": { "group": "Public Beta", "publicLinkLimit": 10000 },
      "android": { "track": "production" }
    }
  }
}
"#;

const METADATA_JSON: &str = r#"{
  "ios": {
    "locale": "en-US",
    "description": "",
    "feedbackEmail": "",
    "marketingUrl": "",
    "privacyPolicyUrl": ""
  },
  "android": {
    "listings": {
      "en-US": { "title": "", "shortDescription": "", "fullDescription": "" }
    }
  }
}
"#;

const ENV_EXAMPLE: &str = r#"# Deploy credentials — copy to .env.deploy (never commit the filled file).
APPLE_DEVELOPMENT_TEAM=
APPLE_API_KEY=
APPLE_API_ISSUER=
APPLE_API_KEY_PATH=
APP_STORE_CONNECT_API_KEY_BASE64=
GOOGLE_PLAY_SERVICE_ACCOUNT=
ANDROID_KEYSTORE_PATH=
ANDROID_KEYSTORE_PASSWORD=
ANDROID_KEY_ALIAS=
ANDROID_KEY_PASSWORD=
"#;

const WORKFLOW: &str = r#"name: Deploy (tauri deploy)

# Manual: Actions -> "Deploy (tauri deploy)" -> Run workflow.
# Assumes an npm-based Tauri project (@tauri-apps/cli in devDependencies).
# Secrets to set (Settings -> Secrets and variables -> Actions):
#   APPLE_DEVELOPMENT_TEAM, APP_STORE_CONNECT_KEY_ID,
#   APP_STORE_CONNECT_ISSUER_ID, APP_STORE_CONNECT_API_KEY_BASE64,
#   GOOGLE_PLAY_SERVICE_ACCOUNT_JSON (contents of the service-account JSON)

on:
  workflow_dispatch:
    inputs:
      platform:
        type: choice
        options: [all, ios, android]
        default: all
      profile:
        type: choice
        options: [preview, production]
        default: preview

jobs:
  ios:
    if: ${{ inputs.platform == 'all' || inputs.platform == 'ios' }}
    runs-on: macos-14
    env:
      APPLE_DEVELOPMENT_TEAM: ${{ secrets.APPLE_DEVELOPMENT_TEAM }}
      APPLE_API_KEY: ${{ secrets.APP_STORE_CONNECT_KEY_ID }}
      APPLE_API_ISSUER: ${{ secrets.APP_STORE_CONNECT_ISSUER_ID }}
      APP_STORE_CONNECT_API_KEY_BASE64: ${{ secrets.APP_STORE_CONNECT_API_KEY_BASE64 }}
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-node@v4
        with: { node-version: 20, cache: npm }
      - uses: dtolnay/rust-toolchain@stable
        with: { targets: aarch64-apple-ios }
      - run: npm ci
      - run: npx tauri ios build --export-method app-store-connect
      - name: Submit + wait + public link (one-shot pipeline)
        run: npx tauri deploy submit --platform ios --profile ${{ inputs.profile }} --wait

  android:
    if: ${{ inputs.platform == 'all' || inputs.platform == 'android' }}
    runs-on: ubuntu-latest
    env:
      GOOGLE_PLAY_SERVICE_ACCOUNT_JSON: ${{ secrets.GOOGLE_PLAY_SERVICE_ACCOUNT_JSON }}
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-node@v4
        with: { node-version: 20, cache: npm }
      - uses: actions/setup-java@v4
        with: { distribution: temurin, java-version: 17 }
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: aarch64-linux-android,armv7-linux-androideabi,i686-linux-android,x86_64-linux-android
      - run: npm ci
      - run: npx tauri android build
      - run: npx tauri deploy submit --platform android --profile ${{ inputs.profile }}
"#;

pub fn command(options: Options) -> Result<()> {
  let dir = &options.tauri_dir;
  let project_root = dir
    .parent()
    .map(|p| p.to_path_buf())
    .unwrap_or_else(|| PathBuf::from("."));
  let targets: Vec<(PathBuf, &str)> = vec![
    (dir.join("deploy.json"), DEPLOY_JSON),
    (dir.join("metadata.json"), METADATA_JSON),
    (project_root.join(".env.deploy.example"), ENV_EXAMPLE),
    (
      project_root.join(".github/workflows/tauri-deploy.yml"),
      WORKFLOW,
    ),
  ];

  for (path, contents) in targets {
    if path.exists() {
      println!("skip (exists): {}", path.display());
      continue;
    }
    if let Some(parent) = path.parent() {
      std::fs::create_dir_all(parent).map_err(|e| {
        crate::Error::GenericError(format!("failed to create {}: {e}", parent.display()))
      })?;
    }
    std::fs::write(&path, contents).map_err(|e| {
      crate::Error::GenericError(format!("failed to write {}: {e}", path.display()))
    })?;
    println!("created: {}", path.display());
  }
  println!(
    "\nNext: fill the review contact details in deploy.json, copy .env.deploy.example to \
     .env.deploy with your store credentials, and run `tauri deploy doctor`."
  );
  Ok(())
}

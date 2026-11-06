// Copyright 2019-2024 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

//! Store submission and beta distribution for Tauri apps — the delivery
//! layer the CLI previously left to third-party tooling (fastlane, EAS in
//! the Expo world):
//!
//! - `tauri deploy submit --platform ios` uploads a built `.ipa` to App
//!   Store Connect / TestFlight (via `xcrun altool`, macOS only).
//! - `tauri deploy submit --platform android` uploads a built `.aab` to a
//!   Google Play track through the Play Developer API (any OS).
//! - `tauri deploy testflight link` creates (or reuses) an external
//!   TestFlight beta group, enables its **public link** and prints it —
//!   anyone with the link can install the beta, no tester list needed.
//! - `tauri deploy testflight status` lists recent builds and beta
//!   groups, including which public links are live.
//!
//! Configuration comes from an optional `deploy.json` next to
//! `tauri.conf.json` (profiles in the style of Expo's `eas.json`:
//! `development` / `preview` / `production`, each with a submit target),
//! falling back to the app `identifier` from the Tauri config. Every value
//! can be overridden by a CLI flag.
//!
//! Credentials are read from the environment only, never from config
//! files, and are never logged:
//!
//! - iOS: `APPLE_API_KEY` (or `APP_STORE_CONNECT_KEY_ID`),
//!   `APPLE_API_ISSUER` (or `APP_STORE_CONNECT_ISSUER_ID`), and the `.p8`
//!   key at `APPLE_API_KEY_PATH`, `~/private_keys/AuthKey_<key id>.p8`,
//!   or base64 in `APP_STORE_CONNECT_API_KEY_BASE64`.
//! - Android: `GOOGLE_PLAY_SERVICE_ACCOUNT` (path to the service-account
//!   JSON) or `GOOGLE_PLAY_SERVICE_ACCOUNT_JSON` (its contents).
//!
//! As a convenience, a `.env.deploy` file in the working directory is
//! loaded for these variables when they are not already set.

use crate::{Error, Result};
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

mod asc;
mod config;
mod desktop;
mod doctor;
mod jwt;
mod metadata;
mod play;
mod release;
mod scaffold;
mod share;
mod submit;
mod version;

#[derive(Parser)]
#[clap(
  author,
  version,
  about = "Submit builds to the stores and manage TestFlight beta distribution",
  subcommand_required(true),
  arg_required_else_help(true)
)]
pub struct Cli {
  #[clap(subcommand)]
  command: Commands,
}

#[derive(Subcommand)]
enum Commands {
  /// Upload a built app bundle to App Store Connect / TestFlight (iOS)
  /// or to a Google Play track (Android)
  Submit(submit::Options),
  /// Manage TestFlight external beta groups and their public links
  Testflight(TestflightCli),
  /// Sync store listing text from metadata.json
  Metadata(metadata::Options),
  /// Production release operations: App Store review submission,
  /// Play track promotion, rollout halt/resume
  Release(release::Cli),
  /// Get a shareable install link (Android internal app sharing / iOS TestFlight)
  Share(share::Options),
  /// Publish desktop updater artifacts and their manifest
  Desktop(desktop::Options),
  /// Preflight checks for common store-submission blockers
  Doctor(doctor::Options),
  /// Show or set the app version / build number in tauri.conf.json
  Version(version::Options),
  /// Scaffold deploy.json, metadata.json, .env.deploy.example and a CI workflow
  Init(scaffold::Options),
}

#[derive(Parser)]
#[clap(subcommand_required(true), arg_required_else_help(true))]
pub struct TestflightCli {
  #[clap(subcommand)]
  command: TestflightCommands,
}

#[derive(Subcommand)]
enum TestflightCommands {
  /// Ensure an external beta group exists, enable its public link and print it
  Link(asc::LinkOptions),
  /// List recent App Store Connect builds and beta groups (with public links)
  Status(asc::StatusOptions),
  /// Apply Beta App Review details + "What to Test" and submit a build for beta review
  Review(asc::ReviewOptions),
  /// List or invite beta testers
  Testers(TestersCli),
  /// List recent beta feedback submissions (crashes / tester reports)
  Feedback(asc::FeedbackOptions),
}

#[derive(Parser)]
#[clap(subcommand_required(true), arg_required_else_help(true))]
pub struct TestersCli {
  #[clap(subcommand)]
  command: TestersCommands,
}

#[derive(Subcommand)]
enum TestersCommands {
  /// List the app's beta testers
  List(asc::TestersListOptions),
  /// Invite a beta tester by email
  Add(asc::TesterAddOptions),
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Platform {
  Ios,
  Android,
}

pub fn command(cli: Cli) -> Result<()> {
  load_env_deploy();
  match cli.command {
    Commands::Submit(options) => submit::command(options),
    Commands::Testflight(cli) => match cli.command {
      TestflightCommands::Link(options) => asc::link_command(options),
      TestflightCommands::Status(options) => asc::status_command(options),
      TestflightCommands::Review(options) => asc::review_command(options),
      TestflightCommands::Testers(cli) => match cli.command {
        TestersCommands::List(options) => asc::testers_list_command(options),
        TestersCommands::Add(options) => asc::tester_add_command(options),
      },
      TestflightCommands::Feedback(options) => asc::feedback_command(options),
    },
    Commands::Metadata(options) => metadata::command(options),
    Commands::Release(cli) => release::command(cli),
    Commands::Share(options) => share::command(options),
    Commands::Desktop(options) => desktop::command(options),
    Commands::Doctor(options) => doctor::command(options),
    Commands::Version(options) => version::command(options),
    Commands::Init(options) => scaffold::command(options),
  }
}

pub(crate) fn err<T>(msg: impl std::fmt::Display) -> Result<T> {
  Err(Error::GenericError(msg.to_string()))
}

/// Load KEY=VALUE pairs from `./.env.deploy` for variables that are not
/// already present in the environment. Values are never printed.
fn load_env_deploy() {
  let path = PathBuf::from(".env.deploy");
  let Ok(contents) = std::fs::read_to_string(&path) else {
    return;
  };
  for line in contents.lines() {
    let Some((key, value)) = line.trim().split_once('=') else {
      continue;
    };
    let key = key.trim();
    if key.is_empty() || std::env::var_os(key).is_some() {
      continue;
    }
    let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
    // SAFETY: called once, before any worker threads are spawned by this
    // command, and only for keys that were unset.
    unsafe { std::env::set_var(key, value) };
  }
}

/// Build the shared HTTP agent (proxy from env, platform root certs),
/// mirroring `helpers::http`, with status codes surfaced to the caller
/// instead of being folded into transport errors.
pub(crate) fn http_agent() -> ureq::Agent {
  #[allow(unused_mut)]
  let mut config_builder = ureq::Agent::config_builder()
    .user_agent(concat!(
      env!("CARGO_PKG_NAME"),
      "/",
      env!("CARGO_PKG_VERSION")
    ))
    .proxy(ureq::Proxy::try_from_env())
    .http_status_as_error(false)
    .timeout_global(Some(std::time::Duration::from_secs(900)));

  #[cfg(feature = "platform-certs")]
  {
    config_builder = config_builder.tls_config(
      ureq::tls::TlsConfig::builder()
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build(),
    );
  }

  config_builder.build().into()
}

/// Minimal JSON-over-HTTP helper returning the status and parsed body.
pub(crate) fn http_json(
  agent: &ureq::Agent,
  method: &str,
  url: &str,
  token: &str,
  body: Option<&serde_json::Value>,
) -> Result<(u16, serde_json::Value)> {
  let auth = format!("Bearer {token}");
  // ureq's request builders are typestate (with/without body), so each
  // method arm drives its request to completion itself.
  let result = match method {
    "GET" => agent.get(url).header("Authorization", &auth).call(),
    "DELETE" => agent.delete(url).header("Authorization", &auth).call(),
    "POST" | "PUT" | "PATCH" => {
      let payload = body
        .map(|b| b.to_string())
        .unwrap_or_else(|| "{}".to_string());
      let send = |req: ureq::RequestBuilder<ureq::typestate::WithBody>| {
        req
          .header("Authorization", &auth)
          .header("Content-Type", "application/json")
          .send(payload.as_str())
      };
      match method {
        "POST" => send(agent.post(url)),
        "PUT" => send(agent.put(url)),
        _ => send(agent.patch(url)),
      }
    }
    other => return err(format!("unsupported HTTP method {other}")),
  };
  let mut resp = result.map_err(|e| Error::GenericError(format!("{method} {url} failed: {e}")))?;
  let status = resp.status().as_u16();
  let text = resp.body_mut().read_to_string().unwrap_or_default();
  let json = if text.is_empty() {
    serde_json::Value::Null
  } else {
    serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text))
  };
  Ok((status, json))
}

pub(crate) fn expect_ok<'a>(
  what: &str,
  status: u16,
  json: &'a serde_json::Value,
) -> Result<&'a serde_json::Value> {
  if (200..300).contains(&status) {
    Ok(json)
  } else {
    let detail = json
      .get("errors")
      .and_then(|e| e.as_array())
      .map(|errs| {
        errs
          .iter()
          .filter_map(|e| {
            let code = e.get("code")?.as_str()?;
            let detail = e
              .get("detail")
              .and_then(|d| d.as_str())
              .or_else(|| e.get("title").and_then(|t| t.as_str()))?;
            Some(format!("{code}: {detail}"))
          })
          .collect::<Vec<_>>()
          .join("; ")
      })
      .filter(|s| !s.is_empty())
      .unwrap_or_else(|| json.to_string());
    err(format!("{what} failed (HTTP {status}): {detail}"))
  }
}

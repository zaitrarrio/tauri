---
"tauri-cli": minor:feat
"@tauri-apps/cli": minor:feat
---

Extend the `tauri deploy` command group toward full store parity: Beta App Review details and "What to Test" from `deploy.json` (`deploy testflight review`, also applied by `testflight link --submit-review`), a one-shot iOS pipeline (`deploy submit --wait` polls processing, assigns, submits and prints the public link), Play staged rollouts with release notes plus halt/resume and track promotion (`deploy release halt|resume`, submit `--release-status/--user-fraction/--release-notes`), TestFlight tester management and beta feedback, store metadata sync from `metadata.json`, App Store review submission, Android internal app sharing links (`deploy share`), a `deploy doctor` preflight, `deploy version` management, `deploy init` scaffolding (including a CI workflow), and desktop updater publishing to GitHub Releases (`deploy desktop`).

---
"tauri-cli": minor:feat
"@tauri-apps/cli": minor:feat
---

Add a `deploy` command group to the CLI for store submission and beta distribution: `tauri deploy submit --platform ios` uploads an IPA to App Store Connect/TestFlight via `xcrun altool`, `tauri deploy submit --platform android` releases an AAB to a Google Play track through the Play Developer API, and `tauri deploy testflight link` / `status` manage external TestFlight beta groups and their public install links through the App Store Connect API. Configuration uses an optional EAS-style `deploy.json` (profiles such as `preview` and `production`) next to `tauri.conf.json`; credentials come from the environment only.

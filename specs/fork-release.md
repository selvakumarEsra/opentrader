---
id: FORK-RELEASE-DOC
title: App releases of selvakumarEsra/opentrader
owner: selvakumarEsra
priority: high
---

<!-- id: FORK-RELEASE-DOC -->
# App releases of selvakumarEsra/opentrader

The fork releases the OpenTrader app from its own repository, with its own
update key, and ships the moomoo plugin with each release.

<!-- id: REQ-FORK-001 -->
## Installed apps of the fork MUST update only from selvakumarEsra/opentrader

The app's updater reads `latest.json` of selvakumarEsra/opentrader's latest
release and checks updates (and plugin packages) against the fork's own
release key, whose private half is only in the repository's secrets and on the
maintainer's computer.

implementations:
  - src-tauri/tauri.conf.json

## Acceptance
<!-- id: REQ-FORK-001.A1 -->
- `plugins.updater.endpoints` is `https://github.com/selvakumarEsra/opentrader/releases/latest/download/latest.json` and `pubkey` is the fork's key, not upstream's.
<!-- id: REQ-FORK-001.A2 -->
- A plugin zip signed by the fork's release workflow verifies in the fork's app.

<!-- id: REQ-FORK-002 -->
## A version tag MUST release the app and the moomoo plugin to selvakumarEsra/opentrader

A pushed tag `v<version>` (tauri.conf.json's version) builds the installers for
Windows, macOS and Linux into a draft release of the fork, adds the moomoo
plugin's programs, zips (signed) and their checksums to it, and publishes it
once all of them are in. In the fork the market-data token is optional (a
warning: the app's own data feed is off, plugins still serve data), and the
upstream website is not deployed.

implementations:
  - .github/workflows/release.yml
  - .github/workflows/plugin-moomoo.yml

## Acceptance
<!-- id: REQ-FORK-002.A1 -->
- The release of `v<version>` holds the app installers, `latest.json`, and `opentrader-moomoo-<plugin version>-*`, `moomoo-<plugin version>*.zip` with `.sig` and `moomoo-SHA256SUMS`.
<!-- id: REQ-FORK-002.A2 -->
- The release is published only after the installers and the plugin are uploaded; a failed plugin build leaves it a draft.
<!-- id: REQ-FORK-002.A3 -->
- The web deploy job runs only in deepentropy/opentrader.
<!-- id: REQ-FORK-002.A4 -->
- A plugin-only release (`plugin-moomoo-v*`) is never marked Latest, so `releases/latest/download/latest.json` stays the app's.

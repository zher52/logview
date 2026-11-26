# LogView

A native desktop log viewer built with **Rust + GPUI** (self-drawn UI, no WebView), inspired by the
Chrome DevTools console. The interface supports **Chinese and English**; the primary documentation is
[README.md](README.md) (Chinese).

## Screenshots

![Main window](docs/screenshots/main.png)

![Auto-follow](docs/screenshots/follow.gif)

## Features

- **Four log sources** — local files (stat polling + seek), SSH/SFTP (password, key file, key text or
  ssh-agent), HTTP(S) with `Range` requests and built-in SSRF protection, and commands (local `spawn` or
  remote SSH `exec`, with stdout/stderr merged into the tab).
- **Level detection** — regex-based profiles (5 built-ins + unlimited custom profiles, import/export as JSON),
  per-level coloring and filter chips with live counts.
- **Search** — highlight or filter mode, plain text or regex, case sensitivity, match counter and jump.
- **Auto-follow** — per-tab worker polls for appended data, sticky-to-bottom with "N new lines" button,
  rotation/truncation detection.
- **Large files** — virtual scrolling renders only visible rows; initial head load, "load all" and
  "jump to end" (tail-aligned) actions.
- **Inline links** — URLs, POSIX/Windows paths and JSON lines open in new tabs; JSON lines get a pretty view.
- **Code & Markdown** — syntax highlighting for common code/config extensions, Markdown preview,
  optional editing with save.
- **Appearance** — follows system light/dark theme, custom accent color, configurable UI/log font size
  and family.
- **Credentials** — stored in the OS keychain (macOS Keychain, Windows Credential Manager,
  Linux Secret Service); settings files never contain secrets.
- **SSH host keys** — TOFU confirmation with the SHA256 fingerprint; unknown hosts can be trusted once
  or written to `known_hosts`, while changed keys are rejected.
- **Bilingual UI** — switch between Chinese and English in Settings; untranslated entries fall back to
  Chinese.

## Requirements

- Rust **1.92+** (`rustup`, stable).
- First build is slow (gpui / ssh2 / reqwest / keyring); incremental builds are fast.

## Build & run

```bash
cargo run                        # opens the "open source" dialog
cargo run -- sample/app.log      # open a file directly
cargo build --release            # binary at target/release/logview
cargo test --locked              # unit tests
```

macOS app bundle (document types, Finder "Open With"):

```bash
./scripts/make_app.sh
```

## Platform support

| Platform | Status |
|---|---|
| macOS | Primary development platform; `.app` bundling and file association work |
| Windows | Build check in CI; interaction and installer are not fully verified |
| Linux (X11 / Wayland) | Build check in CI; `.desktop` / AppImage packaging is still missing |

## Security

- HTTP sources only accept public `http/https` targets; DNS results are fully validated, the connection
  is pinned to the validated IP, and redirects are re-checked hop by hop.
- Credentials are kept in memory unless you explicitly choose to remember them (OS keychain).
- No telemetry.
- SSH host keys are verified against `~/.ssh/known_hosts`; mismatches are rejected, and unknown hosts
  require explicit TOFU confirmation before connecting (see [SECURITY.md](SECURITY.md)).

Please report vulnerabilities privately via GitHub Security Advisories, not public issues.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) and [CHANGELOG.md](CHANGELOG.md). Code comments and commit
messages are in Chinese; the UI is bilingual; identifiers are English.

## License

[GNU AGPL-3.0](LICENSE) (SPDX: `AGPL-3.0-only`), Copyright (C) 2026 zher52.

If you modify and distribute this program, or offer it to users over a network, you must make the
complete corresponding source available as required by section 13 of the AGPL.
Bundled Ionicons icons are MIT; dependency licenses are listed in
[THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md).
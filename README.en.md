# Log Viewer

A native desktop log viewer built with **Rust + GPUI** (self-drawn UI, no WebView), inspired by the
Chrome DevTools console. The interface supports **Chinese, English and Russian**; the primary documentation is
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
  "jump to end" (tail-aligned) actions. Command sources get the same history, pause and seek behaviour
  through a local spool.
- **Inline links** — URLs, POSIX/Windows paths and JSON lines open in new tabs; JSON lines get a pretty view.
- **Code & Markdown** — syntax highlighting for common code/config extensions, Markdown preview,
  optional editing with save.
- **Appearance** — follows system light/dark theme, custom accent color, configurable UI/log font size
  and family.
- **Credentials** — stored in the OS keychain (macOS Keychain, Windows Credential Manager,
  Linux Secret Service); settings files never contain secrets.
- **SSH host keys** — TOFU confirmation with the SHA256 fingerprint; unknown hosts can be trusted once
  or written to `known_hosts`, while changed keys are rejected.
- **Multilingual UI** — switch between Chinese, English and Russian in Settings; entries fall back to
  English, then Chinese. Translations live in `locales/{zh,en,ru}.toml` and can be edited without touching
  Rust; counts use per-language plural forms.

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

## Usage

### Opening a source (Ctrl/Cmd+T)

| Type | Fields | Notes |
|---|---|---|
| Local | File path | Or use **Browse…** to pick a file |
| SSH | Host / port / user / auth / remote path | Auth: password, key file (path on the server), key text (paste PEM), or ssh-agent |
| HTTP | URL / auth (none, Basic, Bearer, API key, custom header) | Public addresses only (see Security); following requires `Range` support |
| Command | Preset + parameters (local) or host + auth + preset + parameters (SSH) | Built-in adb / tail / journalctl / docker / kubectl presets; custom argv allowed |

With **Remember connection** enabled, credentials go to the **OS keychain** (macOS Keychain, Windows
Credential Manager, Linux Secret Service). `settings.json` only stores non-sensitive fields such as
host, port, user and path. If the keychain is unavailable, credentials live in memory for the current
session only. Plaintext credentials from older versions are migrated automatically at startup; on
failure the plaintext is still removed and you are asked to re-enter it. Pasted private-key text is
not written to the keychain (it can exceed the Windows 2560-byte entry limit); prefer a key file.

URL credentials have two modes: **Save credentials** (keychain) and **History only** (default,
sanitized URL). SSH host fields expand saved servers on focus; servers can be managed under
Settings → Connection, or imported from the system ssh config. The recents limit is configurable
(default 50). Deleting a URL entry also removes its keychain credentials by default.

### Command sources

Use **Command** on the welcome screen, or switch to the **Command** tab in the SSH dialog. Both share
the same presets and form.

- **Presets are data** in `command_presets` in `settings.json` (seeded on first run; new built-ins are
  appended by id). Edit argv or add your own commands without changing code. Empty parameters are
  dropped together with their adjacent flag.
- **Local commands** are executed directly by argv, without a shell; the executable is looked up on
  `PATH` with a clear error when missing.
- **SSH commands** run in the remote login shell (`sh -c`) with each argument single-quoted, so pipes
  and redirection work. This is equivalent to typing the command in a terminal; there is no argv-level
  sandbox or whitelist.
- **Semantics**: stderr is merged into the same tab. Output is also written to a local temporary
  spool (128 MB cap, mode 0600), so pausing or scrolling away never loses lines and **Load all** /
  **Jump to end** work exactly like file sources. Clearing the view also clears the spool; closing the
  tab or quitting deletes it, and leftovers from crashes are swept on the next start (macOS:
  `~/Library/Caches/logview/spool/`, Windows/Linux: `logview/spool/` under the system cache dir).
- **Exit & reconnect**: the exit code is shown at the bottom, and **Retry after exit** reopens with
  backoff (useful for device or container restarts); output from before the reconnect stays in the spool.
- **Auth**: SSH commands reuse the SSH file source credentials and keychain entry for the same
  `user@host:port`.

### Level profiles

```jsonc
// the configs array in settings (import/export uses the same format)
[
  {
    "id": "my-nginx",
    "name": "my-nginx",
    "enabled": true,
    "built_in": false,
    "rules": [
      { "auto": true,  "level": 2, "regex": "(?:^|\\s)severity=(\\w+)", "flags": "i" },
      { "auto": false, "level": 4, "regex": "\\b5\\d\\d\\b",        "flags": "" }
    ]
  }
]
```

- `auto: true` maps capture group 1 text to a level (`err/error→ERROR`, `warn→WARN`,
  `fatal/critical/panic→FATAL`, `trace→TRACE`, `debug→DEBUG`, `info→INFO`).
- `auto: false` uses the fixed `level` (0=TRACE … 5=FATAL).
- Enabled profiles are evaluated in order and **the first match wins**; the toolbar dropdown toggles
  them per tab.

### Search

| Control | Behavior |
|---|---|
| Highlight | All rows stay visible; matches get a highlight background and a match counter; `▲`/`▼`/Enter cycle |
| Filter | Only matching rows are shown (intersected with level filters); the box shows `visible/total` |
| `Aa` | Case sensitive |
| `.*` | Regex mode (invalid patterns show an inline error without breaking the view) |

### Shortcuts

Rebind actions under Settings → Shortcuts: click **Edit**, then press a combination with a modifier.
One action per combination; conflicts **swap** bindings. Each action can also be reset individually.

| Key | Action |
|---|---|
| Ctrl/Cmd+T | Open a source |
| Ctrl/Cmd+W | Close tab |
| Ctrl/Cmd+F | Focus search |
| Ctrl/Cmd+K | Clear screen (keeps the offset; new logs still append) |
| Ctrl/Cmd+S | Save code-view edits |
| Ctrl/Cmd+C | Copy selected log rows |
| Ctrl/Cmd+, | Open settings (applies and saves immediately) |
| Esc | Close dialogs |
| Enter (search) | Jump to the next match in highlight mode (fixed) |

**Row selection**: click a row to select it, Shift/Cmd-click to extend, right-click to copy the row or
selection; Cmd/Ctrl+C also copies. Link context menus add open-in-new-tab / open-in-browser / copy-link.

## Configuration & persistence

Settings file: macOS `~/Library/Application Support/logview/settings.json` · Linux
`~/.config/logview/settings.json` · Windows `%APPDATA%\logview\settings.json`

It contains level profiles (`configs`), level colors (`colors` / `colors_light`), search highlight
colors, UI/log font sizes and families, accent color, polling interval, line limit, initial/tail load
sizes, line-number and timestamp toggles, recents and their limit/purge policy, saved SSH hosts
(no credentials), and command presets (no credentials).

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

## Architecture

```
UI thread (GPUI + gpui-kit; self-drawn virtual list, visible rows only)
   │ crossbeam channel → Cmd { Pause | SeekStart | SeekEnd | Stop }
   ▼                    ← Msg { Append | Replace | Partial | Complete | Truncated | NoFollow | Error }
Tail worker thread per tab (poll size → incremental reads, 1 MB chunks, ≤8 MB per tick)
   ├─ LocalSource  fs::metadata + File::seek/read
   ├─ SshSource    ssh2::Session + SFTP stat/open/seek (5 retries, then fatal)
   ├─ HttpSource   reqwest blocking + Range (pinned validated IP; redirects re-checked per hop)
   └─ Command      local child process / SSH exec (merged stdout+stderr, exit code + backoff retry)
```

| Module | Responsibility |
|---|---|
| `main.rs` | GPUI startup, window options, CLI open |
| `app_view.rs` | Tabs, toolbar, virtual log view, dialogs, settings, message polling, recents, shortcuts |
| `theme.rs` | antd-v5-style dark/light palettes, fonts and scaling |
| `assets.rs` | Built-in SVG icon table (ionicons subset) |
| `level.rs` | Level constants/aliases, `LevelConfig`/`Rule`, regex engine |
| `lines.rs` | `LineStore`: bytes → lines, counts, line limit |
| `search.rs` | Search state and matcher (text/regex, highlight ranges) |
| `links.rs` | URL/path detection, overlap resolution, JSON detection |
| `ssrf.rs` | URL validation and blocked network ranges |
| `command.rs` | Command presets, argv expansion, built-in presets |
| `source.rs` | Source open/meta/read_at, SSH, HTTP client with pinned IP, credentials |
| `ssh_config.rs` | Import hosts from the system ssh config |
| `secrets.rs` | OS keychain storage, in-memory fallback |
| `store.rs` | `settings.json` read/write and legacy migration |
| `tail.rs` | Worker protocol and file/stream loops |
| `i18n.rs` | semantic-ID lookups (`t` / `tf` / `td` / `tn`); translations live in `locales/*.toml` and are compiled by `build.rs` |
| `code_input.rs` | Regex input highlighting |
| `code.rs` | Code/Markdown highlighting and preview limits |

## Development

```bash
cargo build                      # debug build (dependencies at opt-level=2)
cargo clippy --all-targets       # lint (kept warning-free)
cargo test --locked              # unit tests (71)
cargo run -- sample/app.log      # run with the sample log
```

`dev.sh` / `package.sh` are maintainer-local scripts and are not tracked. See
[CONTRIBUTING.md](CONTRIBUTING.md) for the workflow.

- The UI is fully self-drawn with GPUI components from gpui-kit; palette tokens live in `theme.rs`.
- Level detection runs when the worker appends; after a profile change the UI re-evaluates in frames
  (`relevel_step`, 100k lines per frame).
- Design notes: [DESIGN.md](DESIGN.md); prototype: [prototype.figcraft](prototype.figcraft).

### Packaging

```bash
cargo build --release            # target/release/logview
./scripts/make_app.sh            # (macOS) builds the .app bundle and registers file types
```

The bundle declares log/txt/out/err/json/csv document types, so Finder’s **Open With** works. Windows
uses “Open with → logview.exe” via argv; Linux packaging (`.desktop` / AppImage) is still missing.

## FAQ

**CJK glyphs show as boxes?** Set a font that contains CJK glyphs under Settings → Appearance →
Size and fonts, or leave it empty to follow the system font.

**`adb logcat` prints nothing?** Check `adb devices`; leave the device field empty to let adb choose,
and fill it when several devices are connected. If `adb` is not on `PATH`, use an absolute path in
the preset.

**Where is command history stored?** Each command tab spools output under `logview/spool/` in the
system cache directory; it is deleted on tab close or quit, truncated by “clear”, and leftovers are
swept at startup. The cap is 128 MB (the oldest half is dropped). If the spool cannot be written you
only get a live preview (with a notice). Spooled log text is briefly written to disk in plain text —
clear or close the tab if the output contains secrets.

**SSH reports authentication failure?** Check the username and credentials; key-file paths refer to
the **server’s** filesystem; agent auth needs `SSH_AUTH_SOCK` in the environment.

**HTTP opens with “following unsupported”?** The server ignores `Range`, so only whole-page viewing
is possible and auto-follow is disabled.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) and [CHANGELOG.md](CHANGELOG.md). To improve translations, edit
`locales/en.toml` or `locales/ru.toml` — the build fails if the three files have different keys, and
`cargo test` verifies that every ID used in the source exists. Code comments and commit
messages are in Chinese; the UI is available in Chinese, English and Russian; identifiers are English.

## License

[GNU AGPL-3.0](LICENSE) (SPDX: `AGPL-3.0-only`), Copyright (C) 2026 zher52.

If you modify and distribute this program, or offer it to users over a network, you must make the
complete corresponding source available as required by section 13 of the AGPL.
Bundled Ionicons icons are MIT; dependency licenses are listed in
[THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md).
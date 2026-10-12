# FastScanCleaner

[![CI](https://github.com/Benny93/fast-scan-cleaner/actions/workflows/ci.yml/badge.svg)](https://github.com/Benny93/fast-scan-cleaner/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/Benny93/fast-scan-cleaner)](https://github.com/Benny93/fast-scan-cleaner/releases/latest)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/macOS%20%7C%20Windows%20%7C%20Linux-blue)](#)

https://github.com/user-attachments/assets/b6c77ce3-8dd0-4354-8598-4af6f2667343

See what fills your disk and clean it up safely. Inspired by GrandPerspective, built for people who don't want to be computer experts.

## Download

Grab an installer from the [latest release](https://github.com/Benny93/fast-scan-cleaner/releases/latest):

| Platform | Packages |
|---|---|
| macOS | `.dmg` for Apple Silicon and for Intel |
| Windows | `.exe` (NSIS installer) |
| Linux | `.deb` and `.AppImage` (built on old glibc, so they run on most distros) |

Installers are currently unsigned: macOS may show a Gatekeeper warning (right-click → Open), and Windows SmartScreen an "unknown publisher" prompt.

## What you get

- **Map of your disk** — a treemap where big blocks are big folders and files. It fills in live while scanning. Click a block to zoom in; use browser-style back/forward to move around. Right-click blocks to select several at once (e.g. all `node_modules`) and press Delete to move them to the Trash in one go.
- **Plain-language advice** — every block is colour-coded: 🟢 green = safe to remove, 🟡 yellow = check first, 🔴 red = keep. Comes with a curated rules catalog (Xcode caches, Docker data, iOS backups, `node_modules`, …) and a legend. Switch colours between "tell blocks apart" and the traffic light at any time.
- **Nothing is deleted, it goes to the Trash** — so you can restore it. Only the optional system tools delete permanently, and they always ask first: empty Trash, Homebrew cleanup, Docker cleanup, Time Machine local snapshots, old iOS simulators, merged Git branches and stale worktrees.
- **AI assistant support (MCP)** — let Qoder, Claude or another assistant explore your disk with you, see what you see, and propose things to remove. It can never remove anything without your confirmation, except Trash moves inside folders you explicitly allow.
- **Comfortable defaults** — six languages (English, Deutsch, Français, Español, 中文, 日本語), light/dark/system theme, recent scans at a click. Settings live in `FastScanCleaner/settings.toml` inside your standard config directory.

## Connecting an AI assistant

1. In the app, open **Settings** and turn on **Allow AI assistants to control this app**, then restart.
2. Add the server to your assistant:

   **Qoder** — open Qoder IDE Settings (⌘⇧`,` / Ctrl+Shift+`,) → **MCP** → **My Servers** → **+ Add**, and paste:

   ```json
   {
     "mcpServers": {
       "fastscancleaner": {
         "type": "sse",
         "url": "http://127.0.0.1:7878/mcp"
       }
     }
   }
   ```

   Qoder auto-detects the Streamable HTTP endpoint. Save and close the file; a link icon means the connection is up, and expanding the entry lists the tools.

   **Claude** — click **Copy config for Claude** in Settings and paste it into your client's MCP configuration, or run:

   ```bash
   claude mcp add --transport http fastscancleaner http://127.0.0.1:7878/mcp
   ```

The server listens on `127.0.0.1` only (default port 7878) and rejects requests from other hosts.

**Tools:** `get_status`, `list_recent_scans`, `scan`, `refresh`, `get_tree`, `top_items`, `get_findings`, `navigate`, `select`, `show_screen`, `screenshot`, `request_trash`, `list_system_cleanups`, `request_system_cleanup`.

**Safety model:** every removal is confirmed by you in the app. The only exceptions are Trash moves inside folders you allow-listed; system cleanups always ask. There is deliberately no tool to change settings or the allowlist.

## Permissions

- **macOS**: grant _Full Disk Access_ to see every folder, and _Screen Recording_ for the assistant's `screenshot` tool (System Settings → Privacy & Security).
- **Windows / Linux**: some system folders need administrator rights and are skipped.

## Building from source

Requires [Rust](https://rustup.rs) (stable). On macOS, Xcode with the Metal toolchain is needed for GPUI. On Linux, install the system libraries listed in [`.github/workflows/ci.yml`](.github/workflows/ci.yml) first.

```bash
cargo run --release        # or: make run (foreground)
make test                  # cargo test
```

The `Makefile` also manages the app as a background process: `make start` / `stop` / `restart` / `status` / `logs`.

To produce installers locally (`.app`/`.dmg`, NSIS `.exe`, `.deb`/AppImage):

```bash
cargo install cargo-packager --locked
cargo packager --release
```

## Releasing

Bump `version` in `Cargo.toml`, commit, then `make release` — it tags and pushes, and CI builds installers for all platforms into a GitHub Release.

## License

MIT — see [LICENSE](LICENSE).

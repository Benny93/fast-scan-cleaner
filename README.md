# FastScanCleaner

See what fills your disk and clean it up safely. Inspired by GrandPerspective, built for people who don't want to be computer experts.

- **Map of your disk**: big blocks are big folders and files. Click to zoom in; right-click blocks to select several (e.g. all `node_modules`) and press Delete to move them to the Trash in one go. Colour it to tell blocks apart or as a traffic light (green = safe to remove, light green = partly, white = no advice), with a legend. It fills in live while scanning.
- **Plain-language advice**: green = safe to remove, yellow = check first, red = keep.
- **Files you remove go to the Trash**, so you can restore them. Only the optional System tools (empty Trash, Homebrew/Docker cleanup, merged Git branches and worktrees, …) delete permanently — and always ask you first.
- **Six languages**: English, Deutsch, Français, Español, 中文, 日本語.
- **AI assistant support (MCP)**: let Qoder, Claude or another assistant explore your disk with you. It must ask before removing anything, except in folders you allow.
- macOS, Windows and Linux.

## Build

`make run` (foreground), `make start` / `make stop` / `make restart` / `make status` / `make logs` (background), `make test`.

Requires Rust (https://rustup.rs). On macOS, Xcode with the Metal toolchain is needed for GPUI.

```bash
cargo run --release
```

On Linux, install the GPUI system libraries listed in `.github/workflows/ci.yml` first.

## Connect an AI assistant

1. In the app open **Settings**, turn on **Allow AI assistants to control this app** and restart the app.
2. Click **Copy config for Claude** and paste it into your client's MCP configuration, or run:

   ```bash
   claude mcp add --transport http fastscancleaner http://127.0.0.1:7878/mcp
   ```

The server only listens on `127.0.0.1` and rejects requests from other hosts. Tools: `get_status`, `list_recent_scans`, `scan`, `refresh`, `get_tree`, `top_items`, `get_findings`, `navigate`, `select`, `show_screen`, `screenshot`, `request_trash`, `list_system_cleanups`, `request_system_cleanup`. Every removal is confirmed by you in the app (except Trash moves inside folders you allow-listed; system cleanups always ask). There is deliberately no tool to change settings or the allowlist.

## Permissions

- **macOS**: grant _Full Disk Access_ to see every folder, and _Screen Recording_ for the assistant's `screenshot` tool (System Settings → Privacy & Security).
- **Windows / Linux**: some system folders need administrator rights and are skipped.

## License

MIT

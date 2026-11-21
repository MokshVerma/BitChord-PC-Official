# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

BitChord PC: a native YouTube Music desktop client in Rust (egui/eframe UI, rodio/symphonia audio). Single binary crate `bitchord` (edition 2024), targeting Windows, macOS and Linux. Default branch is `rust-2.0`. License GPL-3.0.

## Commands

```sh
cargo build --release          # output in target/release/
cargo run                      # dev build (dependencies are optimized at opt-level 2 even in dev)
cargo test --locked            # CI runs this on Windows + Ubuntu 22.04
cargo test <name>              # single test by name substring; tests are inline #[cfg(test)] modules (most in src/ytm.rs)
cargo clippy --locked --all-targets -- -D warnings   # CI treats warnings as errors
```

- Linux needs: `libssl-dev libasound2-dev libgtk-3-dev libwebkit2gtk-4.1-dev libxdo-dev libayatana-appindicator3-dev libdbus-1-dev`.
- Release builds use fat LTO, `codegen-units = 1`, and `panic = "unwind"` on purpose (a panic in one request must fail that request, not close the app).
- The release workflow checks that the version in `Cargo.toml` matches the tag; bump it when releasing. Release notes live in `.github/release-notes`.
- `server/` is a separate Cargo project (not a workspace member): a Cloudflare Worker (workers-rs, `wasm32-unknown-unknown`) that relays "Jam" (listen-together) messages. Deploy/dev with `npx wrangler` from `server/`; end-to-end test with `cd server/smoke && cargo run`. Protocol is documented in `server/README.md`; the client side is `src/jam.rs` + `src/ui/jam.rs`, so change both sides together.
- `site/` is the GitHub Pages site; `installer/`, `linux/`, `macos/` hold packaging (Inno Setup, install.sh, bundle.sh). Each has a `portable.txt` marker used for portable builds.

## Architecture

**UI ↔ backend split.** `src/ui/` (state, per-frame layout, playback logic) never does network work directly. It sends `Req` values over a crossbeam channel to `src/backend.rs`, which runs a small tokio runtime, and drains `Resp` values once per frame; every response wakes the UI. New features usually mean: add a `Req`/`Resp` variant in `backend.rs`, handle the request there, and handle the response in `src/ui/responses.rs`. `src/ui/mod.rs` documents the layout (animated album-art shader background → sidebar/top bar/pages → player bar → now-playing overlay → toasts); `src/ui/` is split by concern (pages, queue, playback, lyrics prefs, mini player, etc.).

**YouTube Music access** is layered: `ytmapi.rs` / `innertube.rs` / `ytm.rs` (API requests and response parsing, where most unit tests live), `webmusic.rs`, `ytdlp.rs` and `jsc.rs` (embedded QuickJS via `rquickjs`, used for stream signature handling), `model.rs` (shared types such as `Track`, `Card`, `Target`).

**Playback pipeline** (in `backend.rs`): resolve stream → progressive download (`stream.rs`) → build decoder on a blocking thread → hand to the audio engine (`audio.rs`, rodio-based, handles crossfade/automix/normalization/output device). Hi-res/Lossless/Dolby Atmos comes from addon sources via `hires.rs` and `dash.rs` (E-AC-3 JOC decoded with `filmcraft-ac3`, Opus via `opus.rs`), falling back to YouTube audio.

**Integrations:** `discord.rs` (Rich Presence), `lastfm.rs`, `transfer/` (Spotify import), `lyrics.rs` + `translate.rs` (provider fallbacks), `sponsorblock.rs`, `media.rs` (system media controls via souvlaki), `tray.rs`, `downloads.rs`, `update.rs` (self-update; the portable build downloads the new version).

**Sign-in and secrets.** Google/Spotify sign-in runs in a separate webview window by re-spawning the same exe in a child-process mode (flags handled at the top of `main()`, see `login.rs` / `turnstile.rs`). Credentials are stored via `keyring` (Credential Manager / Keychain / Secret Service) through `vault.rs`.

**Rendering.** eframe with glow by default; `renderer.rs` is an opt-in wgpu renderer (Vulkan/Metal), and `shader.rs` / `motion.rs` / `theme.rs` / `preset.rs` drive the animated background and theme presets. `build.rs` embeds the Windows icon and manifest (SegmentHeap) and generates a table of theme cover thumbnails from `assets/themes/covers/<id>.jpg`, so a new theme preset needs its cover added there.

**Data directory.** Settings, caches and sign-in live in the platform data dir, or in a `data` folder beside the exe when a `portable.txt` is present (see `main.rs` and `update::portable_dir`). Keep paths going through that logic.

## Notes

- Platform-specific code is gated with `cfg(windows)` / `cfg(target_os = ...)`; a change that touches one platform's code should still compile on the others (CI builds Windows and Linux; macOS is tested only in the release job).
- The README states the project is unaffiliated with YouTube/Google/Apple and does not bypass DRM, subscriptions or regional restrictions.

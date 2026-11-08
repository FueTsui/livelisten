# 实时聆听

Context for AI agents (Claude Code, Codex, etc.) working in this repository. User-facing documentation is in [README.md](README.md) (Simplified Chinese).

- Project name: live-listen
- Project type: rust
- Standard version: personal-dev-standard 1.0.1
- Created: 2026-11-06

## Purpose

Windows 11 桌面语音工具：把说话实时写入当前输入框，并提供实时字幕和随手记卡片。

## Common commands

```powershell
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked -- --skip secrets::tests::store_read_and_delete
python -B -m unittest discover -s providers\tests -v
.\scripts\build.ps1                 # Rust core + WinUI 3 app (Debug); -Release, -Publish
cargo run --locked -p live-listen-cli -- providers --json
```

## Layout

- `crates/live-listen-core`: all business logic (audio, hooks, input, focus, dictation planning, captions, provider processes, settings, secrets, note cards in `cards.rs` and their engine commands in `engine/notes.rs`).
- `crates/live-listen-ffi`: C ABI `livelisten_native.dll` (`ll_start`, `ll_call`, `ll_free`, `ll_destroy`, `ll_version`); JSON in and out.
- `crates/live-listen-cli`: `live-listen.exe` (`devices`, `providers`, `check`, `listen`, `config`).
- `app/LiveListen`: C# WinUI 3 shell, UI built in code; no business rules.
- `setup`: per-user installer and uninstaller (`setup.exe`, egui/eframe 0.31.1 like SightOCR's) and `pack.exe`, which appends the zipped app to it. Product details come from `setup/install-manifest.json`; see ADR 0005.
- Build output: Rust in `target/x86_64-pc-windows-msvc/`, .NET in `target/dotnet/` (set by `app/Directory.Build.props`).
- `providers/doubao_live.py`: Doubao bridge for provider protocol v1; it imports the sibling `doubao_asr` module, which must not be modified.
- Docs: `docs/ARCHITECTURE.md`, `docs/PROVIDER_PROTOCOL.md`, `docs/DESIGN.md`, `docs/THREAT_MODEL.md`, `docs/adr/`.

## Pitfalls

- The native DLL must not be named `livelisten.dll`: it collides with the managed `LiveListen.dll` on a case-insensitive file system.
- Overlay windows (bar, captions) must never activate (`WS_EX_NOACTIVATE`); taking focus breaks dictation into the target text box. Desktop cards are the exception: `Overlay.Prepare(window, floating: false)` keeps them activatable and not topmost so their text can be edited.
- Send Backspaces one at a time with `BACKSPACE_PACE` between them (`win/input.rs`). TSF input methods such as Microsoft Pinyin drop or reorder a burst of Backspaces, which corrupts live-mode corrections.
- Qt apps (WeChat is `Qt51514QWindowIcon`) record every Unicode key event under VK_PACKET, and Microsoft Pinyin swallows the key-up of full-width punctuation, so the next Unicode key becomes an auto-repeat of that punctuation. `input::route` sends `WM_CHAR` and Backspace messages to Qt windows instead. A Shift held around the next key also clears the record but toggles the input method between Chinese and English, so do not use it. Test with a PyQt window, never with a real chat window.
- A lone-modifier hold key (RCtrl) passes its first key-down to the system, and its auto-repeats keep it down. `Matcher::tick` releases it in the system once dictation starts (mask key first) and swallows its repeats and key-up; a key pressed during the hold gets the modifier pressed again first (`Decision::repress`). Without this, `input::route` sees Ctrl held and never uses messages for Qt windows during a hold.
- Doubao dictation text has no punctuation and `stable` stays 0 until the session ends; the final adds punctuation and drops fillers, so a long session ends in one big rewrite. Dictation splits its session at pauses after `DICTATION_SPLIT_AFTER` (`Dictation::next` takes the audio while the old session delivers its final). The hook's held input limit (`BUFFER_LIMIT`) only runs while input is actually held, so the engine's `FINISH_MAX` decides how long to wait.
- Doubao confirms caption text only when a session ends. Caption sessions end at a pause after `CAPTION_CYCLE_AFTER`; without it, captions under continuous sound never become final.
- Cards are user data in `%APPDATA%\LiveListen`; tests must use a temporary folder, and manual runs should move test data out afterwards.
- Run the GUI for tests with `LIVE_LISTEN_CONFIG_DIR` set to a temporary folder whose `settings.json` has `"version": 3` and every hotkey cleared (`""`). That instance is separate from the user's and never writes the startup entry, but it shares Credential Manager: an older settings version runs the upgrade that deletes legacy credentials.
- `cargo test --locked` also runs `secrets::tests::store_read_and_delete`, which writes and deletes a dummy Credential Manager entry; validation records filter it with `-- --skip secrets::tests::store_read_and_delete`.
- The bridges run at most two sessions at once (`MAX_ACTIVE`) and hold a newer session's messages until the older one has sent `end` (`SessionOrder`): the engine reads text of a newer session as the end of the one before.
- Logs record events, codes and lengths only, never recognized text, keystrokes or secrets.
- Test the installer only with the debug build and `SETUP_TEST_ROOT` set to a short temporary folder (the install path length check counts the staging folder): it redirects the install folder, Start menu, AppData and registry (`registry.json`). Never run the release installer for real on this machine; `/VERIFY`, bad arguments and damaged packages write nothing. `Start-Process -Wait` also waits for the app an installer starts.
- Crates published without a license file need a pinned copy in `third_party/licenses/` and an entry in `SUPPLIED_LICENSES` (`scripts/package_portable.py`), or packaging stops.
- End-to-end runs can replace audio with `LIVE_LISTEN_TEST_AUDIO` and `LIVE_LISTEN_TEST_CAPTIONS_AUDIO`; they send audio to the real Doubao service.

## Conventions

- Business logic lives only in the core module; CLI, FFI and UI are thin adapters (GEN-02, AGT-02).
- Code comments and agent instruction files are in English; README, CHANGELOG and `docs/` are in Simplified Chinese (ENG-10).
- Commit messages: Conventional Commits with an English type prefix and a Chinese description (ENG-03).
- Interface changes update the README "接口" section (DOC-02); key decisions go into `docs/adr/` (DOC-03).
- Talk to the user in Simplified Chinese.

## Prohibitions

- Never read or print `.env`, `.dev.vars`, `*.pem` or `*.key`; inject secrets with `op run --env-file=.env.tpl -- <command>` (AI-03).
- Get explicit approval before pushing, publishing, tagging or changing live data (FLW-03).
- Never commit build output, local configuration or private samples (ENG-02, ENG-06).

## Verification

- After changing code, run at least the format check, static check and tests listed above.
- Run secret scans against a snapshot containing only the files intended for publication.
- Record the checks and their scope in release notes. Skipped tests do not count as passed (FLW-02).

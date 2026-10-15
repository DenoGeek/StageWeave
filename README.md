# StageWeave

A desktop app for worship teams and live music. Split any song into stems, mix them live, build sing-along lyric slides, and export lyric videos. It runs on macOS, Windows and Linux.

![The song editor looping a cue over the Clouds background](docs/screenshots/editor-cue.png)

## Features

- **Setlists.** Make one project per service or event. Drop songs in, set the key, BPM and notes, and switch songs with PageUp / PageDown.
- **Search everything.** Press **⌘K** (Ctrl+K) to find any project or song across every setlist, by title, artist, key, notes or a line of the lyrics. You can also filter projects on Home or songs within a setlist.
- **Reuse songs.** **Add from Library…** brings a song from another setlist into this one, already split, with its lyrics, timing, cues and mix. Files are hard-linked where possible, so a song used in many setlists only takes up disk space once.
- **Stem splitting.** Separate vocals, drums, bass and other with Demucs, or karaoke and instrumental with UVR models. It all runs on your machine, and the engine installs itself the first time you split a song.
- **Live mixer.** Each stem has mute, solo, volume and pan, plus level meters and a master fader.
- **Cues.** While playing, press **Q** to mark the start of a section and **Q** again to loop it. Each cue keeps its own mix (for example, the chorus with drums muted). Exit it with **Esc**, the **Exit Cue** button, or by clicking elsewhere on the timeline.
- **Sections.** Press **S** where a part of the song starts and **S** again where it ends. Playback never stops: whenever the playhead is inside a section, that section's mix plays (for example, drums muted in the bridge, bass down in the verse), and the song's own mix comes back after it. Change mutes and volumes while the section plays, or right-click it in the **Sections** lane to rename it and set its mix. The mix switches on the exact sample, and lyric video exports follow it too.
- **Lyric slides.**
  - Karaoke-style highlighting, with an optional translation line.
  - Themes, text size, and "next line" and progress bar options.
  - A separate full-screen lyric window for the projector.
- **Motion backgrounds.**
  - Three starter loops are built in: Clouds, Flowers and Drapes.
  - You can also use your own image or short video loop.
  - A darken slider keeps the text readable.
- **AI lyrics (optional).**
  - Transcribe a song with Whisper (OpenAI or Groq), or align lyrics you already have to the singer's phrases.
  - Alignment can run fully on-device.
  - Transcription works phrase by phrase on the vocals stem, which helps a lot with non-English songs.
- **Import lyrics.** Click **Import…** next to **AI**, or drop a file on the song. Formats:
  - LRC, SRT and WebVTT keep their timing.
  - ChordPro, OpenLyrics / OpenLP, OpenSong and plain text are cleaned of chords and section labels, then timed automatically.
- **Lyric editing.**
  - Edit lines on the timeline or in a list, with full undo.
  - **N** adds a line at the playhead. **E** stretches the previous line to the playhead.
  - Tap-sync lets you stamp lines live with Enter.
- **Export lyric videos.** Render an MP4 (720p or 1080p, 25 or 30 fps) with the original audio or your current stem mix.
- **Move projects between computers.** Export a project as a `.zip` (stems optional) and import it on another laptop, or just drop the zip on the window.

| Setlist | Perform |
| --- | --- |
| ![Setlist](docs/screenshots/setlist.png) | ![Perform workspace over the Drapes background](docs/screenshots/perform.png) |
| **Lyrics over a motion background** | **AI lyric alignment** |
| ![Lyrics over the Flowers background](docs/screenshots/lyrics-background.png) | ![Align lyrics dialog](docs/screenshots/ai-align.png) |

## Install

Download the latest build from the [Releases](../../releases) page:

| Platform | File |
| --- | --- |
| macOS (Apple Silicon) | `StageWeave_<version>_aarch64.dmg` |
| macOS (Intel) | `StageWeave_<version>_x64.dmg` |
| Windows | `StageWeave_<version>_x64-setup.exe` |
| Debian / Ubuntu | `stageweave_<version>_amd64.deb` (`sudo apt install ./stageweave_*_amd64.deb`) |

The macOS builds aren't notarized yet. Drag StageWeave from the DMG into **Applications**, then, if macOS says the app "is damaged" or "can't be opened", run this in Terminal and open it again:

```sh
xattr -dr com.apple.quarantine /Applications/StageWeave.app
```

### What gets downloaded on first use

- **Separation engine.** The first split installs a private Python environment with PyTorch, Demucs and UVR (about 2 GB) into the app-data folder. Models are fetched when you first use them. You can manage them in **Window → Models**.
- **ffmpeg.** It's needed for video backgrounds and MP4 export. StageWeave uses the ffmpeg on your PATH if there is one, otherwise it downloads a static build once.

## Keyboard shortcuts

| Key | Action |
| --- | --- |
| Space | Play / pause |
| Home / End | Jump to start / end |
| ← / → | Nudge 5 seconds |
| L | Toggle loop |
| ⌘/Ctrl + K (or F) | Search all projects, songs and lyrics |
| Q | Mark a cue start, then close the cue and loop it |
| S | Mark a section start, then close it (it keeps playing with its own mix) |
| Esc | Exit the cue, or cancel marking a cue or section |
| N | New lyric line at the playhead (asks before replacing a line already there) |
| E | Stretch the previous lyric line to the playhead |
| Enter | Stamp a line (tap sync) |
| ⌘/Ctrl + Z | Undo (⇧⌘Z or ⌘Y to redo) |
| PageDown / PageUp | Next / previous song (works with foot pedals) |
| ⌘/Ctrl + scroll | Zoom the timeline |
| ⇧⌘P | Lyric window |
| F / double-click | Fullscreen lyric window |

## Where your data lives

| What | Where |
| --- | --- |
| Songs, stems, backgrounds | The workspace folder (default `~/Music/StageWeave`, change it in Settings) |
| Database, engine, models, API keys | The OS app-data folder, e.g. `~/Library/Application Support/com.CodeFamiliar.StageWeave` on macOS |

- **API keys** are stored in a private file that only your user account can read. They are never stored in the system keychain.
- **Coming from MusicKit?** Data from the app's former name is moved over automatically on first launch.

## Building from source

You need Rust (the version is pinned in `rust-toolchain.toml`; `rustup` picks it up automatically).

```sh
cargo run --release
cargo run --release -- --demo   # a sample setlist with mock audio; nothing touches your real data
cargo test
cargo clippy --all-targets
```

On Linux, install the audio and GUI development headers first:

```sh
sudo apt install pkg-config libasound2-dev libgtk-3-dev libxkbcommon-dev libxkbcommon-x11-dev \
  libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libwayland-dev libssl-dev
```

`src/dev.rs` lists the `STAGEWEAVE_*` environment variables used for development and for the screenshots in this README.

## Releasing

Pushing a tag that starts with `v` runs [`.github/workflows/release.yml`](.github/workflows/release.yml). It builds the .dmg, .exe and .deb packages and attaches them to a GitHub release. The tag must match the `version` in `Cargo.toml`:

```sh
git tag -a v0.1.0 -m "v0.1.0"
git push origin v0.1.0
```

## Credits

- **Starter backgrounds:** stock footage from [Mixkit](https://mixkit.co) under the Mixkit Stock Video Free License. See [`assets/backgrounds/CREDITS.md`](assets/backgrounds/CREDITS.md).
- **Separation:** [Demucs](https://github.com/adefossez/demucs) and [python-audio-separator](https://github.com/nomadkaraoke/python-audio-separator) (UVR models).
- **UI:** built with [egui](https://github.com/emilk/egui).

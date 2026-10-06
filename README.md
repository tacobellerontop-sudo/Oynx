# Oynx

Oynx is a Rust desktop Spotify client built with eframe/egui and Librespot.

## Install

Download `oynx-<version>-windows-x86_64-setup.exe` from the [releases page](https://github.com/tacobellerontop-sudo/Oynx/releases) and run it. Oynx installs for your account only, under `%LOCALAPPDATA%\Programs\Oynx`, so no administrator rights are needed; it adds a Start menu shortcut (and a desktop shortcut if you tick the box). Uninstall it from **Settings > Apps** in Windows; your Oynx settings and Spotify sign-in are kept.

Oynx updates itself through the same installer. With **Automatic** updates on (Settings > Updates), new versions download in the background and install when you quit Oynx; **Restart now** installs straight away. The installer is not code-signed, so SmartScreen warns about an unknown publisher the first time: choose **More info**, then **Run anyway**.

Copies of Oynx 0.1.2 and earlier were standalone exes and can't install the setup program themselves; download and run the installer once to switch over.

## Run locally

```powershell
cargo run
```

For everyday listening, use an optimized build; the debug build uses several times more CPU for audio playback:

```powershell
cargo run --release
```

## System tray

By default, closing the window hides Oynx to the system tray instead of quitting, so playback continues; choose **Quit Oynx** under **When closing** in Settings to exit instead. While hidden, Oynx stops rendering and releases its album-art textures. Left-click the tray icon to show the window again, or right-click it for Play/Pause, Next, Previous, and **Quit Oynx**.

## Equaliser and waveform

The waveform beside the lyrics on the Now Playing screen is a live spectrum of the audio you hear, with bass in the middle and treble towards the ends. It is timed to the speakers rather than the decoder, so it stays in step with the music. The button at the bottom left opens a ten-band equaliser with presets; changes apply within a moment and are saved to `%APPDATA%Oynxqualizer.json`.

## Exploring music

- **Song credits**: the **Credits** tab on the Now Playing screen (or **Song credits** in the player's **…** menu) shows who wrote, produced, mixed and played on the song, the songs it samples, the songs that sample it, and other recordings of it. Click a name or song to search for it. Spotify's own SongDNA isn't available to other apps, so Oynx combines the credits in Spotify's track data with [MusicBrainz](https://musicbrainz.org), the open music encyclopedia; songs MusicBrainz doesn't know yet show fewer credits.
- **Song menu**: hover a song and click **…**, or right-click it, to play it next, add it to the queue, start a song radio, add it to one of your playlists, go to its artist or album, or copy its link. The player's **…** menu has the same actions for the song that's playing.
- **Song radio** plays songs like the one you picked, chosen by Spotify.
- **Artist and album pages** show an artist's popular songs, albums and singles, and an album's full track list.
- **Your top songs and artists** in the Library shows what you've played most over the last month, six months and year.
- **Recently played** in the queue panel's menu lists what this account played lately, on any device.
- **Lyrics sources**: pick where lyrics come from in **Settings > Lyrics**, or from the link beside the Lyrics tab: Spotify's own lyrics (mostly Musixmatch), [LRCLIB](https://lrclib.net), or NetEase Cloud Music, which is strong on Chinese, Japanese and Korean songs. **Auto** tries them in that order and prefers lyrics timed to the song.
- **Sleep timer** in the player's **…** menu pauses playback after 5 to 90 minutes or at the end of the song.

## Themes

**Settings > Themes** changes how Oynx looks:

- **Mode**: Dark, Light, or **Match Windows** to follow the Windows app mode.
- **Colours**: text, secondary text, background, panels, accent and visualiser. Dark and light mode keep separate colours; start from a preset or pick any colour.
- **Background image**: any PNG, JPEG, WebP or BMP, with its visibility and blur adjustable.
- **Font**: Inter by default, one of the fonts that ship with Windows, or any TrueType/OpenType file.
- **Window**: background opacity and an acrylic blur of whatever is behind the window. Turning these on the first time needs a restart, because a see-through window has to be created that way; Oynx then uses its OpenGL renderer.

The theme is saved to `%APPDATA%\Oynx\theme.json`. **Restore the default theme** puts everything back.

## Spotify sign-in

Oynx uses Spotify's browser-based OAuth authorization-code flow with PKCE. It does not ask for or store your Spotify password.

1. Launch Oynx.
2. Select **Sign in with Spotify** in the connection banner.
3. Approve the request in your browser.
4. If you configured a separate Web API client, approve its second browser authorization when it opens.
5. After the session is ready, Oynx loads your Spotify playlists, Liked Songs, and personalized recommendations automatically. Use **Search** to run a different Spotify query.

The initial catalog is seeded with real Spotify tracks: Blinding Lights, Never Gonna Give You Up, All I Want, Borderline, A New Error, Outro, and Billie Jean. After sign-in, Oynx replaces the home recommendations with Spotify's personalized results, loads the playlists from your account, and opens **Liked Songs** from the sidebar to show tracks saved by your account. The playlist view can load the tracks from any playlist you open.

## Settings

Open **Settings** in the left sidebar to configure the streaming client ID, Web API client ID, and redirect URIs without environment variables. For the Web API field, create a second Spotify app and register `http://127.0.0.1:8989/login` as its redirect URI (or enter a different registered URI). Spotify rate limits are per app, so sharing the Librespot client can produce HTTP 429 responses. Oynx never asks for a client secret. The Settings page also shows the exact config and cache paths.

Saved settings are used by the app, while these environment variables take precedence when set:

```powershell
$env:OYNX_SPOTIFY_CLIENT_ID = "your-streaming-client-id"
$env:OYNX_SPOTIFY_REDIRECT_URI = "http://127.0.0.1:8898/login"
$env:OYNX_SPOTIFY_WEB_API_CLIENT_ID = "your-separate-web-api-client-id"
$env:OYNX_SPOTIFY_WEB_API_REDIRECT_URI = "http://127.0.0.1:8989/login"
```

Oynx requests `user-library-read`, `user-library-modify`, playlist-read, `user-top-read`, and streaming permissions during sign-in, plus `user-read-recently-played` and playlist-modify for Recently played and Add to playlist. If you signed in with a build older than 1.2.0, everything else keeps working; use **Sign out** and sign in again once to turn on those two features.

Librespot requires a Spotify Premium account. Reusable credentials, the Web API refresh token, and the audio cache are stored locally by Oynx/Librespot under:

```text
%LOCALAPPDATA%\Oynx\librespot
```

The default OAuth client and redirect URI are the values used by Librespot's desktop examples. They can be overridden for a custom Spotify application with:

```powershell
$env:OYNX_SPOTIFY_CLIENT_ID = "your-client-id"
$env:OYNX_SPOTIFY_REDIRECT_URI = "http://127.0.0.1:8898/login"
cargo run
```

The redirect URI must be registered for the corresponding client ID in Spotify's developer dashboard.

Oynx also caches playlist, Liked Songs, and recommendation responses briefly and serves the last good copy during a short Spotify rate-limit event. The Liked Songs view follows Spotify's `/v1/me/tracks` pagination so larger libraries can be loaded in order. Playlist and playlist-track requests use the same bounded pagination/cache approach. Album artwork is fetched and decoded on the worker, downsampled before upload, cached under the local data directory, and installed a few images per frame so resizing or moving the window stays responsive. Playback now supports a local queue, shuffle, repeat-off/all/one, real Spotify library save/remove actions, LRCLIB synced/plain lyrics, and paused session resume. Web API jobs run outside the playback command loop so slow library requests do not block play, pause, next, or volume controls. This is the same cache-and-stale-fallback approach used by MYX.

The Web API transport/cache design was adapted from the MIT-licensed [MYX](https://github.com/HaseebKhalid1507/Myx) player. The ten-band equaliser (`src/audio/equalizer.rs`) is vendored from MYX (MIT, (c) 2026 Haseeb Khalid), and the spectrum analyser behind the Now Playing waveform (`src/audio/visualizer.rs`) is vendored from MYX, which adapted it from [spotify-player](https://github.com/aome510/spotify-player) (MIT, (c) 2021 Thang Pham). The bundled Inter font is distributed under the SIL Open Font License; see `assets/fonts/Inter-LICENSE.txt`.

On Windows, the Settings page writes the JSON configuration under `%APPDATA%\Oynx\config.json`; Librespot credentials, the cached Web API token, cached responses, album artwork, session resume state, and audio files remain under `%LOCALAPPDATA%\Oynx\librespot`.

## Building on CI

`.github/workflows/windows-pr.yml` runs on every pull request. It builds the release executable, asserts the binary is linked against the Windows GUI subsystem (so no console window appears), then launches it and confirms a top-level `Oynx` window is actually created. It then packages the exe into the installer with `scripts/build-installer.ps1` (Inno Setup, script in `installer/oynx.iss`), installs and uninstalls it silently with `scripts/test-installer.ps1`, and uploads the exe and installer as artifacts.

`.github/workflows/release.yml` runs the same checks when a `v*` tag is pushed and attaches the installer, with its SHA-256 in the release notes, to the GitHub release. To build the installer locally, install [Inno Setup 6](https://jrsoftware.org/isinfo.php), then run:

```powershell
cargo build --release
.\scripts\build-installer.ps1 -Exe target\release\oynx.exe
```

## License

MIT, (c) 2026 tacobellerontop-sudo. See [LICENSE](LICENSE).

Parts of Oynx are vendored from, or adapted from, other MIT-licensed projects,
and the bundled font is under the SIL Open Font License. Attribution is in
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

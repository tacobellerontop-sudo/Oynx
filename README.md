# Oynx

Oynx is a Rust desktop Spotify client built with eframe/egui and Librespot.

## Run locally

```powershell
cargo run
```

For everyday listening, use an optimized build; the debug build uses several times more CPU for audio playback:

```powershell
cargo run --release
```

## System tray

Closing the window hides Oynx to the system tray instead of quitting, so playback continues. While hidden, Oynx stops rendering and releases its album-art textures. Left-click the tray icon to show the window again, or right-click it for Play/Pause, Next, Previous, and **Quit Oynx**.

## Equaliser and waveform

The waveform beside the lyrics on the Now Playing screen is a live spectrum of the audio you hear, with bass in the middle and treble towards the ends. It is timed to the speakers rather than the decoder, so it stays in step with the music. The button at the bottom left opens a ten-band equaliser with presets; changes apply within a moment and are saved to `%APPDATA%Oynxqualizer.json`.

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

Oynx requests `user-library-read`, `user-library-modify`, playlist-read, `user-top-read`, and streaming permissions during sign-in. If you already signed in with an older build, use **Sign out** and sign in again once so Spotify can grant the new scopes.

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

The Web API transport/cache design was adapted from the MIT-licensed [MYX](https://github.com/HaseebKhalid1507/Myx) player. The ten-band equaliser (`src/audio/equalizer.rs`) is vendored from MYX (MIT, (c) 2026 Haseeb Khalid), and the spectrum analyser behind the Now Playing waveform (`src/audio/visualizer.rs`) is vendored from MYX, which adapted it from [spotify-player](https://github.com/aome510/spotify-player) (MIT, (c) 2021 Thang Pham). The queue-panel layout, compact navigation, and dark visual system were inspired by the MIT-licensed [Spotifast](https://github.com/crmne/spotifast) client. The bundled Inter font is distributed under the SIL Open Font License; see `assets/fonts/Inter-LICENSE.txt`.

On Windows, the Settings page writes the JSON configuration under `%APPDATA%\Oynx\config.json`; Librespot credentials, the cached Web API token, cached responses, album artwork, session resume state, and audio files remain under `%LOCALAPPDATA%\Oynx\librespot`.

## Building on CI

`.github/workflows/windows-pr.yml` runs on every pull request. It builds the release executable, asserts the binary is linked against the Windows GUI subsystem (so no console window appears), then launches it and confirms a top-level `Oynx` window is actually created before uploading the exe as an artifact.

## License

MIT, (c) 2026 tacobellerontop-sudo. See [LICENSE](LICENSE).

Parts of Oynx are vendored from, or adapted from, other MIT-licensed projects,
and the bundled font is under the SIL Open Font License. Attribution is in
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

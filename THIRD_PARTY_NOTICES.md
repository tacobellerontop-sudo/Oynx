# Third-party notices

Oynx is MIT licensed (see [LICENSE](LICENSE)). It builds on and incorporates
work from other projects. This file records what came from where, so the
attribution required by those licenses travels with the source.

## Vendored and adapted code

| Oynx file | Origin | License |
| --- | --- | --- |
| `src/audio/equalizer.rs` | Vendored from [MYX](https://github.com/HaseebKhalid1507/Myx) (`src/audio/equalizer.rs`) | MIT, (c) 2026 Haseeb Khalid |
| `src/audio/visualizer.rs` | Vendored from [MYX](https://github.com/HaseebKhalid1507/Myx) (`src/audio/visualizer.rs`), which itself adapted [spotify-player](https://github.com/aome510/spotify-player) (`ui/streaming.rs`) | MIT, (c) 2026 Haseeb Khalid; MIT, (c) 2021 Thang Pham |
| `src/spotify.rs` | Web API transport and cache design adapted from [MYX](https://github.com/HaseebKhalid1507/Myx) | MIT, (c) 2026 Haseeb Khalid |

Both vendored files carry a header comment naming their origin and listing the
changes Oynx made, so the provenance is visible at the top of each file and not
only here.

### Changes made to vendored code

- **`src/audio/equalizer.rs`** — librespot paths moved to the umbrella crate,
  settings are persisted to disk, and `response_db` is exposed for the UI curve.
- **`src/audio/visualizer.rs`** — the rodio backend queues roughly half a second
  of audio ahead of the speakers, so analysed frames are stamped with a
  presentation time to keep the spectrum in step with what is actually heard.

## MYX

MIT License

Copyright (c) 2026 Haseeb Khalid

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.

## spotify-player

The frequency-band visualizer in `src/audio/visualizer.rs` originates here,
adapted by MYX and vendored by Oynx.

MIT License

Copyright (c) 2021 Thang Pham

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.

## Inter

`assets/fonts/InterVariable.ttf` is distributed under the SIL Open Font
License 1.1. The full license text ships alongside the font at
[`assets/fonts/Inter-LICENSE.txt`](assets/fonts/Inter-LICENSE.txt).

## Inspiration

The queue-panel layout, compact navigation, and dark visual system were
inspired by [Spotifast](https://github.com/crmne/spotifast) (MIT). No Spotifast
code is copied into this repository.

## Build-time dependencies

Oynx's own license does not cover its dependencies, which carry their own terms.
Run `cargo tree` for the full graph, or `cargo license` / `cargo about` to
generate a consolidated report. Notable large dependencies include
[librespot](https://github.com/librespot-org/librespot) (MIT),
[egui](https://github.com/emilk/egui) (MIT OR Apache-2.0),
[rodio](https://github.com/RustAudio/rodio) (MIT OR Apache-2.0), and
[tray-icon](https://github.com/tauri-apps/tray-icon) (MIT OR Apache-2.0).

There is **no strong copyleft** (no GPL-only or AGPL) anywhere in the dependency
graph, so distributing a built Oynx binary does not oblige you to release
anything under Oynx's own terms. Two things are worth knowing if you ship a
binary:

- The `symphonia` family, pulled in by rodio for audio decoding, is
  **MPL-2.0**. That is file-level copyleft: it requires modified versions of
  those files to stay available under MPL-2.0, but it does not spread to the
  larger work that links them. Unmodified, it is a normal dynamic/static
  dependency.
- `self_cell` is `Apache-2.0 OR GPL-2.0-only` and `priority-queue` is
  `LGPL-3.0-or-later OR MPL-2.0`. Both are dual-licensed with a permissive
  option, so nothing is owed under the copyleft terms.

This is a pointer, not legal advice — re-verify with `cargo about --threshold
0.1` if your situation needs it.


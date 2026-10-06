//! Lyrics from several sources. The listener picks one in Settings, or Auto,
//! which tries each source in turn and prefers lyrics timed to the song.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{credits::clean_title, spotify::LyricLine};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LyricsSource {
    #[default]
    Auto,
    /// Spotify's own lyrics, as its apps show them (mostly from Musixmatch).
    Spotify,
    /// LRCLIB, an open, community-made database of timed lyrics.
    Lrclib,
    /// NetEase Cloud Music, which is strong on Chinese, Japanese and Korean songs.
    NetEase,
}

impl LyricsSource {
    pub const ALL: [Self; 4] = [Self::Auto, Self::Spotify, Self::Lrclib, Self::NetEase];
    /// The order Auto tries the sources in.
    pub const AUTO_ORDER: [Self; 3] = [Self::Spotify, Self::Lrclib, Self::NetEase];

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Spotify => "Spotify",
            Self::Lrclib => "LRCLIB",
            Self::NetEase => "NetEase Cloud Music",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            Self::Auto => "Try Spotify, then LRCLIB, then NetEase, preferring lyrics timed to the song.",
            Self::Spotify => "The lyrics Spotify's own apps show, mostly from Musixmatch.",
            Self::Lrclib => "An open, community-made library of timed lyrics.",
            Self::NetEase => "Strong on Chinese, Japanese and Korean songs.",
        }
    }
}

/// Lyrics one source found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoundLyrics {
    pub lines: Vec<LyricLine>,
    pub synced: bool,
    /// Who provided them, for the "Lyrics from …" label.
    pub provider: String,
}

const NOT_FOUND: &str = "No lyrics were found for this track.";

/// Reads Spotify's lyrics response (`/color-lyrics/v2/track/{id}`).
pub fn parse_spotify(body: &[u8]) -> Option<FoundLyrics> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let lyrics = value.get("lyrics")?;
    let synced = lyrics.get("syncType").and_then(Value::as_str) == Some("LINE_SYNCED");
    let lines = lyrics
        .get("lines")?
        .as_array()?
        .iter()
        .filter_map(|line| {
            let text = line.get("words")?.as_str()?.trim();
            let timestamp_ms = line
                .get("startTimeMs")
                .and_then(|time| {
                    time.as_str()
                        .and_then(|time| time.parse().ok())
                        .or_else(|| time.as_u64().and_then(|time| u32::try_from(time).ok()))
                })
                .unwrap_or(0);
            // Spotify marks instrumental breaks with a note symbol.
            let text = if text == "♪" { "" } else { text };
            Some(LyricLine {
                timestamp_ms: if synced { timestamp_ms } else { 0 },
                text: text.to_owned(),
            })
        })
        .collect::<Vec<_>>();
    if lines.iter().all(|line| line.text.is_empty()) {
        return None;
    }
    let provider = lyrics
        .get("providerDisplayName")
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .map(|name| format!("Spotify · {name}"))
        .unwrap_or_else(|| "Spotify".to_owned());
    Some(FoundLyrics {
        lines,
        synced,
        provider,
    })
}

/// Reads an LRCLIB `/api/get` response.
pub fn parse_lrclib(value: &Value) -> Option<FoundLyrics> {
    if let Some(synced) = value["syncedLyrics"].as_str().filter(|text| !text.trim().is_empty()) {
        return Some(FoundLyrics {
            lines: crate::spotify::parse_lrc(synced),
            synced: true,
            provider: "LRCLIB".to_owned(),
        });
    }
    let plain = value["plainLyrics"].as_str().filter(|text| !text.trim().is_empty())?;
    Some(FoundLyrics {
        lines: plain_lines(plain),
        synced: false,
        provider: "LRCLIB".to_owned(),
    })
}

pub async fn lrclib(
    http: &reqwest::Client,
    artist: &str,
    title: &str,
    album: &str,
    duration_ms: u32,
) -> Result<FoundLyrics, String> {
    let params = [
        ("artist_name", artist.to_owned()),
        ("track_name", title.to_owned()),
        ("album_name", album.to_owned()),
        ("duration", (duration_ms / 1000).to_string()),
    ];
    let response = http
        .get("https://lrclib.net/api/get")
        .query(&params)
        .header(
            reqwest::header::USER_AGENT,
            concat!("Oynx/", env!("CARGO_PKG_VERSION"), " (desktop Spotify client)"),
        )
        .send()
        .await
        .map_err(|error| format!("LRCLIB request failed: {error}"))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(NOT_FOUND.to_owned());
    }
    if !response.status().is_success() {
        return Err(format!("LRCLIB returned HTTP {}", response.status()));
    }
    let value: Value = response
        .json()
        .await
        .map_err(|error| format!("Could not read the LRCLIB response: {error}"))?;
    parse_lrclib(&value).ok_or_else(|| NOT_FOUND.to_owned())
}

/// Picks the NetEase search result that is this song: the same title and
/// artist, and about the same length.
pub fn pick_netease_song(value: &Value, artist: &str, title: &str, duration_ms: u32) -> Option<u64> {
    let wanted_title = clean_title(title).to_lowercase();
    let wanted_artist = artist.split(',').next().unwrap_or_default().trim().to_lowercase();
    value
        .get("result")?
        .get("songs")?
        .as_array()?
        .iter()
        .filter_map(|song| {
            let id = song.get("id")?.as_u64()?;
            let name = clean_title(song.get("name")?.as_str()?).to_lowercase();
            if name != wanted_title {
                return None;
            }
            let artists = song
                .get("artists")
                .or_else(|| song.get("ar"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|artist| artist.get("name").and_then(Value::as_str))
                .map(str::to_lowercase)
                .collect::<Vec<_>>();
            if !wanted_artist.is_empty() && !artists.iter().any(|name| *name == wanted_artist) {
                return None;
            }
            let length = song
                .get("duration")
                .or_else(|| song.get("dt"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let distance = if length > 0 && duration_ms > 0 {
                length.abs_diff(u64::from(duration_ms))
            } else {
                5_000
            };
            (distance <= 8_000).then_some((id, distance))
        })
        .min_by_key(|(_, distance)| *distance)
        .map(|(id, _)| id)
}

/// Turns NetEase's LRC into lines, leaving out the credit lines it puts at
/// the start (such as "作词 : …" for the lyricist).
pub fn parse_netease_lrc(lrc: &str) -> Vec<LyricLine> {
    const CREDIT_PREFIXES: &[&str] = &[
        "作词", "作曲", "编曲", "制作", "监制", "混音", "母带", "和声", "吉他", "贝斯", "鼓", "录音",
        "出品", "发行", "词", "曲", "Lyricist", "Composer", "Arranger", "Producer",
    ];
    crate::spotify::parse_lrc(lrc)
        .into_iter()
        .filter(|line| {
            let text = line.text.trim();
            !CREDIT_PREFIXES.iter().any(|prefix| {
                text.strip_prefix(prefix)
                    .is_some_and(|rest| rest.trim_start().starts_with([':', '：']))
            })
        })
        .collect()
}

pub async fn netease(
    http: &reqwest::Client,
    artist: &str,
    title: &str,
    duration_ms: u32,
) -> Result<FoundLyrics, String> {
    let query = format!("{} {}", clean_title(title), artist.split(',').next().unwrap_or_default());
    let search: Value = netease_get(
        http,
        "https://music.163.com/api/search/get/web",
        &[("s", query.as_str()), ("type", "1"), ("limit", "10")],
    )
    .await?;
    let id = pick_netease_song(&search, artist, title, duration_ms).ok_or_else(|| NOT_FOUND.to_owned())?;
    let id = id.to_string();
    let lyrics: Value = netease_get(
        http,
        "https://music.163.com/api/song/lyric",
        &[("id", id.as_str()), ("lv", "1"), ("tv", "-1")],
    )
    .await?;
    let text = lyrics
        .get("lrc")
        .and_then(|lrc| lrc.get("lyric"))
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| NOT_FOUND.to_owned())?;
    let lines = parse_netease_lrc(text);
    let provider = "NetEase Cloud Music".to_owned();
    if lines.iter().any(|line| !line.text.trim().is_empty()) {
        return Ok(FoundLyrics {
            lines,
            synced: true,
            provider,
        });
    }
    // Lyrics without timestamps come back as plain text.
    Ok(FoundLyrics {
        lines: plain_lines(text),
        synced: false,
        provider,
    })
}

async fn netease_get(http: &reqwest::Client, url: &str, query: &[(&str, &str)]) -> Result<Value, String> {
    let response = http
        .get(url)
        .query(query)
        .header(reqwest::header::REFERER, "https://music.163.com/")
        .header(
            reqwest::header::USER_AGENT,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Oynx",
        )
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|error| format!("NetEase request failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("NetEase returned HTTP {}", response.status()));
    }
    response
        .json()
        .await
        .map_err(|error| format!("Could not read the NetEase response: {error}"))
}

fn plain_lines(text: &str) -> Vec<LyricLine> {
    text.lines()
        .map(|line| LyricLine {
            timestamp_ms: 0,
            text: line.to_owned(),
        })
        .collect()
}

/// Picks the result Auto shows: the first timed lyrics in source order, or
/// failing that the first untimed ones.
pub fn choose_auto(results: Vec<Result<FoundLyrics, String>>) -> Result<FoundLyrics, String> {
    let mut plain = None;
    let mut errors: Vec<String> = Vec::new();
    for result in results {
        match result {
            Ok(found) if found.synced => return Ok(found),
            Ok(found) => {
                plain.get_or_insert(found);
            }
            Err(error) => errors.push(error),
        }
    }
    if let Some(found) = plain {
        return Ok(found);
    }
    // Every source failing for another reason (such as no internet) is worth saying.
    match errors.iter().find(|error| *error != NOT_FOUND) {
        Some(error) if errors.iter().all(|other| other != NOT_FOUND) => Err(error.clone()),
        _ => Err(NOT_FOUND.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spotify_lyrics_are_read_with_their_timing_and_provider() {
        let body = serde_json::json!({
            "lyrics": {
                "syncType": "LINE_SYNCED",
                "providerDisplayName": "Musixmatch",
                "lines": [
                    {"startTimeMs": "1200", "words": "First line", "endTimeMs": "0"},
                    {"startTimeMs": "5000", "words": "♪", "endTimeMs": "0"},
                    {"startTimeMs": "9000", "words": "Second line", "endTimeMs": "0"}
                ]
            }
        });
        let found = parse_spotify(&serde_json::to_vec(&body).unwrap()).expect("lyrics");
        assert!(found.synced);
        assert_eq!(found.provider, "Spotify · Musixmatch");
        assert_eq!(found.lines[0].timestamp_ms, 1200);
        assert_eq!(found.lines[1].text, "");
        assert_eq!(found.lines[2].text, "Second line");
    }

    #[test]
    fn netease_search_needs_matching_title_artist_and_length() {
        let results = serde_json::json!({"result": {"songs": [
            {"id": 1, "name": "Song", "artists": [{"name": "Someone Else"}], "duration": 200_000},
            {"id": 2, "name": "Song", "artists": [{"name": "Artist"}], "duration": 260_000},
            {"id": 3, "name": "Song", "artists": [{"name": "artist"}], "duration": 201_000}
        ]}});
        assert_eq!(pick_netease_song(&results, "Artist, Guest", "Song - Remastered", 200_000), Some(3));
        assert_eq!(pick_netease_song(&results, "Nobody", "Song", 200_000), None);
    }

    #[test]
    fn netease_credit_lines_are_dropped() {
        let lines = parse_netease_lrc("[00:00.00]作词 : 某人\n[00:01.00]作曲：某人\n[00:12.50]Real lyric");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "Real lyric");
    }

    #[test]
    fn auto_prefers_timed_lyrics_over_earlier_plain_ones() {
        let plain = FoundLyrics {
            lines: plain_lines("words"),
            synced: false,
            provider: "Spotify".to_owned(),
        };
        let timed = FoundLyrics {
            lines: vec![LyricLine { timestamp_ms: 10, text: "words".into() }],
            synced: true,
            provider: "LRCLIB".to_owned(),
        };
        let chosen = choose_auto(vec![Ok(plain.clone()), Err("down".into()), Ok(timed.clone())]);
        assert_eq!(chosen, Ok(timed));
        assert_eq!(choose_auto(vec![Ok(plain.clone()), Err("down".into())]), Ok(plain));
        assert_eq!(choose_auto(vec![Err("down".into())]), Err("down".to_owned()));
        assert_eq!(
            choose_auto(vec![Err("down".into()), Err(NOT_FOUND.into())]),
            Err(NOT_FOUND.to_owned())
        );
    }
}

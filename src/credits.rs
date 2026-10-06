//! Song credits and connections, in the spirit of Spotify's SongDNA.
//!
//! SongDNA itself is not part of Spotify's public Web API, so Oynx builds its
//! own version: the roles Spotify's track metadata carries (featured artists,
//! remixers, composers) plus the open MusicBrainz database, which has writer,
//! producer and engineer credits, samples in both directions, and other
//! recordings of the same song. The recording is found by its ISRC, or by
//! title and artist when there is none.

use std::{
    sync::OnceLock,
    time::{Duration, Instant},
};

use serde_json::Value;
use tokio::sync::Mutex;

const MUSICBRAINZ_API: &str = "https://musicbrainz.org/ws/2";
const USER_AGENT: &str = concat!(
    "Oynx/",
    env!("CARGO_PKG_VERSION"),
    " ( https://github.com/tacobellerontop-sudo/Oynx )"
);
/// MusicBrainz allows one request per second per client.
const REQUEST_SPACING: Duration = Duration::from_millis(1_100);
const MAX_LINKS: usize = 12;

/// The order credit groups are shown in, with the label for each.
const ROLE_ORDER: &[&str] = &[
    "Featuring",
    "Written by",
    "Composed by",
    "Lyrics by",
    "Produced by",
    "Remixed by",
    "Arranged by",
    "Vocals",
    "Instruments",
    "Programming",
    "Performed by",
    "Conducted by",
    "Mixed by",
    "Mastered by",
    "Engineered by",
];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CreditGroup {
    pub role: String,
    pub names: Vec<String>,
}

/// Another song connected to this one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SongLink {
    pub title: String,
    pub artist: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SongCredits {
    pub credits: Vec<CreditGroup>,
    /// Songs this recording samples or interpolates.
    pub samples: Vec<SongLink>,
    /// Songs that sample this recording.
    pub sampled_by: Vec<SongLink>,
    /// Other recordings of the same song, such as covers.
    pub versions: Vec<SongLink>,
    /// The MusicBrainz recording page, when the song was found there.
    pub source_url: Option<String>,
}

impl SongCredits {
    pub fn is_empty(&self) -> bool {
        self.credits.is_empty()
            && self.samples.is_empty()
            && self.sampled_by.is_empty()
            && self.versions.is_empty()
    }

    fn add(&mut self, role: &str, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        match self.credits.iter_mut().find(|group| group.role == role) {
            Some(group) => {
                if !group.names.iter().any(|existing| existing.eq_ignore_ascii_case(name)) {
                    group.names.push(name.to_owned());
                }
            }
            None => self.credits.push(CreditGroup {
                role: role.to_owned(),
                names: vec![name.to_owned()],
            }),
        }
    }

    fn sort(&mut self) {
        let rank = |role: &str| {
            ROLE_ORDER
                .iter()
                .position(|known| *known == role)
                .unwrap_or(ROLE_ORDER.len())
        };
        self.credits.sort_by_key(|group| rank(&group.role));
    }
}

/// What Oynx knows about the track before asking MusicBrainz.
#[derive(Clone, Debug, Default)]
pub struct CreditsQuery {
    pub isrc: Option<String>,
    pub title: String,
    pub artist: String,
    pub duration_ms: u32,
    /// Credits from Spotify's own metadata, as (role label, name).
    pub spotify_credits: Vec<(String, String)>,
}

/// Looks the song up in MusicBrainz and merges its credits with Spotify's.
/// A song MusicBrainz does not know still returns Spotify's credits.
pub async fn lookup(query: &CreditsQuery) -> Result<SongCredits, String> {
    let http = client();
    let mut credits = SongCredits::default();
    for (role, name) in &query.spotify_credits {
        credits.add(role, name);
    }

    let recording_id = match find_recording(http, query).await {
        Ok(id) => id,
        Err(error) => {
            if credits.is_empty() {
                return Err(error);
            }
            log::debug!("MusicBrainz lookup failed: {error}");
            None
        }
    };
    if let Some(recording_id) = recording_id {
        let url = format!("{MUSICBRAINZ_API}/recording/{recording_id}");
        let full = get_json(
            http,
            &url,
            &[(
                "inc",
                "artist-credits+artist-rels+recording-rels+release-rels+work-rels+work-level-rels",
            )],
        )
        .await;
        // A much-covered song lists every recording of it, which can be too
        // large to fetch in time; credits without the connections still help.
        let recording = match full {
            Ok(recording) => recording,
            Err(error) => {
                log::debug!("Full MusicBrainz lookup failed, retrying without connections: {error}");
                get_json(
                    http,
                    &url,
                    &[("inc", "artist-credits+artist-rels+work-rels+work-level-rels")],
                )
                .await?
            }
        };
        merge_recording(&mut credits, &recording);
        credits.source_url = Some(format!("https://musicbrainz.org/recording/{recording_id}"));
    }
    credits.sort();
    Ok(credits)
}

async fn find_recording(http: &reqwest::Client, query: &CreditsQuery) -> Result<Option<String>, String> {
    if let Some(isrc) = query.isrc.as_deref().filter(|isrc| !isrc.trim().is_empty()) {
        let url = format!("{MUSICBRAINZ_API}/isrc/{}", isrc.trim().to_ascii_uppercase());
        match get_json(http, &url, &[]).await {
            Ok(value) => {
                if let Some(id) = best_recording(&value, query, false) {
                    return Ok(Some(id));
                }
            }
            // An ISRC MusicBrainz has never seen is a 404; fall back to a search.
            Err(error) => log::debug!("MusicBrainz ISRC lookup failed: {error}"),
        }
    }

    let title = clean_title(&query.title);
    let artist = query.artist.split(',').next().unwrap_or_default().trim();
    if title.is_empty() || artist.is_empty() {
        return Ok(None);
    }
    let search = format!(
        "recording:\"{}\" AND artist:\"{}\"",
        escape_lucene(&title),
        escape_lucene(artist)
    );
    let value = get_json(
        http,
        &format!("{MUSICBRAINZ_API}/recording"),
        &[("query", search.as_str()), ("limit", "10")],
    )
    .await?;
    Ok(best_recording(&value, query, true))
}

/// Picks the recording whose length is closest to the Spotify track's. Search
/// results must also be confident matches with the same title.
fn best_recording(value: &Value, query: &CreditsQuery, from_search: bool) -> Option<String> {
    let wanted_title = clean_title(&query.title).to_lowercase();
    value
        .get("recordings")?
        .as_array()?
        .iter()
        .filter(|recording| {
            if !from_search {
                return true;
            }
            let score = recording.get("score").and_then(Value::as_u64).unwrap_or(0);
            let title = recording
                .get("title")
                .and_then(Value::as_str)
                .map(|title| clean_title(title).to_lowercase())
                .unwrap_or_default();
            score >= 80 && title == wanted_title
        })
        .filter_map(|recording| {
            let id = recording.get("id")?.as_str()?.to_owned();
            let length = recording.get("length").and_then(Value::as_u64);
            let distance = match (length, query.duration_ms) {
                (Some(length), wanted) if wanted > 0 => length.abs_diff(u64::from(wanted)),
                _ => 30_000,
            };
            Some((id, distance))
        })
        .filter(|(_, distance)| !from_search || *distance <= 15_000)
        .min_by_key(|(_, distance)| *distance)
        .map(|(id, _)| id)
}

/// Reads credits and connections out of a MusicBrainz recording lookup.
pub fn merge_recording(credits: &mut SongCredits, recording: &Value) {
    let own_id = recording.get("id").and_then(Value::as_str).unwrap_or_default();
    for relation in relations(recording) {
        match target_type(relation) {
            "artist" => {
                if let Some((role, name)) = artist_credit(relation) {
                    credits.add(role, &name);
                }
            }
            "recording" | "release" if relation_type(relation) == "samples material" => {
                let Some(link) = song_link(relation) else {
                    continue;
                };
                let list = if direction(relation) == "backward" {
                    &mut credits.sampled_by
                } else {
                    &mut credits.samples
                };
                push_link(list, link);
            }
            "work" if relation_type(relation) == "performance" => {
                let Some(work) = relation.get("work") else {
                    continue;
                };
                for work_relation in relations(work) {
                    match target_type(work_relation) {
                        "artist" => {
                            if let Some((role, name)) = artist_credit(work_relation) {
                                credits.add(role, &name);
                            }
                        }
                        "recording" if relation_type(work_relation) == "performance" => {
                            let other = work_relation.get("recording");
                            let other_id = other
                                .and_then(|recording| recording.get("id"))
                                .and_then(Value::as_str)
                                .unwrap_or_default();
                            if other_id == own_id {
                                continue;
                            }
                            if let Some(link) = song_link(work_relation) {
                                push_link(&mut credits.versions, link);
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

fn relations(value: &Value) -> impl Iterator<Item = &Value> {
    value
        .get("relations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

fn target_type(relation: &Value) -> &str {
    relation.get("target-type").and_then(Value::as_str).unwrap_or_default()
}

fn relation_type(relation: &Value) -> &str {
    relation.get("type").and_then(Value::as_str).unwrap_or_default()
}

fn direction(relation: &Value) -> &str {
    relation.get("direction").and_then(Value::as_str).unwrap_or_default()
}

fn attributes(relation: &Value) -> Vec<&str> {
    relation
        .get("attributes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

fn artist_credit(relation: &Value) -> Option<(&'static str, String)> {
    let artist = relation.get("artist")?;
    let name = relation
        .get("target-credit")
        .and_then(Value::as_str)
        .filter(|credit| !credit.trim().is_empty())
        .or_else(|| artist.get("name").and_then(Value::as_str))?
        .to_owned();
    let role = match relation_type(relation) {
        "writer" => "Written by",
        "composer" => "Composed by",
        "lyricist" | "librettist" => "Lyrics by",
        "producer" => "Produced by",
        "remixer" => "Remixed by",
        "arranger" | "instrument arranger" | "vocal arranger" | "orchestrator" => "Arranged by",
        "vocal" => "Vocals",
        "instrument" => "Instruments",
        "programming" => "Programming",
        "performer" | "performing orchestra" => "Performed by",
        "conductor" => "Conducted by",
        "mix" => "Mixed by",
        "mastering" => "Mastered by",
        "recording" | "engineer" | "sound" | "audio" => "Engineered by",
        _ => return None,
    };
    // Say which instrument, or which kind of vocals, when MusicBrainz knows.
    let detail = attributes(relation)
        .into_iter()
        .filter(|attribute| !matches!(*attribute, "additional" | "co" | "guest" | "solo" | "executive"))
        .collect::<Vec<_>>();
    let name = if matches!(role, "Instruments" | "Vocals") && !detail.is_empty() {
        format!("{name} ({})", detail.join(", "))
    } else {
        name
    };
    Some((role, name))
}

fn song_link(relation: &Value) -> Option<SongLink> {
    let target = relation
        .get("recording")
        .or_else(|| relation.get("release"))?;
    let title = target.get("title").and_then(Value::as_str)?.trim().to_owned();
    if title.is_empty() {
        return None;
    }
    Some(SongLink {
        title,
        artist: artist_credit_text(target),
    })
}

fn artist_credit_text(value: &Value) -> String {
    value
        .get("artist-credit")
        .and_then(Value::as_array)
        .map(|credits| {
            credits
                .iter()
                .map(|credit| {
                    let name = credit
                        .get("name")
                        .and_then(Value::as_str)
                        .or_else(|| {
                            credit
                                .get("artist")
                                .and_then(|artist| artist.get("name"))
                                .and_then(Value::as_str)
                        })
                        .unwrap_or_default();
                    let join = credit.get("joinphrase").and_then(Value::as_str).unwrap_or_default();
                    format!("{name}{join}")
                })
                .collect::<String>()
                .trim()
                .to_owned()
        })
        .unwrap_or_default()
}

fn push_link(list: &mut Vec<SongLink>, link: SongLink) {
    if list.len() < MAX_LINKS
        && !list.iter().any(|existing| {
            existing.title.eq_ignore_ascii_case(&link.title)
                && existing.artist.eq_ignore_ascii_case(&link.artist)
        })
    {
        list.push(link);
    }
}

/// Drops the parts of a Spotify title that MusicBrainz does not use, such as
/// "- Remastered 2011" or "(feat. Someone)".
pub fn clean_title(title: &str) -> String {
    let mut title = title.trim();
    if let Some(index) = title.find(" - ") {
        title = &title[..index];
    }
    let mut cleaned = title.to_owned();
    for marker in ["(feat", "(with ", "[feat", "(ft."] {
        if let Some(index) = cleaned.to_lowercase().find(marker) {
            cleaned.truncate(index);
        }
    }
    cleaned.trim().to_owned()
}

fn escape_lucene(text: &str) -> String {
    text.chars()
        .filter(|character| !matches!(character, '"' | '\\'))
        .collect()
}

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent(USER_AGENT)
            .build()
            .unwrap_or_default()
    })
}

fn last_request() -> &'static Mutex<Option<Instant>> {
    static LAST: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    LAST.get_or_init(|| Mutex::new(None))
}

async fn get_json(http: &reqwest::Client, url: &str, query: &[(&str, &str)]) -> Result<Value, String> {
    for attempt in 0..2 {
        {
            let mut last = last_request().lock().await;
            if let Some(previous) = *last {
                let wait = REQUEST_SPACING.saturating_sub(previous.elapsed());
                if !wait.is_zero() {
                    tokio::time::sleep(wait).await;
                }
            }
            *last = Some(Instant::now());
        }
        let response = http
            .get(url)
            .query(&[("fmt", "json")])
            .query(query)
            .send()
            .await
            .map_err(|error| format!("MusicBrainz request failed: {error}"))?;
        let status = response.status();
        // MusicBrainz answers 503 when it is busy or the rate limit was hit.
        if status == reqwest::StatusCode::SERVICE_UNAVAILABLE && attempt == 0 {
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }
        if !status.is_success() {
            return Err(format!("MusicBrainz returned HTTP {status}"));
        }
        return response
            .json()
            .await
            .map_err(|error| format!("Could not read the MusicBrainz response: {error}"));
    }
    Err("MusicBrainz is busy; try again in a moment.".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_relations_become_credits_samples_and_versions() {
        let recording = serde_json::json!({
            "id": "rec-1",
            "title": "Song",
            "relations": [
                {"type": "producer", "target-type": "artist", "direction": "backward",
                 "attributes": [], "artist": {"name": "Max Martin"}},
                {"type": "instrument", "target-type": "artist", "direction": "backward",
                 "attributes": ["guitar"], "artist": {"name": "Jane Doe"}},
                {"type": "mastering", "target-type": "artist", "direction": "backward",
                 "attributes": [], "artist": {"name": "Engineer"}, "target-credit": "E. Credit"},
                {"type": "samples material", "target-type": "recording", "direction": "forward",
                 "recording": {"id": "rec-2", "title": "Old Song",
                   "artist-credit": [{"name": "A", "joinphrase": " & "}, {"name": "B", "joinphrase": ""}]}},
                {"type": "samples material", "target-type": "recording", "direction": "backward",
                 "recording": {"id": "rec-3", "title": "New Song"}},
                {"type": "performance", "target-type": "work", "direction": "forward",
                 "work": {"title": "Song", "relations": [
                    {"type": "composer", "target-type": "artist", "direction": "backward",
                     "artist": {"name": "Writer One"}},
                    {"type": "lyricist", "target-type": "artist", "direction": "backward",
                     "artist": {"name": "Writer Two"}},
                    {"type": "performance", "target-type": "recording", "direction": "backward",
                     "attributes": ["cover"], "recording": {"id": "rec-1", "title": "Song"}},
                    {"type": "performance", "target-type": "recording", "direction": "backward",
                     "attributes": ["cover"], "recording": {"id": "rec-4", "title": "Song (Cover)"}}
                 ]}}
            ]
        });
        let mut credits = SongCredits::default();
        credits.add("Featuring", "Guest");
        merge_recording(&mut credits, &recording);
        credits.sort();

        let roles = credits.credits.iter().map(|group| group.role.as_str()).collect::<Vec<_>>();
        assert_eq!(
            roles,
            ["Featuring", "Composed by", "Lyrics by", "Produced by", "Instruments", "Mastered by"]
        );
        assert_eq!(credits.credits[4].names, ["Jane Doe (guitar)"]);
        assert_eq!(credits.credits[5].names, ["E. Credit"]);
        assert_eq!(
            credits.samples,
            [SongLink { title: "Old Song".into(), artist: "A & B".into() }]
        );
        assert_eq!(credits.sampled_by[0].title, "New Song");
        // The recording itself is not listed as another version of the song.
        assert_eq!(credits.versions.len(), 1);
        assert_eq!(credits.versions[0].title, "Song (Cover)");
    }

    #[test]
    fn spotify_titles_are_cleaned_for_searching() {
        assert_eq!(clean_title("Come Together - Remastered 2009"), "Come Together");
        assert_eq!(clean_title("Stay (with Justin Bieber)"), "Stay");
        assert_eq!(clean_title("Peaches (feat. Daniel Caesar)"), "Peaches");
        assert_eq!(clean_title("Plain"), "Plain");
    }

    #[test]
    fn search_matches_need_the_same_title_and_a_close_length() {
        let query = CreditsQuery {
            title: "Song - Remastered".into(),
            artist: "Artist".into(),
            duration_ms: 200_000,
            ..CreditsQuery::default()
        };
        let results = serde_json::json!({"recordings": [
            {"id": "far", "score": 100, "title": "Song", "length": 260_000},
            {"id": "other", "score": 100, "title": "Different", "length": 200_000},
            {"id": "close", "score": 95, "title": "Song", "length": 201_000}
        ]});
        assert_eq!(best_recording(&results, &query, true).as_deref(), Some("close"));
        let none = serde_json::json!({"recordings": [
            {"id": "far", "score": 100, "title": "Song", "length": 260_000}
        ]});
        assert_eq!(best_recording(&none, &query, true), None);
        // ISRC matches are trusted even without a title match.
        assert_eq!(best_recording(&none, &query, false).as_deref(), Some("far"));
    }
}

use std::{
    collections::hash_map::DefaultHasher,
    future::Future,
    hash::{Hash, Hasher},
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use librespot::{
    metadata::{
        Album as AlbumMetadata, Artist as ArtistMetadata, Metadata,
        Track as TrackMetadata, artist::ArtistRole,
    },
    core::{
        SpotifyUri, authentication::Credentials, cache::Cache, config::SessionConfig,
        session::Session, spotify_id::SpotifyId,
    },
    oauth::{OAuthClientBuilder, OAuthToken},
    playback::{
        audio_backend,
        config::{AudioFormat, PlayerConfig},
        mixer::{self, Mixer, MixerConfig},
        player::{Player, PlayerEvent},
    },
};
use tokio::sync::{Mutex as TokioMutex, Semaphore, mpsc as tokio_mpsc};

use crate::{
    audio::AudioTaps,
    credits::{self, CreditsQuery, SongCredits},
    lyrics::{self, FoundLyrics, LyricsSource},
};

const DEFAULT_CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";
const DEFAULT_REDIRECT_URI: &str = "http://127.0.0.1:8898/login";
const DEFAULT_WEB_API_REDIRECT_URI: &str = "http://127.0.0.1:8989/login";
const STREAMING_SCOPES: &[&str] = &["streaming"];
const WEB_API_SCOPES: &[&str] = &[
    "user-library-read",
    "user-library-modify",
    "playlist-read-private",
    "playlist-read-collaborative",
    "user-top-read",
];
/// Permissions newer features use. They are requested at sign-in but not
/// required to restore a saved sign-in, so existing users are not signed out;
/// those features explain how to grant them when Spotify refuses a request.
const OPTIONAL_WEB_API_SCOPES: &[&str] = &[
    "user-read-recently-played",
    "playlist-modify-public",
    "playlist-modify-private",
];
const MISSING_PERMISSION: &str = "Spotify needs a newer permission for this. Sign out from Settings and sign in again, then try once more.";
const AUDIO_CACHE_LIMIT: u64 = 1_000_000_000;
const MAX_ARTWORK_BYTES: usize = 8 * 1024 * 1024;
const MAX_API_PAGES: usize = 100;
const MAX_LIKED_SONG_PAGES: usize = MAX_API_PAGES;

fn requested_web_api_scopes() -> Vec<&'static str> {
    WEB_API_SCOPES
        .iter()
        .chain(OPTIONAL_WEB_API_SCOPES)
        .copied()
        .collect()
}

fn combined_oauth_scopes() -> Vec<&'static str> {
    STREAMING_SCOPES
        .iter()
        .copied()
        .chain(requested_web_api_scopes())
        .collect()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpotifyConfig {
    pub client_id: String,
    pub redirect_uri: String,
    #[serde(default)]
    pub web_api_client_id: String,
    #[serde(default)]
    pub web_api_redirect_uri: String,
}

impl Default for SpotifyConfig {
    fn default() -> Self {
        Self {
            client_id: DEFAULT_CLIENT_ID.to_owned(),
            redirect_uri: DEFAULT_REDIRECT_URI.to_owned(),
            web_api_client_id: String::new(),
            web_api_redirect_uri: String::new(),
        }
    }
}

impl SpotifyConfig {
    pub fn load() -> Self {
        let path = Self::path();
        let config: Self = std::fs::read_to_string(path)
            .ok()
            .and_then(|contents| serde_json::from_str(&contents).ok())
            .unwrap_or_default();
        let client_id = config.client_id.trim();
        let redirect_uri = config.redirect_uri.trim();
        let web_api_client_id = config.web_api_client_id.trim();
        let web_api_redirect_uri = config.web_api_redirect_uri.trim();
        Self {
            client_id: if client_id.is_empty() {
                DEFAULT_CLIENT_ID.to_owned()
            } else {
                client_id.to_owned()
            },
            redirect_uri: if redirect_uri.is_empty() {
                DEFAULT_REDIRECT_URI.to_owned()
            } else {
                redirect_uri.to_owned()
            },
            web_api_client_id: web_api_client_id.to_owned(),
            web_api_redirect_uri: web_api_redirect_uri.to_owned(),
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("Could not create Oynx config directory: {error}"))?;
        }
        let contents = serde_json::to_string_pretty(self)
            .map_err(|error| format!("Could not serialize Oynx settings: {error}"))?;
        std::fs::write(path, contents)
            .map_err(|error| format!("Could not save Oynx settings: {error}"))
    }

    pub fn path() -> PathBuf {
        dirs::config_dir()
            .or_else(dirs::data_local_dir)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Oynx")
            .join("config.json")
    }

    pub fn cache_path() -> PathBuf {
        dirs::data_local_dir()
            .or_else(dirs::data_dir)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Oynx")
            .join("librespot")
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredOAuthToken {
    client_id: String,
    redirect_uri: String,
    access_token: String,
    refresh_token: String,
    expires_at: u64,
    scopes: Vec<String>,
}

impl StoredOAuthToken {
    fn from_token(client_id: &str, redirect_uri: &str, token: &OAuthToken) -> Self {
        let remaining = token
            .expires_at
            .saturating_duration_since(Instant::now())
            .as_secs();
        Self {
            client_id: client_id.to_owned(),
            redirect_uri: redirect_uri.to_owned(),
            access_token: token.access_token.clone(),
            refresh_token: token.refresh_token.clone(),
            expires_at: now_unix().saturating_add(remaining),
            scopes: token.scopes.clone(),
        }
    }

    fn into_token(self) -> Option<OAuthToken> {
        let remaining = self
            .expires_at
            .saturating_sub(now_unix())
            .min(30 * 24 * 60 * 60);
        if remaining == 0 || self.access_token.is_empty() {
            return None;
        }
        Some(OAuthToken {
            access_token: self.access_token,
            refresh_token: self.refresh_token,
            expires_at: Instant::now() + Duration::from_secs(remaining),
            token_type: "Bearer".to_owned(),
            scopes: self.scopes,
        })
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn web_api_token_path() -> PathBuf {
    SpotifyConfig::cache_path().join("web-api-token.json")
}

fn web_api_cache_path() -> PathBuf {
    SpotifyConfig::cache_path().join("web-api-cache")
}

fn artwork_cache_path() -> PathBuf {
    SpotifyConfig::cache_path().join("artwork")
}

fn web_api_cache_key(namespace: &str, url: &str, query: &[(String, String)]) -> String {
    let mut key = format!("{namespace}|{url}");
    for (name, value) in query {
        key.push('|');
        key.push_str(name);
        key.push('=');
        key.push_str(value);
    }
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    format!("{:016x}.json", hasher.finish())
}

fn should_cache(url: &str) -> bool {
    url.contains("/me/playlists")
        || url.contains("/me/top/")
        || url.contains("/me/tracks")
        || url.contains("/recommendations")
        || url.contains("/playlists/")
        || url.contains("/albums/")
        || url.contains("/artists/")
}

fn read_cached_json<T: DeserializeOwned>(key: &str, max_age: Option<Duration>) -> Option<T> {
    let path = web_api_cache_path().join(key);
    let metadata = std::fs::metadata(&path).ok()?;
    if let Some(max_age) = max_age {
        let age = metadata.modified().ok()?.elapsed().ok()?;
        if age > max_age {
            return None;
        }
    }
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn temporary_path(path: &std::path::Path) -> std::path::PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    path.with_extension(format!("tmp-{}-{stamp}", std::process::id()))
}

fn commit_temporary_file(
    temporary: &std::path::Path,
    destination: &std::path::Path,
) -> std::io::Result<()> {
    match std::fs::rename(temporary, destination) {
        Ok(()) => Ok(()),
        Err(error) if destination.exists() => {
            std::fs::remove_file(destination)?;
            std::fs::rename(temporary, destination).map_err(|_| error)
        }
        Err(error) => Err(error),
    }
}

fn write_cached_json<T: Serialize>(key: &str, value: &T) {
    let dir = web_api_cache_path();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let Ok(contents) = serde_json::to_vec(value) else {
        return;
    };
    let path = dir.join(key);
    let temporary = temporary_path(&path);
    if std::fs::write(&temporary, contents).is_ok()
        && commit_temporary_file(&temporary, &path).is_err()
    {
        let _ = std::fs::remove_file(temporary);
    }
}

fn save_oauth_token(client_id: &str, redirect_uri: &str, token: &OAuthToken) -> Result<(), String> {
    let path = web_api_token_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("Could not create the Web API token directory: {error}"))?;
    }
    let stored = StoredOAuthToken::from_token(client_id, redirect_uri, token);
    let contents = serde_json::to_vec_pretty(&stored)
        .map_err(|error| format!("Could not serialize the Web API token: {error}"))?;
    let temporary = temporary_path(&path);
    std::fs::write(&temporary, contents)
        .map_err(|error| format!("Could not save the Web API token: {error}"))?;
    commit_temporary_file(&temporary, &path).map_err(|error| {
        let _ = std::fs::remove_file(temporary);
        format!("Could not replace the Web API token: {error}")
    })
}

async fn refresh_oauth_token(
    client_id: String,
    redirect_uri: String,
    refresh_token: String,
    scopes: Vec<String>,
) -> Result<OAuthToken, String> {
    tokio::task::spawn_blocking(move || {
        let scope_refs = scopes.iter().map(String::as_str).collect::<Vec<_>>();
        let client = OAuthClientBuilder::new(&client_id, &redirect_uri, scope_refs)
            .build()
            .map_err(|error| format!("Could not prepare Spotify token refresh: {error}"))?;
        client
            .refresh_token(&refresh_token)
            .map_err(|error| format!("Spotify token refresh failed: {error}"))
    })
    .await
    .map_err(|error| format!("Spotify token refresh worker failed: {error}"))?
}

fn token_from_cached_credentials(credentials: &Credentials) -> Option<OAuthToken> {
    let serialized = serde_json::to_value(credentials).ok()?;
    if serialized.get("auth_type").and_then(|value| value.as_u64()) != Some(3) {
        return None;
    }
    let access_token = String::from_utf8(credentials.auth_data.clone()).ok()?;
    if access_token.is_empty() {
        return None;
    }
    Some(OAuthToken {
        access_token,
        refresh_token: String::new(),
        expires_at: Instant::now() + Duration::from_secs(15 * 60),
        token_type: "Bearer".to_owned(),
        scopes: combined_oauth_scopes()
            .into_iter()
            .map(str::to_owned)
            .collect(),
    })
}

fn has_scopes(granted: &[String], required: &[&str]) -> bool {
    required
        .iter()
        .all(|scope| granted.iter().any(|value| value == scope))
}

async fn load_saved_token(client_id: &str, redirect_uri: &str) -> Option<OAuthToken> {
    let contents = std::fs::read_to_string(web_api_token_path()).ok()?;
    let stored: StoredOAuthToken = serde_json::from_str(&contents).ok()?;
    if stored.client_id != client_id
        || stored.redirect_uri != redirect_uri
        || !has_scopes(&stored.scopes, WEB_API_SCOPES)
    {
        return None;
    }

    if let Some(token) = stored.clone().into_token() {
        return Some(token);
    }
    if stored.refresh_token.is_empty() {
        return None;
    }

    match refresh_oauth_token(
        client_id.to_owned(),
        redirect_uri.to_owned(),
        stored.refresh_token,
        requested_web_api_scopes()
            .into_iter()
            .map(str::to_owned)
            .collect(),
    )
    .await
    {
        Ok(token) => {
            let _ = save_oauth_token(client_id, redirect_uri, &token);
            Some(token)
        }
        Err(error) => {
            log::debug!("Could not restore the Spotify Web API token: {error}");
            None
        }
    }
}

// Artwork decoding happens on the Spotify worker rather than egui's UI thread.
struct DecodedArtwork {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

// The Web API layer follows the separate-client, cached-token approach used by
// the MIT-licensed MYX player, adapted to Oynx's eframe/Librespot worker.
struct WebApiClient {
    http: reqwest::Client,
    client_id: String,
    redirect_uri: String,
    cache_namespace: String,
    scopes: Vec<String>,
    token: TokioMutex<Option<OAuthToken>>,
}

impl WebApiClient {
    fn new(
        http: reqwest::Client,
        client_id: String,
        redirect_uri: String,
        cache_namespace: String,
        scopes: Vec<String>,
        token: Option<OAuthToken>,
    ) -> Self {
        Self {
            http,
            client_id,
            redirect_uri,
            cache_namespace,
            scopes,
            token: TokioMutex::new(token),
        }
    }

    async fn expire_token(&self) {
        if let Some(token) = self.token.lock().await.as_mut() {
            token.expires_at = Instant::now();
        }
    }

    async fn access_token(&self) -> Result<String, String> {
        let mut token_slot = self.token.lock().await;
        if let Some(token) = token_slot.as_ref()
            && token.expires_at > Instant::now() + Duration::from_secs(30)
        {
            return Ok(token.access_token.clone());
        }

        let Some(refresh_token) = token_slot
            .as_ref()
            .map(|token| token.refresh_token.clone())
            .filter(|refresh_token| !refresh_token.is_empty())
        else {
            return Err(
                "Spotify Web API authorization is missing. Sign out and sign in again to refresh catalog access."
                    .to_owned(),
            );
        };
        let previous_scopes = token_slot
            .as_ref()
            .map(|token| token.scopes.clone())
            .unwrap_or_else(|| self.scopes.clone());
        let refreshed = refresh_oauth_token(
            self.client_id.clone(),
            self.redirect_uri.clone(),
            refresh_token.clone(),
            previous_scopes.clone(),
        )
        .await?;
        let token = OAuthToken {
            access_token: refreshed.access_token,
            refresh_token: if refreshed.refresh_token.is_empty() {
                refresh_token
            } else {
                refreshed.refresh_token
            },
            expires_at: refreshed.expires_at,
            token_type: if refreshed.token_type.is_empty() {
                "Bearer".to_owned()
            } else {
                refreshed.token_type
            },
            scopes: if refreshed.scopes.is_empty() {
                previous_scopes
            } else {
                refreshed.scopes
            },
        };
        let _ = save_oauth_token(&self.client_id, &self.redirect_uri, &token);
        let access_token = token.access_token.clone();
        *token_slot = Some(token);
        Ok(access_token)
    }

    async fn get_json<T: DeserializeOwned>(
        &self,
        url: &str,
        query: &[(String, String)],
    ) -> Result<T, String> {
        let cacheable = should_cache(url);
        let cache_key = web_api_cache_key(&self.cache_namespace, url, query);
        if cacheable
            && let Some(value) = read_cached_json(&cache_key, Some(Duration::from_secs(5 * 60)))
        {
            return Ok(value);
        }

        let mut refreshed_after_unauthorized = false;
        let mut access_token = match self.access_token().await {
            Ok(token) => token,
            Err(error) => {
                if cacheable && let Some(value) = read_cached_json(&cache_key, None) {
                    return Ok(value);
                }
                return Err(error);
            }
        };

        for attempt in 0..3 {
            let response = match self
                .http
                .get(url)
                .query(query)
                .bearer_auth(access_token.clone())
                .send()
                .await
            {
                Ok(response) => response,
                Err(error) => {
                    if cacheable && let Some(value) = read_cached_json(&cache_key, None) {
                        return Ok(value);
                    }
                    return Err(format!("Spotify Web API request failed: {error}"));
                }
            };
            let status = response.status();
            if status == reqwest::StatusCode::UNAUTHORIZED && !refreshed_after_unauthorized {
                // The token can be rejected before its recorded expiry (revoked, or a
                // skewed clock), so force one refresh before giving up.
                refreshed_after_unauthorized = true;
                self.expire_token().await;
                access_token = self.access_token().await?;
                continue;
            }
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok());
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                if attempt < 2 && retry_after.unwrap_or(3) <= 5 {
                    tokio::time::sleep(Duration::from_secs(
                        retry_after.unwrap_or(3).saturating_add(1),
                    ))
                    .await;
                    continue;
                }
                if cacheable && let Some(value) = read_cached_json(&cache_key, None) {
                    return Ok(value);
                }
                return Err(format!(
                    "Spotify Web API rate limit exceeded (HTTP 429). Configure a separate Web API client ID in Settings; Retry-After was {retry_after:?}."
                ));
            }
            let body = response.text().await.unwrap_or_default();
            if !status.is_success() {
                let detail = body.trim().chars().take(300).collect::<String>();
                if status == reqwest::StatusCode::UNAUTHORIZED
                    || status == reqwest::StatusCode::FORBIDDEN
                {
                    return Err(format!(
                        "Spotify rejected the Web API request ({status}). Sign out and sign in again to refresh Oynx's Spotify permissions. {detail}"
                    ));
                }
                return Err(if detail.is_empty() {
                    format!("Spotify Web API returned HTTP {status}.")
                } else {
                    format!("Spotify Web API returned HTTP {status}: {detail}")
                });
            }
            let raw: serde_json::Value = serde_json::from_str(&body)
                .map_err(|error| format!("Could not read Spotify Web API response: {error}"))?;
            let value: T = serde_json::from_value(raw.clone())
                .map_err(|error| format!("Could not decode Spotify Web API response: {error}"))?;
            if cacheable {
                write_cached_json(&cache_key, &raw);
            }
            return Ok(value);
        }
        Err("Spotify Web API rate limit retry failed.".to_owned())
    }

    async fn set_track_saved(&self, track_id: &str, saved: bool) -> Result<(), String> {
        let access_token = self.access_token().await?;
        let params = [("uris".to_owned(), format!("spotify:track:{track_id}"))];
        let url = "https://api.spotify.com/v1/me/library";
        for attempt in 0..2 {
            let request = if saved {
                self.http.put(url)
            } else {
                self.http.delete(url)
            };
            let response = match request
                .query(&params)
                .bearer_auth(access_token.clone())
                // Spotify rejects bodyless PUT/DELETE requests with 411 Length Required.
                .header(reqwest::header::CONTENT_LENGTH, "0")
                .body(Vec::<u8>::new())
                .send()
                .await
            {
                Ok(response) => response,
                Err(error) => return Err(format!("Spotify library update failed: {error}")),
            };
            let status = response.status();
            if status.is_success() {
                let _ = std::fs::remove_dir_all(web_api_cache_path());
                return Ok(());
            }
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS && attempt == 0 {
                let retry_after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or(1);
                if retry_after <= 5 {
                    tokio::time::sleep(Duration::from_secs(retry_after + 1)).await;
                    continue;
                }
            }
            let body = response.text().await.unwrap_or_default();
            let detail = body.trim().chars().take(300).collect::<String>();
            return Err(if detail.is_empty() {
                format!("Spotify library update returned HTTP {status}.")
            } else {
                format!("Spotify library update returned HTTP {status}: {detail}")
            });
        }
        Err("Spotify library update rate limit retry failed.".to_owned())
    }

    /// Sends a JSON body to a Web API endpoint that changes something, such as
    /// adding a song to a playlist, and clears the response cache afterwards.
    async fn send_json(
        &self,
        method: reqwest::Method,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<(), String> {
        let mut access_token = self.access_token().await?;
        let mut refreshed = false;
        for attempt in 0..3 {
            let response = self
                .http
                .request(method.clone(), url)
                .bearer_auth(access_token.clone())
                .json(body)
                .send()
                .await
                .map_err(|error| format!("Spotify request failed: {error}"))?;
            let status = response.status();
            if status.is_success() {
                let _ = std::fs::remove_dir_all(web_api_cache_path());
                return Ok(());
            }
            if status == reqwest::StatusCode::UNAUTHORIZED && !refreshed {
                refreshed = true;
                self.expire_token().await;
                access_token = self.access_token().await?;
                continue;
            }
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS && attempt < 2 {
                let retry_after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or(1);
                if retry_after <= 5 {
                    tokio::time::sleep(Duration::from_secs(retry_after + 1)).await;
                    continue;
                }
            }
            if status == reqwest::StatusCode::FORBIDDEN {
                return Err(MISSING_PERMISSION.to_owned());
            }
            let detail = response.text().await.unwrap_or_default();
            let detail = detail.trim().chars().take(300).collect::<String>();
            return Err(if detail.is_empty() {
                format!("Spotify returned HTTP {status}.")
            } else {
                format!("Spotify returned HTTP {status}: {detail}")
            });
        }
        Err("Spotify rate limit retry failed.".to_owned())
    }

    async fn fetch_artwork(&self, url: &str) -> Result<DecodedArtwork, String> {
        let key = web_api_cache_key("artwork", url, &[]);
        let path = artwork_cache_path().join(key);
        let bytes = if let Ok(bytes) = std::fs::read(&path) {
            if bytes.is_empty() {
                return Err("Cached artwork response was empty.".to_owned());
            }
            bytes
        } else {
            let response = self
                .http
                .get(url)
                .send()
                .await
                .map_err(|error| format!("Artwork request failed: {error}"))?;
            if !response.status().is_success() {
                return Err(format!(
                    "Artwork request returned HTTP {}",
                    response.status()
                ));
            }
            let bytes = response
                .bytes()
                .await
                .map_err(|error| format!("Could not read artwork response: {error}"))?;
            if bytes.is_empty() || bytes.len() > MAX_ARTWORK_BYTES {
                return Err("Artwork response was empty or too large.".to_owned());
            }
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let temporary = temporary_path(&path);
            if std::fs::write(&temporary, &bytes).is_ok() {
                let _ = commit_temporary_file(&temporary, &path);
            }
            bytes.to_vec()
        };

        let decoded = image::load_from_memory(&bytes)
            .map_err(|error| format!("Could not decode artwork response: {error}"))?
            .thumbnail(512, 512);
        let rgba = decoded.to_rgba8();
        let expected_size = rgba.width() as usize * rgba.height() as usize * 4;
        if rgba.as_raw().len() != expected_size {
            return Err("Decoded artwork had an invalid pixel buffer.".to_owned());
        }
        Ok(DecodedArtwork {
            width: rgba.width(),
            height: rgba.height(),
            rgba: rgba.into_raw(),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected,
    Authenticating,
    Connecting,
    Ready,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RepeatMode {
    Off,
    All,
    One,
}

impl RepeatMode {
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::All,
            Self::All => Self::One,
            Self::One => Self::Off,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LyricLine {
    pub timestamp_ms: u32,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct SpotifyTrack {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u32,
    pub image_url: Option<String>,
    /// The first credited artist, for "Go to artist".
    pub artist_id: Option<String>,
    pub album_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct SpotifyPlaylist {
    pub id: String,
    pub name: String,
    pub description: String,
    pub track_count: u32,
    pub owner: String,
    pub owner_id: Option<String>,
    pub collaborative: bool,
    pub image_url: Option<String>,
}

#[derive(Clone, Debug)]
pub struct SpotifyArtist {
    pub id: String,
    pub name: String,
    pub image_url: Option<String>,
}

#[derive(Clone, Debug)]
pub struct SpotifyAlbum {
    pub id: String,
    pub name: String,
    pub artist: String,
    pub year: Option<i32>,
    pub image_url: Option<String>,
}

/// The period Spotify's top tracks and artists cover.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TopRange {
    Short,
    Medium,
    Long,
}

impl TopRange {
    pub const ALL: [Self; 3] = [Self::Short, Self::Medium, Self::Long];

    pub fn label(self) -> &'static str {
        match self {
            Self::Short => "Last 4 weeks",
            Self::Medium => "Last 6 months",
            Self::Long => "Last year",
        }
    }

    fn api_value(self) -> &'static str {
        match self {
            Self::Short => "short_term",
            Self::Medium => "medium_term",
            Self::Long => "long_term",
        }
    }
}

#[derive(Debug)]
pub enum PlaybackEvent {
    State(ConnectionState),
    Ready {
        username: String,
    },
    Position {
        track_id: String,
        position_ms: u32,
        playing: bool,
    },
    TrackChanged {
        track_id: String,
        title: String,
        duration_ms: u32,
    },
    SearchResults {
        tracks: Vec<SpotifyTrack>,
    },
    Recommendations {
        tracks: Vec<SpotifyTrack>,
    },
    Playlists {
        playlists: Vec<SpotifyPlaylist>,
    },
    Profile {
        display_name: String,
        image_url: Option<String>,
    },
    PlaylistTracks {
        tracks: Vec<SpotifyTrack>,
    },
    LikedSongs {
        tracks: Vec<SpotifyTrack>,
    },
    LikedSongsError {
        message: String,
    },
    QueueEnded,
    LibraryUpdate {
        track_id: String,
        saved: bool,
        error: Option<String>,
    },
    Lyrics {
        track_id: String,
        lines: Vec<LyricLine>,
        synced: bool,
        /// Where the lyrics came from, such as "LRCLIB".
        provider: Option<String>,
        error: Option<String>,
    },
    ArtworkReady {
        url: String,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
    ArtworkError {
        url: String,
    },
    ArtistPage {
        artist_id: String,
        name: String,
        image_url: Option<String>,
        tracks: Vec<SpotifyTrack>,
        albums: Vec<SpotifyAlbum>,
        error: Option<String>,
    },
    AlbumPage {
        album_id: String,
        name: String,
        artist: String,
        artist_id: Option<String>,
        year: Option<i32>,
        image_url: Option<String>,
        tracks: Vec<SpotifyTrack>,
        error: Option<String>,
    },
    RecentlyPlayed {
        tracks: Vec<SpotifyTrack>,
        error: Option<String>,
    },
    TopItems {
        range: TopRange,
        tracks: Vec<SpotifyTrack>,
        artists: Vec<SpotifyArtist>,
        error: Option<String>,
    },
    Credits {
        track_id: String,
        result: Result<SongCredits, String>,
    },
    Radio {
        seed_track_id: String,
        tracks: Vec<SpotifyTrack>,
        error: Option<String>,
    },
    /// A short confirmation to show the listener, such as "Added to a playlist".
    Notice(String),
    Ended,
    Error(String),
}

#[derive(Debug)]
enum PlaybackCommand {
    Login {
        client_id: Option<String>,
        redirect_uri: Option<String>,
        web_api_client_id: Option<String>,
        web_api_redirect_uri: Option<String>,
    },
    Logout,
    Play,
    Pause,
    Next,
    Previous,
    SetShuffle(bool),
    SetRepeat(RepeatMode),
    CycleRepeat,
    SetTrackSaved {
        track_id: String,
        saved: bool,
    },
    LoadLyrics {
        track_id: String,
        artist: String,
        title: String,
        album: String,
        duration_ms: u32,
        source: LyricsSource,
    },
    LoadArtwork {
        url: String,
    },
    Load {
        track_id: String,
        queue: Vec<String>,
        start_playing: bool,
        start_position_ms: u32,
    },
    SetVolume(f32),
    Search {
        query: String,
    },
    LoadRecommendations,
    LoadPlaylists,
    LoadProfile,
    LoadPlaylist {
        playlist_id: String,
    },
    LoadLikedSongs,
    LoadArtist {
        artist_id: String,
        name: String,
    },
    LoadAlbum {
        album_id: String,
    },
    LoadRecentlyPlayed,
    LoadTop(TopRange),
    LoadCredits {
        track_id: String,
        title: String,
        artist: String,
        duration_ms: u32,
    },
    StartRadio {
        track_id: String,
    },
    Enqueue {
        track_id: String,
        next: bool,
    },
    AddToPlaylist {
        playlist_id: String,
        playlist_name: String,
        track_id: String,
    },
    Shutdown,
}

struct ConnectedPlayer {
    /// Spotify's own metadata and radio services, which the Web API lacks.
    session: Session,
    player: std::sync::Arc<Player>,
    web_api: Arc<WebApiClient>,
    mixer: std::sync::Arc<dyn Mixer>,
    queue: Vec<String>,
    original_queue: Vec<String>,
    current_index: Option<usize>,
    shuffle: bool,
    repeat: RepeatMode,
}

pub struct SpotifyClient {
    command_tx: tokio_mpsc::UnboundedSender<PlaybackCommand>,
    event_rx: Receiver<PlaybackEvent>,
    worker: Option<JoinHandle<()>>,
}

impl SpotifyClient {
    pub fn new(audio: AudioTaps) -> Self {
        let (command_tx, command_rx) = tokio_mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::channel();

        let worker = thread::Builder::new()
            .name("oynx-spotify".to_owned())
            .spawn(move || run_worker(command_rx, event_tx, audio))
            .expect("failed to start Spotify worker");

        Self {
            command_tx,
            event_rx,
            worker: Some(worker),
        }
    }

    pub fn login_with_config(
        &self,
        client_id: Option<String>,
        redirect_uri: Option<String>,
        web_api_client_id: Option<String>,
        web_api_redirect_uri: Option<String>,
    ) {
        let _ = self.command_tx.send(PlaybackCommand::Login {
            client_id,
            redirect_uri,
            web_api_client_id,
            web_api_redirect_uri,
        });
    }

    pub fn logout(&self) {
        let _ = self.command_tx.send(PlaybackCommand::Logout);
    }

    pub fn play(&self) {
        let _ = self.command_tx.send(PlaybackCommand::Play);
    }

    pub fn pause(&self) {
        let _ = self.command_tx.send(PlaybackCommand::Pause);
    }

    pub fn next(&self) {
        let _ = self.command_tx.send(PlaybackCommand::Next);
    }

    pub fn previous(&self) {
        let _ = self.command_tx.send(PlaybackCommand::Previous);
    }

    pub fn set_shuffle(&self, enabled: bool) {
        let _ = self.command_tx.send(PlaybackCommand::SetShuffle(enabled));
    }

    pub fn set_repeat(&self, mode: RepeatMode) {
        let _ = self.command_tx.send(PlaybackCommand::SetRepeat(mode));
    }

    pub fn cycle_repeat(&self) {
        let _ = self.command_tx.send(PlaybackCommand::CycleRepeat);
    }

    pub fn set_track_saved(&self, track_id: String, saved: bool) {
        let _ = self
            .command_tx
            .send(PlaybackCommand::SetTrackSaved { track_id, saved });
    }

    pub fn load_lyrics(
        &self,
        track_id: String,
        artist: String,
        title: String,
        album: String,
        duration_ms: u32,
        source: LyricsSource,
    ) {
        let _ = self.command_tx.send(PlaybackCommand::LoadLyrics {
            track_id,
            artist,
            title,
            album,
            duration_ms,
            source,
        });
    }

    pub fn load_artwork(&self, url: String) {
        let _ = self.command_tx.send(PlaybackCommand::LoadArtwork { url });
    }

    pub fn load(
        &self,
        track_id: String,
        queue: Vec<String>,
        start_playing: bool,
        start_position_ms: u32,
    ) {
        let _ = self.command_tx.send(PlaybackCommand::Load {
            track_id,
            queue,
            start_playing,
            start_position_ms,
        });
    }

    pub fn set_volume(&self, volume: f32) {
        let _ = self.command_tx.send(PlaybackCommand::SetVolume(volume));
    }

    pub fn search(&self, query: String) {
        let _ = self.command_tx.send(PlaybackCommand::Search { query });
    }

    pub fn load_recommendations(&self) {
        let _ = self.command_tx.send(PlaybackCommand::LoadRecommendations);
    }

    pub fn load_playlists(&self) {
        let _ = self.command_tx.send(PlaybackCommand::LoadPlaylists);
    }

    pub fn load_profile(&self) {
        let _ = self.command_tx.send(PlaybackCommand::LoadProfile);
    }

    pub fn load_playlist(&self, playlist_id: String) {
        let _ = self
            .command_tx
            .send(PlaybackCommand::LoadPlaylist { playlist_id });
    }

    pub fn load_liked_songs(&self) {
        let _ = self.command_tx.send(PlaybackCommand::LoadLikedSongs);
    }

    pub fn load_artist(&self, artist_id: String, name: String) {
        let _ = self
            .command_tx
            .send(PlaybackCommand::LoadArtist { artist_id, name });
    }

    pub fn load_album(&self, album_id: String) {
        let _ = self.command_tx.send(PlaybackCommand::LoadAlbum { album_id });
    }

    pub fn load_recently_played(&self) {
        let _ = self.command_tx.send(PlaybackCommand::LoadRecentlyPlayed);
    }

    pub fn load_top(&self, range: TopRange) {
        let _ = self.command_tx.send(PlaybackCommand::LoadTop(range));
    }

    pub fn load_credits(&self, track_id: String, title: String, artist: String, duration_ms: u32) {
        let _ = self.command_tx.send(PlaybackCommand::LoadCredits {
            track_id,
            title,
            artist,
            duration_ms,
        });
    }

    pub fn start_radio(&self, track_id: String) {
        let _ = self.command_tx.send(PlaybackCommand::StartRadio { track_id });
    }

    /// Adds a track after the current one (`next`) or at the end of the queue.
    pub fn enqueue(&self, track_id: String, next: bool) {
        let _ = self
            .command_tx
            .send(PlaybackCommand::Enqueue { track_id, next });
    }

    pub fn add_to_playlist(&self, playlist_id: String, playlist_name: String, track_id: String) {
        let _ = self.command_tx.send(PlaybackCommand::AddToPlaylist {
            playlist_id,
            playlist_name,
            track_id,
        });
    }

    pub fn poll(&self) -> Option<PlaybackEvent> {
        self.event_rx.try_recv().ok()
    }
}

impl Drop for SpotifyClient {
    fn drop(&mut self) {
        let _ = self.command_tx.send(PlaybackCommand::Shutdown);
        // Do not join here: an in-flight browser OAuth callback can be waiting
        // indefinitely, and closing the window should never freeze the UI.
        let _ = self.worker.take();
    }
}

fn spawn_api_job<F, Fut>(
    api: Arc<WebApiClient>,
    events: Sender<PlaybackEvent>,
    permits: Arc<Semaphore>,
    job: F,
) where
    F: FnOnce(Arc<WebApiClient>, Sender<PlaybackEvent>) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        let Ok(_permit) = permits.acquire_owned().await else {
            return;
        };
        job(api, events).await;
    });
}

fn run_worker(
    mut commands: tokio_mpsc::UnboundedReceiver<PlaybackCommand>,
    events: Sender<PlaybackEvent>,
    audio: AudioTaps,
) {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = events.send(PlaybackEvent::Error(format!(
                "Could not start Spotify runtime: {error}"
            )));
            return;
        }
    };

    runtime.block_on(async move {
        let cache = match create_cache() {
            Ok(cache) => cache,
            Err(error) => {
                let _ = events.send(PlaybackEvent::State(ConnectionState::Disconnected));
                let _ = events.send(PlaybackEvent::Error(error));
                return;
            }
        };

        let spotify_config = SpotifyConfig::load();
        let cached_client_id = std::env::var("OYNX_SPOTIFY_CLIENT_ID")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(spotify_config.client_id);
        let cached_redirect_uri = std::env::var("OYNX_SPOTIFY_REDIRECT_URI")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(spotify_config.redirect_uri);
        let cached_web_api_client_id = std::env::var("OYNX_SPOTIFY_WEB_API_CLIENT_ID")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| {
                if spotify_config.web_api_client_id.is_empty() {
                    cached_client_id.clone()
                } else {
                    spotify_config.web_api_client_id.clone()
                }
            });
        let cached_web_api_redirect_uri = std::env::var("OYNX_SPOTIFY_WEB_API_REDIRECT_URI")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| {
                if spotify_config.web_api_redirect_uri.is_empty() {
                    if cached_web_api_client_id == cached_client_id {
                        cached_redirect_uri.clone()
                    } else {
                        DEFAULT_WEB_API_REDIRECT_URI.to_owned()
                    }
                } else {
                    spotify_config.web_api_redirect_uri.clone()
                }
            });
        let saved_web_token =
            load_saved_token(&cached_web_api_client_id, &cached_web_api_redirect_uri).await;
        let api_permits = Arc::new(Semaphore::new(2));
        let mut connected = None;
        if let Some(credentials) = cache.credentials() {
            let cached_web_token = saved_web_token.or_else(|| {
                (cached_web_api_client_id == cached_client_id)
                    .then(|| token_from_cached_credentials(&credentials))
                    .flatten()
            });
            let _ = events.send(PlaybackEvent::State(ConnectionState::Connecting));
            match connect(
                &cache,
                credentials,
                &cached_client_id,
                &cached_web_api_client_id,
                &cached_web_api_redirect_uri,
                cached_web_token,
                &events,
                &audio,
            )
            .await
            {
                Ok(player) => connected = Some(player),
                Err(error) => {
                    let _ = events.send(PlaybackEvent::State(ConnectionState::Disconnected));
                    let _ = events.send(PlaybackEvent::Error(format!(
                        "Could not restore the previous Spotify session: {error}"
                    )));
                }
            }
        }

        while let Some(command) = commands.recv().await {
            match command {
                PlaybackCommand::Login {
                    client_id,
                    redirect_uri,
                    web_api_client_id,
                    web_api_redirect_uri,
                } => {
                    if connected.is_some() {
                        continue;
                    }

                    let _ = events.send(PlaybackEvent::State(ConnectionState::Authenticating));
                    match oauth_credentials(
                        client_id,
                        redirect_uri,
                        web_api_client_id,
                        web_api_redirect_uri,
                    )
                    .await
                    {
                        Ok(login) => {
                            let _ = events.send(PlaybackEvent::State(ConnectionState::Connecting));
                            match connect(
                                &cache,
                                login.credentials,
                                &login.streaming_client_id,
                                &login.web_api_client_id,
                                &login.web_api_redirect_uri,
                                Some(login.web_token),
                                &events,
                                &audio,
                            )
                            .await
                            {
                                Ok(player) => connected = Some(player),
                                Err(error) => {
                                    let _ = events
                                        .send(PlaybackEvent::State(ConnectionState::Disconnected));
                                    let _ = events.send(PlaybackEvent::Error(error));
                                }
                            }
                        }
                        Err(error) => {
                            let _ =
                                events.send(PlaybackEvent::State(ConnectionState::Disconnected));
                            let _ = events.send(PlaybackEvent::Error(error));
                        }
                    }
                }
                PlaybackCommand::Logout => {
                    connected = None;
                    clear_cached_credentials();
                    let _ = events.send(PlaybackEvent::State(ConnectionState::Disconnected));
                }
                PlaybackCommand::Play => {
                    if let Some(player) = connected.as_ref() {
                        player.player.play();
                    }
                }
                PlaybackCommand::Pause => {
                    if let Some(player) = connected.as_ref() {
                        player.player.pause();
                    }
                }
                PlaybackCommand::Next => {
                    if let Some(player) = connected.as_mut() {
                        move_in_queue(player, 1, false, &events);
                    }
                }
                PlaybackCommand::Previous => {
                    if let Some(player) = connected.as_mut() {
                        move_in_queue(player, -1, false, &events);
                    }
                }
                PlaybackCommand::SetShuffle(enabled) => {
                    if let Some(player) = connected.as_mut() {
                        set_shuffle(player, enabled);
                    }
                }
                PlaybackCommand::SetRepeat(mode) => {
                    if let Some(player) = connected.as_mut() {
                        player.repeat = mode;
                    }
                }
                PlaybackCommand::CycleRepeat => {
                    if let Some(player) = connected.as_mut() {
                        player.repeat = player.repeat.next();
                    }
                }
                PlaybackCommand::SetTrackSaved { track_id, saved } => {
                    if let Some(player) = connected.as_ref() {
                        spawn_api_job(
                            player.web_api.clone(),
                            events.clone(),
                            api_permits.clone(),
                            move |api, events| async move {
                                let error = api.set_track_saved(&track_id, saved).await.err();
                                let _ = events.send(PlaybackEvent::LibraryUpdate {
                                    track_id,
                                    saved,
                                    error,
                                });
                            },
                        );
                    }
                }
                PlaybackCommand::LoadLyrics {
                    track_id,
                    artist,
                    title,
                    album,
                    duration_ms,
                    source,
                } => {
                    if let Some(player) = connected.as_ref() {
                        let session = player.session.clone();
                        spawn_api_job(
                            player.web_api.clone(),
                            events.clone(),
                            api_permits.clone(),
                            move |api, events| async move {
                                let song = SongForLyrics {
                                    track_id: &track_id,
                                    artist: &artist,
                                    title: &title,
                                    album: &album,
                                    duration_ms,
                                };
                                let result = find_lyrics(&api.http, &session, source, &song).await;
                                let event = match result {
                                    Ok(found) => PlaybackEvent::Lyrics {
                                        track_id,
                                        lines: found.lines,
                                        synced: found.synced,
                                        provider: Some(found.provider),
                                        error: None,
                                    },
                                    Err(error) => PlaybackEvent::Lyrics {
                                        track_id,
                                        lines: Vec::new(),
                                        synced: false,
                                        provider: None,
                                        error: Some(error),
                                    },
                                };
                                let _ = events.send(event);
                            },
                        );
                    }
                }
                PlaybackCommand::LoadArtwork { url } => {
                    if let Some(player) = connected.as_ref() {
                        spawn_api_job(
                            player.web_api.clone(),
                            events.clone(),
                            api_permits.clone(),
                            move |api, events| async move {
                                match api.fetch_artwork(&url).await {
                                    Ok(artwork) => {
                                        let _ = events.send(PlaybackEvent::ArtworkReady {
                                            url,
                                            width: artwork.width,
                                            height: artwork.height,
                                            rgba: artwork.rgba,
                                        });
                                    }
                                    Err(_) => {
                                        let _ = events.send(PlaybackEvent::ArtworkError { url });
                                    }
                                }
                            },
                        );
                    }
                }
                PlaybackCommand::Load {
                    track_id,
                    queue,
                    start_playing,
                    start_position_ms,
                } => {
                    if let Some(player) = connected.as_mut() {
                        player.original_queue = queue.clone();
                        player.queue = if player.shuffle {
                            shuffle_queue(queue, Some(&track_id))
                        } else {
                            queue
                        };
                        player.current_index = player
                            .queue
                            .iter()
                            .position(|queued_id| queued_id == &track_id);
                        if let Err(error) =
                            load_track(player, &track_id, start_playing, start_position_ms)
                        {
                            let _ = events.send(PlaybackEvent::Error(error));
                        }
                    }
                }
                PlaybackCommand::SetVolume(volume) => {
                    if let Some(player) = connected.as_ref() {
                        let volume = (volume.clamp(0.0, 1.0) * u16::MAX as f32) as u16;
                        player.mixer.set_volume(volume);
                    }
                }
                PlaybackCommand::Search { query } => {
                    if let Some(player) = connected.as_ref() {
                        spawn_api_job(
                            player.web_api.clone(),
                            events.clone(),
                            api_permits.clone(),
                            move |api, events| async move {
                                search_spotify(&api, &query, &events).await;
                            },
                        );
                    }
                }
                PlaybackCommand::LoadRecommendations => {
                    if let Some(player) = connected.as_ref() {
                        spawn_api_job(
                            player.web_api.clone(),
                            events.clone(),
                            api_permits.clone(),
                            |api, events| async move {
                                load_recommendations(&api, &events).await;
                            },
                        );
                    }
                }
                PlaybackCommand::LoadPlaylists => {
                    if let Some(player) = connected.as_ref() {
                        spawn_api_job(
                            player.web_api.clone(),
                            events.clone(),
                            api_permits.clone(),
                            |api, events| async move {
                                load_playlists(&api, &events).await;
                            },
                        );
                    }
                }
                PlaybackCommand::LoadProfile => {
                    if let Some(player) = connected.as_ref() {
                        spawn_api_job(
                            player.web_api.clone(),
                            events.clone(),
                            api_permits.clone(),
                            |api, events| async move {
                                load_profile(&api, &events).await;
                            },
                        );
                    }
                }
                PlaybackCommand::LoadPlaylist { playlist_id } => {
                    if let Some(player) = connected.as_ref() {
                        spawn_api_job(
                            player.web_api.clone(),
                            events.clone(),
                            api_permits.clone(),
                            move |api, events| async move {
                                load_playlist_tracks(&api, &playlist_id, &events).await;
                            },
                        );
                    }
                }
                PlaybackCommand::LoadLikedSongs => {
                    if let Some(player) = connected.as_ref() {
                        spawn_api_job(
                            player.web_api.clone(),
                            events.clone(),
                            api_permits.clone(),
                            |api, events| async move {
                                load_liked_songs(&api, &events).await;
                            },
                        );
                    }
                }
                PlaybackCommand::LoadArtist { artist_id, name } => {
                    if let Some(player) = connected.as_ref() {
                        let session = player.session.clone();
                        spawn_api_job(
                            player.web_api.clone(),
                            events.clone(),
                            api_permits.clone(),
                            move |api, events| async move {
                                load_artist(&api, &session, &artist_id, &name, &events).await;
                            },
                        );
                    }
                }
                PlaybackCommand::LoadAlbum { album_id } => {
                    if let Some(player) = connected.as_ref() {
                        spawn_api_job(
                            player.web_api.clone(),
                            events.clone(),
                            api_permits.clone(),
                            move |api, events| async move {
                                load_album(&api, &album_id, &events).await;
                            },
                        );
                    }
                }
                PlaybackCommand::LoadRecentlyPlayed => {
                    if let Some(player) = connected.as_ref() {
                        spawn_api_job(
                            player.web_api.clone(),
                            events.clone(),
                            api_permits.clone(),
                            |api, events| async move {
                                load_recently_played(&api, &events).await;
                            },
                        );
                    }
                }
                PlaybackCommand::LoadTop(range) => {
                    if let Some(player) = connected.as_ref() {
                        spawn_api_job(
                            player.web_api.clone(),
                            events.clone(),
                            api_permits.clone(),
                            move |api, events| async move {
                                load_top(&api, range, &events).await;
                            },
                        );
                    }
                }
                PlaybackCommand::LoadCredits {
                    track_id,
                    title,
                    artist,
                    duration_ms,
                } => {
                    if let Some(player) = connected.as_ref() {
                        let session = player.session.clone();
                        // MusicBrainz is slow and rate limited, so credits do not
                        // take one of the Web API permits.
                        let events = events.clone();
                        tokio::spawn(async move {
                            let mut query = CreditsQuery {
                                title,
                                artist,
                                duration_ms,
                                ..CreditsQuery::default()
                            };
                            if let Some(metadata) = track_metadata(&session, &track_id).await {
                                query.isrc = metadata
                                    .external_ids
                                    .iter()
                                    .find(|id| id.external_type.eq_ignore_ascii_case("isrc"))
                                    .map(|id| id.id.clone());
                                query.spotify_credits = metadata
                                    .artists_with_role
                                    .iter()
                                    .filter_map(|artist| {
                                        spotify_role_label(artist.role)
                                            .map(|role| (role.to_owned(), artist.name.clone()))
                                    })
                                    .collect();
                            }
                            let result = credits::lookup(&query).await;
                            let _ = events.send(PlaybackEvent::Credits { track_id, result });
                        });
                    }
                }
                PlaybackCommand::StartRadio { track_id } => {
                    if let Some(player) = connected.as_ref() {
                        let session = player.session.clone();
                        let events = events.clone();
                        tokio::spawn(async move {
                            let (tracks, error) = match song_radio(&session, &track_id).await {
                                Ok(tracks) if !tracks.is_empty() => (tracks, None),
                                Ok(_) => (
                                    Vec::new(),
                                    Some("Spotify has no radio for this song yet.".to_owned()),
                                ),
                                Err(error) => (Vec::new(), Some(error)),
                            };
                            let _ = events.send(PlaybackEvent::Radio {
                                seed_track_id: track_id,
                                tracks,
                                error,
                            });
                        });
                    }
                }
                PlaybackCommand::Enqueue { track_id, next } => {
                    if let Some(player) = connected.as_mut() {
                        enqueue(player, track_id, next, &events);
                    }
                }
                PlaybackCommand::AddToPlaylist {
                    playlist_id,
                    playlist_name,
                    track_id,
                } => {
                    if let Some(player) = connected.as_ref() {
                        spawn_api_job(
                            player.web_api.clone(),
                            events.clone(),
                            api_permits.clone(),
                            move |api, events| async move {
                                let url = format!(
                                    "https://api.spotify.com/v1/playlists/{playlist_id}/items"
                                );
                                let body = serde_json::json!({
                                    "uris": [format!("spotify:track:{track_id}")]
                                });
                                let event = match api.send_json(reqwest::Method::POST, &url, &body).await {
                                    Ok(()) => PlaybackEvent::Notice(format!("Added to {playlist_name}")),
                                    Err(error) => PlaybackEvent::Error(format!(
                                        "Could not add the song to {playlist_name}: {error}"
                                    )),
                                };
                                let _ = events.send(event);
                            },
                        );
                    }
                }
                PlaybackCommand::Shutdown => break,
            }
        }
    });
}

fn create_cache() -> Result<Cache, String> {
    let root = SpotifyConfig::cache_path();
    let audio = root.join("files");

    Cache::new(
        Some(&root),
        Some(&root),
        Some(&audio),
        Some(AUDIO_CACHE_LIMIT),
    )
    .map_err(|error| format!("Could not create the Spotify cache: {error}"))
}

fn clear_cached_credentials() {
    let root = SpotifyConfig::cache_path();
    let _ = std::fs::remove_file(root.join("credentials.json"));
    let _ = std::fs::remove_file(web_api_token_path());
    let _ = std::fs::remove_dir_all(web_api_cache_path());
}

struct OAuthLogin {
    credentials: Credentials,
    streaming_client_id: String,
    web_api_client_id: String,
    web_api_redirect_uri: String,
    web_token: OAuthToken,
}

fn authorize_token(
    client_id: &str,
    redirect_uri: &str,
    scopes: &[&str],
    message: &str,
) -> Result<OAuthToken, String> {
    let client = OAuthClientBuilder::new(client_id, redirect_uri, scopes.to_vec())
        .open_in_browser()
        .with_custom_message(message)
        .build()
        .map_err(|error| format!("Could not start Spotify sign-in: {error}"))?;
    client
        .get_access_token()
        .map_err(|error| format!("Spotify sign-in failed: {error}"))
}

async fn oauth_credentials(
    configured_client_id: Option<String>,
    configured_redirect_uri: Option<String>,
    configured_web_api_client_id: Option<String>,
    configured_web_api_redirect_uri: Option<String>,
) -> Result<OAuthLogin, String> {
    tokio::task::spawn_blocking(move || {
        let streaming_client_id = std::env::var("OYNX_SPOTIFY_CLIENT_ID")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                configured_client_id.filter(|value| !value.trim().is_empty())
            })
            .unwrap_or_else(|| DEFAULT_CLIENT_ID.to_owned());
        let streaming_redirect_uri = std::env::var("OYNX_SPOTIFY_REDIRECT_URI")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                configured_redirect_uri.filter(|value| !value.trim().is_empty())
            })
            .unwrap_or_else(|| DEFAULT_REDIRECT_URI.to_owned());
        let web_api_client_id = std::env::var("OYNX_SPOTIFY_WEB_API_CLIENT_ID")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                configured_web_api_client_id.filter(|value| !value.trim().is_empty())
            })
            .unwrap_or_else(|| streaming_client_id.clone());
        let web_api_redirect_uri = std::env::var("OYNX_SPOTIFY_WEB_API_REDIRECT_URI")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                configured_web_api_redirect_uri.filter(|value| !value.trim().is_empty())
            })
            .unwrap_or_else(|| {
                if web_api_client_id == streaming_client_id {
                    streaming_redirect_uri.clone()
                } else {
                    DEFAULT_WEB_API_REDIRECT_URI.to_owned()
                }
            });

        let same_app = web_api_client_id == streaming_client_id
            && web_api_redirect_uri == streaming_redirect_uri;
        let (streaming_token, web_token) = if same_app {
            let scopes = combined_oauth_scopes();
            let token = authorize_token(
                &streaming_client_id,
                &streaming_redirect_uri,
                &scopes,
                "<html><body style='font-family:sans-serif;padding:40px'><h2>Oynx is connected.</h2><p>You can close this tab and return to Oynx.</p></body></html>",
            )?;
            (token.clone(), token)
        } else {
            let streaming_token = authorize_token(
                &streaming_client_id,
                &streaming_redirect_uri,
                STREAMING_SCOPES,
                "<html><body style='font-family:sans-serif;padding:40px'><h2>Oynx playback is connected.</h2><p>Authorize the Web API app next to load your Spotify library.</p></body></html>",
            )?;
            let web_token = authorize_token(
                &web_api_client_id,
                &web_api_redirect_uri,
                &requested_web_api_scopes(),
                "<html><body style='font-family:sans-serif;padding:40px'><h2>Oynx Web API is connected.</h2><p>You can close this tab and return to Oynx.</p></body></html>",
            )?;
            (streaming_token, web_token)
        };

        if let Err(error) = save_oauth_token(
            &web_api_client_id,
            &web_api_redirect_uri,
            &web_token,
        ) {
            log::warn!(
                "Spotify sign-in succeeded, but the Web API token could not be cached: {error}"
            );
        }
        Ok(OAuthLogin {
            credentials: Credentials::with_access_token(streaming_token.access_token),
            streaming_client_id,
            web_api_client_id,
            web_api_redirect_uri,
            web_token,
        })
    })
    .await
    .map_err(|error| format!("Spotify sign-in worker failed: {error}"))?
}

async fn connect(
    cache: &Cache,
    credentials: Credentials,
    client_id: &str,
    web_api_client_id: &str,
    web_api_redirect_uri: &str,
    web_token: Option<OAuthToken>,
    events: &Sender<PlaybackEvent>,
    audio: &AudioTaps,
) -> Result<ConnectedPlayer, String> {
    let session_config = SessionConfig {
        client_id: client_id.to_owned(),
        ..SessionConfig::default()
    };
    let session = Session::new(session_config, Some(cache.clone()));
    session
        .connect(credentials, true)
        .await
        .map_err(|error| format!("Spotify session connection failed: {error}"))?;

    let sink_builder = audio_backend::find(None)
        .ok_or_else(|| "No supported audio backend was found".to_owned())?;
    let mixer_builder =
        mixer::find(None).ok_or_else(|| "No supported audio mixer was found".to_owned())?;
    let mixer = mixer_builder(MixerConfig::default())
        .map_err(|error| format!("Could not open the audio mixer: {error}"))?;

    let username = session.username();
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_default();
    let web_api = Arc::new(WebApiClient::new(
        http,
        web_api_client_id.to_owned(),
        web_api_redirect_uri.to_owned(),
        format!("{web_api_client_id}:{username}"),
        requested_web_api_scopes()
            .into_iter()
            .map(str::to_owned)
            .collect(),
        web_token,
    ));
    let audio_format = AudioFormat::default();
    let player_config = PlayerConfig {
        position_update_interval: Some(Duration::from_millis(250)),
        ..PlayerConfig::default()
    };
    let audio = audio.clone();
    let player = Player::new(player_config, session.clone(), mixer.get_soft_volume(), move || {
        audio.wrap_sink(sink_builder(None, audio_format), librespot::playback::SAMPLE_RATE as f32)
    });

    let mut player_events = player.get_player_event_channel();
    let event_sender = events.clone();
    tokio::spawn(async move {
        while let Some(event) = player_events.recv().await {
            forward_player_event(event, &event_sender);
        }
    });

    let _ = events.send(PlaybackEvent::Ready { username });
    Ok(ConnectedPlayer {
        session,
        player,
        web_api,
        mixer,
        queue: Vec::new(),
        original_queue: Vec::new(),
        current_index: None,
        shuffle: false,
        repeat: RepeatMode::Off,
    })
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    tracks: SearchTrackPage,
}

#[derive(Debug, Deserialize)]
struct SearchTrackPage {
    items: Vec<SearchTrack>,
}

#[derive(Clone, Debug, Deserialize)]
struct SearchTrack {
    id: String,
    name: String,
    duration_ms: u32,
    artists: Vec<SearchArtist>,
    album: SearchAlbum,
}

#[derive(Clone, Debug, Deserialize)]
struct SearchArtist {
    #[serde(default)]
    id: Option<String>,
    name: String,
    #[serde(default)]
    images: Vec<ApiImage>,
}

#[derive(Clone, Debug, Deserialize)]
struct SearchAlbum {
    #[serde(default)]
    id: Option<String>,
    name: String,
    #[serde(default)]
    images: Vec<ApiImage>,
    #[serde(default)]
    artists: Vec<SearchArtist>,
    #[serde(default)]
    release_date: Option<String>,
}

impl SearchAlbum {
    fn into_album(self) -> Option<SpotifyAlbum> {
        Some(SpotifyAlbum {
            id: self.id?,
            year: self
                .release_date
                .as_deref()
                .and_then(|date| date.get(..4))
                .and_then(|year| year.parse().ok()),
            image_url: select_image(&self.images),
            artist: join_artists(&self.artists),
            name: self.name,
        })
    }
}

fn join_artists(artists: &[SearchArtist]) -> String {
    artists
        .iter()
        .map(|artist| artist.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Clone, Debug, Deserialize)]
struct ApiImage {
    url: String,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
}

fn select_image(images: &[ApiImage]) -> Option<String> {
    let area = |image: &ApiImage| image.width.unwrap_or(0) * image.height.unwrap_or(0);
    images
        .iter()
        .filter(|image| area(image) >= 90_000)
        .min_by_key(|image| area(image))
        .or_else(|| images.iter().max_by_key(|image| area(image)))
        .map(|image| image.url.clone())
}

#[derive(Debug, Deserialize)]
struct TopArtistsResponse {
    items: Vec<TopArtist>,
}

#[derive(Debug, Deserialize)]
struct TopArtist {
    id: String,
}

#[derive(Debug, Deserialize)]
struct TopTracksResponse {
    items: Vec<SearchTrack>,
}

#[derive(Debug, Deserialize)]
struct RecommendationsResponse {
    tracks: Vec<SearchTrack>,
}

#[cfg(test)]
#[derive(Debug, Deserialize)]
struct PlaylistResponse {
    items: Vec<ApiPlaylist>,
}

#[derive(Debug, Deserialize)]
struct ApiPlaylist {
    id: String,
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    tracks: Option<ApiPlaylistTracks>,
    #[serde(default)]
    items: Option<ApiPlaylistItems>,
    #[serde(default)]
    owner: Option<ApiPlaylistOwner>,
    #[serde(default)]
    collaborative: bool,
    #[serde(default)]
    images: Vec<ApiImage>,
}

#[derive(Debug, Default, Deserialize)]
struct ApiPlaylistTracks {
    #[serde(default)]
    total: u32,
}

#[derive(Debug, Default, Deserialize)]
struct ApiPlaylistItems {
    #[serde(default)]
    total: u32,
}

#[derive(Debug, Deserialize)]
struct ApiPlaylistOwner {
    #[serde(default)]
    id: Option<String>,
    display_name: Option<String>,
}

impl ApiPlaylist {
    fn track_count(&self) -> u32 {
        self.tracks
            .as_ref()
            .map(|tracks| tracks.total)
            .or_else(|| self.items.as_ref().map(|items| items.total))
            .unwrap_or_default()
    }
}

#[derive(Debug, Deserialize)]
struct PlaylistItem {
    #[serde(default)]
    track: Option<SearchTrack>,
    #[serde(default)]
    item: Option<SearchTrack>,
}

#[derive(Debug, Deserialize)]
struct SavedTracksResponse {
    #[serde(default)]
    items: Vec<SavedTrackItem>,
    #[serde(default)]
    next: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SavedTrackItem {
    #[serde(default)]
    track: Option<SearchTrack>,
}

#[derive(Debug, Deserialize)]
struct PageResponse {
    #[serde(default)]
    items: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    next: Option<String>,
    #[serde(default)]
    total: Option<u32>,
}

struct PageResult {
    items: Vec<serde_json::Value>,
    total: Option<u32>,
    truncated: bool,
    error: Option<String>,
}

async fn fetch_all_pages(
    client: &WebApiClient,
    first_url: &str,
    first_query: &[(String, String)],
    max_pages: usize,
) -> Result<PageResult, String> {
    let mut url = Some(first_url.to_owned());
    let mut items = Vec::new();
    let mut total = None;
    let mut pages = 0;
    let mut last_error = None;

    while let Some(current_url) = url.take() {
        if pages >= max_pages {
            return Ok(PageResult {
                items,
                total,
                truncated: true,
                error: last_error,
            });
        }
        let query = if pages == 0 { first_query } else { &[] };
        match client.get_json::<PageResponse>(&current_url, query).await {
            Ok(response) => {
                items.extend(response.items.unwrap_or_default());
                if total.is_none() {
                    total = response.total;
                }
                url = response.next;
                pages += 1;
            }
            Err(error) => {
                last_error = Some(error);
                break;
            }
        }
    }

    if items.is_empty()
        && let Some(error) = last_error
    {
        return Err(error);
    }
    Ok(PageResult {
        items,
        total,
        truncated: false,
        error: last_error,
    })
}

fn map_search_track(track: SearchTrack) -> SpotifyTrack {
    SpotifyTrack {
        id: track.id,
        title: track.name,
        artist: join_artists(&track.artists),
        artist_id: track.artists.first().and_then(|artist| artist.id.clone()),
        album: track.album.name,
        album_id: track.album.id,
        duration_ms: track.duration_ms,
        image_url: select_image(&track.album.images),
    }
}

pub(crate) fn parse_lrc(lrc: &str) -> Vec<LyricLine> {
    let mut lines = Vec::new();
    for line in lrc.lines() {
        let mut rest = line;
        let mut timestamps = Vec::new();
        while rest.starts_with('[') {
            let Some(end) = rest.find(']') else {
                break;
            };
            if let Some(timestamp) = parse_lrc_timestamp(&rest[1..end]) {
                timestamps.push(timestamp);
            }
            rest = rest[end + 1..].trim_start();
        }
        let text = rest.trim().to_owned();
        for timestamp_ms in timestamps {
            lines.push(LyricLine {
                timestamp_ms,
                text: text.clone(),
            });
        }
    }
    lines.sort_by_key(|line| line.timestamp_ms);
    lines
}

fn parse_lrc_timestamp(tag: &str) -> Option<u32> {
    let (minutes, rest) = tag.split_once(':')?;
    let minutes = minutes.parse::<u32>().ok()?;
    let (seconds, fraction) = match rest.split_once('.') {
        Some((seconds, fraction)) => (seconds.parse::<u32>().ok()?, fraction),
        None => (rest.parse::<u32>().ok()?, ""),
    };
    if !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let fraction = if fraction.is_empty() {
        0
    } else {
        format!("{:0<3}", &fraction[..fraction.len().min(3)])
            .parse()
            .ok()?
    };
    minutes
        .checked_mul(60)?
        .checked_add(seconds)?
        .checked_mul(1000)?
        .checked_add(fraction)
}

async fn search_spotify(client: &WebApiClient, query: &str, events: &Sender<PlaybackEvent>) {
    let query = query.trim();
    if query.is_empty() {
        return;
    }

    let params = vec![
        ("q".to_owned(), query.to_owned()),
        ("type".to_owned(), "track".to_owned()),
        ("limit".to_owned(), "10".to_owned()),
    ];
    match client
        .get_json::<SearchResponse>("https://api.spotify.com/v1/search", &params)
        .await
    {
        Ok(response) => {
            let tracks = response
                .tracks
                .items
                .into_iter()
                .map(map_search_track)
                .collect();
            let _ = events.send(PlaybackEvent::SearchResults { tracks });
        }
        Err(error) => {
            let _ = events.send(PlaybackEvent::Error(format!(
                "Spotify search failed: {error}"
            )));
        }
    }
}

async fn load_recommendations(client: &WebApiClient, events: &Sender<PlaybackEvent>) {
    let top_artists = client
        .get_json::<TopArtistsResponse>(
            "https://api.spotify.com/v1/me/top/artists",
            &[
                ("limit".to_owned(), "5".to_owned()),
                ("time_range".to_owned(), "short_term".to_owned()),
            ],
        )
        .await
        .ok();

    let mut recommendation_query = vec![("limit".to_owned(), "20".to_owned())];
    if let Some(artists) = top_artists
        .as_ref()
        .filter(|response| !response.items.is_empty())
    {
        recommendation_query.push((
            "seed_artists".to_owned(),
            artists
                .items
                .iter()
                .map(|artist| artist.id.as_str())
                .collect::<Vec<_>>()
                .join(","),
        ));
    } else {
        recommendation_query.push(("seed_genres".to_owned(), "pop".to_owned()));
    }

    let recommendation_result = client
        .get_json::<RecommendationsResponse>(
            "https://api.spotify.com/v1/recommendations",
            &recommendation_query,
        )
        .await;
    if let Ok(response) = &recommendation_result {
        let tracks = response
            .tracks
            .iter()
            .cloned()
            .map(map_search_track)
            .collect::<Vec<_>>();
        if !tracks.is_empty() {
            let _ = events.send(PlaybackEvent::Recommendations { tracks });
            return;
        }
    }

    let fallback = client
        .get_json::<TopTracksResponse>(
            "https://api.spotify.com/v1/me/top/tracks",
            &[
                ("limit".to_owned(), "20".to_owned()),
                ("time_range".to_owned(), "short_term".to_owned()),
            ],
        )
        .await;
    match fallback {
        Ok(response) if !response.items.is_empty() => {
            let tracks = response.items.into_iter().map(map_search_track).collect();
            let _ = events.send(PlaybackEvent::Recommendations { tracks });
        }
        Ok(_) => {
            let _ = events.send(PlaybackEvent::Error(
                "Spotify did not return enough personalized listening data yet. Play a little more, then refresh."
                    .to_owned(),
            ));
        }
        Err(error) => {
            let detail = match recommendation_result {
                Ok(_) => "the recommendations endpoint returned no tracks".to_owned(),
                Err(recommendation_error) => recommendation_error,
            };
            let _ = events.send(PlaybackEvent::Error(format!(
                "Could not load personalized recommendations ({detail}); top tracks also failed: {error}"
            )));
        }
    }
}

/// Loads the signed-in user's display name and a small profile picture.
/// Failures are only logged: the UI falls back to the account ID.
async fn load_profile(client: &WebApiClient, events: &Sender<PlaybackEvent>) {
    let profile = match client
        .get_json::<serde_json::Value>("https://api.spotify.com/v1/me", &[])
        .await
    {
        Ok(profile) => profile,
        Err(error) => {
            log::debug!("Could not load the Spotify profile: {error}");
            return;
        }
    };
    let display_name = profile
        .get("display_name")
        .and_then(|value| value.as_str())
        .filter(|name| !name.trim().is_empty())
        .or_else(|| profile.get("id").and_then(|value| value.as_str()))
        .unwrap_or_default()
        .to_owned();
    if display_name.is_empty() {
        return;
    }
    // Prefer the smallest picture that is still sharp at avatar size.
    let image_url = profile
        .get("images")
        .and_then(|value| value.as_array())
        .and_then(|images| {
            let width = |image: &serde_json::Value| {
                image.get("width").and_then(|value| value.as_u64()).unwrap_or(0)
            };
            images
                .iter()
                .filter(|image| width(image) >= 64)
                .min_by_key(|image| width(image))
                .or_else(|| images.first())
        })
        .and_then(|image| image.get("url"))
        .and_then(|value| value.as_str())
        .map(str::to_owned);
    let _ = events.send(PlaybackEvent::Profile {
        display_name,
        image_url,
    });
}

async fn load_playlists(client: &WebApiClient, events: &Sender<PlaybackEvent>) {
    let result = fetch_all_pages(
        client,
        "https://api.spotify.com/v1/me/playlists",
        &[("limit".to_owned(), "50".to_owned())],
        MAX_API_PAGES,
    )
    .await;
    let page = match result {
        Ok(page) => page,
        Err(error) => {
            let _ = events.send(PlaybackEvent::Error(format!(
                "Could not load your Spotify playlists: {error}"
            )));
            return;
        }
    };

    if page.truncated {
        log::debug!(
            "Spotify playlist pagination reached the safety page cap after {} items (total {:?})",
            page.items.len(),
            page.total
        );
    }
    if let Some(error) = &page.error {
        log::debug!("Spotify playlist pagination stopped early: {error}");
    }
    let playlists = page
        .items
        .into_iter()
        .filter_map(|value| serde_json::from_value::<ApiPlaylist>(value).ok())
        .map(|playlist| {
            let track_count = playlist.track_count();
            let owner_id = playlist.owner.as_ref().and_then(|owner| owner.id.clone());
            let owner = playlist
                .owner
                .and_then(|owner| owner.display_name)
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| "Spotify".to_owned());
            let description = playlist
                .description
                .filter(|description| !description.trim().is_empty())
                .map(|description| description.split_whitespace().collect::<Vec<_>>().join(" "))
                .unwrap_or_else(|| format!("{track_count} songs"));
            SpotifyPlaylist {
                id: playlist.id,
                name: playlist.name,
                description,
                track_count,
                owner,
                owner_id,
                collaborative: playlist.collaborative,
                image_url: select_image(&playlist.images),
            }
        })
        .collect();
    let _ = events.send(PlaybackEvent::Playlists { playlists });
}

async fn load_playlist_tracks(
    client: &WebApiClient,
    playlist_id: &str,
    events: &Sender<PlaybackEvent>,
) {
    let url = format!("https://api.spotify.com/v1/playlists/{playlist_id}/items");
    let result = fetch_all_pages(
        client,
        &url,
        &[("limit".to_owned(), "50".to_owned())],
        MAX_API_PAGES,
    )
    .await;
    let page = match result {
        Ok(page) => page,
        Err(error) => {
            let _ = events.send(PlaybackEvent::Error(format!(
                "Could not load playlist tracks: {error}"
            )));
            return;
        }
    };

    if page.truncated {
        log::debug!(
            "Spotify playlist-track pagination reached the safety page cap after {} items (total {:?})",
            page.items.len(),
            page.total
        );
    }
    if let Some(error) = &page.error {
        log::debug!("Spotify playlist-track pagination stopped early: {error}");
    }
    let tracks: Vec<SpotifyTrack> = page
        .items
        .into_iter()
        .filter_map(|value| serde_json::from_value::<PlaylistItem>(value).ok())
        .filter_map(|item| item.track.or(item.item))
        .map(map_search_track)
        .collect();
    if tracks.is_empty()
        && let Some(error) = page.error
    {
        let _ = events.send(PlaybackEvent::Error(format!(
            "Could not load playlist tracks: {error}"
        )));
    } else {
        let _ = events.send(PlaybackEvent::PlaylistTracks { tracks });
    }
}

async fn load_liked_songs(client: &WebApiClient, events: &Sender<PlaybackEvent>) {
    let mut url = Some("https://api.spotify.com/v1/me/tracks".to_owned());
    let mut tracks = Vec::new();
    let mut pages = 0;
    let mut last_error = None;

    while let Some(current_url) = url.take() {
        if pages >= MAX_LIKED_SONG_PAGES {
            break;
        }
        let result = if pages == 0 {
            client
                .get_json::<SavedTracksResponse>(
                    &current_url,
                    &[("limit".to_owned(), "50".to_owned())],
                )
                .await
        } else {
            client
                .get_json::<SavedTracksResponse>(&current_url, &[])
                .await
        };
        match result {
            Ok(response) => {
                tracks.extend(
                    response
                        .items
                        .into_iter()
                        .filter_map(|item| item.track)
                        .map(map_search_track),
                );
                url = response.next;
                pages += 1;
                if pages % 3 == 0 && !tracks.is_empty() {
                    let _ = events.send(PlaybackEvent::LikedSongs {
                        tracks: tracks.clone(),
                    });
                }
            }
            Err(error) => {
                last_error = Some(error);
                break;
            }
        }
    }

    let error_message = last_error.map(|error| format!("Could not load your Liked Songs: {error}"));
    if let Some(message) = &error_message {
        if tracks.is_empty() {
            let _ = events.send(PlaybackEvent::LikedSongsError {
                message: message.clone(),
            });
            return;
        }
        log::debug!("Liked Songs pagination stopped early: {message}");
    }
    let _ = events.send(PlaybackEvent::LikedSongs { tracks });
    if let Some(message) = error_message {
        let _ = events.send(PlaybackEvent::LikedSongsError { message });
    }
}

struct SongForLyrics<'a> {
    track_id: &'a str,
    artist: &'a str,
    title: &'a str,
    album: &'a str,
    duration_ms: u32,
}

/// Looks lyrics up in the chosen source, or in each source in turn for Auto.
async fn find_lyrics(
    http: &reqwest::Client,
    session: &Session,
    source: LyricsSource,
    song: &SongForLyrics<'_>,
) -> Result<FoundLyrics, String> {
    if source != LyricsSource::Auto {
        return lyrics_from(http, session, source, song).await;
    }
    let mut results = Vec::new();
    for source in LyricsSource::AUTO_ORDER {
        let result = lyrics_from(http, session, source, song).await;
        let synced = matches!(&result, Ok(found) if found.synced);
        results.push(result);
        if synced {
            break;
        }
    }
    lyrics::choose_auto(results)
}

async fn lyrics_from(
    http: &reqwest::Client,
    session: &Session,
    source: LyricsSource,
    song: &SongForLyrics<'_>,
) -> Result<FoundLyrics, String> {
    match source {
        LyricsSource::Spotify => {
            let id = SpotifyId::from_base62(song.track_id)
                .map_err(|error| format!("Invalid Spotify track ID: {error}"))?;
            // Spotify answers 404 for songs it has no lyrics for.
            let body = session
                .spclient()
                .get_lyrics(&id)
                .await
                .map_err(|_| "Spotify has no lyrics for this track.".to_owned())?;
            lyrics::parse_spotify(&body).ok_or_else(|| "Spotify has no lyrics for this track.".to_owned())
        }
        LyricsSource::Lrclib => {
            lyrics::lrclib(http, song.artist, song.title, song.album, song.duration_ms).await
        }
        LyricsSource::NetEase => lyrics::netease(http, song.artist, song.title, song.duration_ms).await,
        LyricsSource::Auto => Err("Auto is not a single source.".to_owned()),
    }
}

#[derive(Debug, Deserialize)]
struct AlbumSearchResponse {
    albums: AlbumPage,
}

#[derive(Debug, Deserialize)]
struct AlbumPage {
    #[serde(default)]
    items: Vec<SearchAlbum>,
}

#[derive(Debug, Deserialize)]
struct AlbumResponse {
    id: String,
    name: String,
    #[serde(default)]
    artists: Vec<SearchArtist>,
    #[serde(default)]
    images: Vec<ApiImage>,
    #[serde(default)]
    release_date: Option<String>,
    tracks: AlbumTrackPage,
}

#[derive(Debug, Deserialize)]
struct AlbumTrackPage {
    #[serde(default)]
    items: Vec<AlbumTrack>,
    #[serde(default)]
    next: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AlbumTrack {
    #[serde(default)]
    id: Option<String>,
    name: String,
    #[serde(default)]
    duration_ms: u32,
    #[serde(default)]
    artists: Vec<SearchArtist>,
}

#[derive(Debug, Deserialize)]
struct RecentlyPlayedResponse {
    #[serde(default)]
    items: Vec<SavedTrackItem>,
}

#[derive(Debug, Deserialize)]
struct TopArtistItems {
    #[serde(default)]
    items: Vec<SearchArtist>,
}

fn album_tracks_from_response(album: AlbumResponse, extra: Vec<AlbumTrack>) -> (SpotifyAlbum, Vec<SpotifyTrack>) {
    let image_url = select_image(&album.images);
    let album_id = album.id.clone();
    let album_name = album.name.clone();
    let tracks = album
        .tracks
        .items
        .into_iter()
        .chain(extra)
        .filter_map(|track| {
            Some(SpotifyTrack {
                id: track.id?,
                title: track.name,
                artist: join_artists(&track.artists),
                artist_id: track.artists.first().and_then(|artist| artist.id.clone()),
                album: album_name.clone(),
                album_id: Some(album_id.clone()),
                duration_ms: track.duration_ms,
                image_url: image_url.clone(),
            })
        })
        .collect();
    let summary = SearchAlbum {
        id: Some(album.id),
        name: album.name,
        images: album.images,
        artists: album.artists,
        release_date: album.release_date,
    };
    let summary = summary.into_album().expect("album has an id");
    (summary, tracks)
}

/// Spotify's top-items endpoints have accepted up to 50 results; ask for fewer
/// if the account's API tier refuses that.
async fn get_top<T: DeserializeOwned>(client: &WebApiClient, kind: &str, range: TopRange) -> Result<T, String> {
    let url = format!("https://api.spotify.com/v1/me/top/{kind}");
    let query = |limit: &str| {
        vec![
            ("limit".to_owned(), limit.to_owned()),
            ("time_range".to_owned(), range.api_value().to_owned()),
        ]
    };
    match client.get_json(&url, &query("30")).await {
        Ok(value) => Ok(value),
        Err(error) if error.contains("400") => client.get_json(&url, &query("10")).await,
        Err(error) => Err(error),
    }
}

async fn load_top(client: &WebApiClient, range: TopRange, events: &Sender<PlaybackEvent>) {
    let tracks = get_top::<TopTracksResponse>(client, "tracks", range).await;
    let artists = get_top::<TopArtistItems>(client, "artists", range).await;
    let error = match (&tracks, &artists) {
        (Err(error), _) | (_, Err(error)) => Some(error.clone()),
        _ => None,
    };
    let tracks = tracks
        .map(|response| response.items.into_iter().map(map_search_track).collect())
        .unwrap_or_default();
    let artists = artists
        .map(|response| {
            response
                .items
                .into_iter()
                .filter_map(|artist| {
                    Some(SpotifyArtist {
                        image_url: select_image(&artist.images),
                        id: artist.id?,
                        name: artist.name,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let _ = events.send(PlaybackEvent::TopItems {
        range,
        tracks,
        artists,
        error,
    });
}

async fn load_recently_played(client: &WebApiClient, events: &Sender<PlaybackEvent>) {
    let result = client
        .get_json::<RecentlyPlayedResponse>(
            "https://api.spotify.com/v1/me/player/recently-played",
            &[("limit".to_owned(), "50".to_owned())],
        )
        .await;
    let event = match result {
        Ok(response) => {
            let mut seen = std::collections::HashSet::new();
            let tracks = response
                .items
                .into_iter()
                .filter_map(|item| item.track)
                .filter(|track| seen.insert(track.id.clone()))
                .map(map_search_track)
                .collect();
            PlaybackEvent::RecentlyPlayed { tracks, error: None }
        }
        Err(error) => PlaybackEvent::RecentlyPlayed {
            tracks: Vec::new(),
            error: Some(if error.contains("403") {
                MISSING_PERMISSION.to_owned()
            } else {
                error
            }),
        },
    };
    let _ = events.send(event);
}

async fn load_album(client: &WebApiClient, album_id: &str, events: &Sender<PlaybackEvent>) {
    let url = format!("https://api.spotify.com/v1/albums/{album_id}");
    let album = match client.get_json::<AlbumResponse>(&url, &[]).await {
        Ok(album) => album,
        Err(error) => {
            let _ = events.send(PlaybackEvent::AlbumPage {
                album_id: album_id.to_owned(),
                name: String::new(),
                artist: String::new(),
                artist_id: None,
                year: None,
                image_url: None,
                tracks: Vec::new(),
                error: Some(format!("Could not load the album: {error}")),
            });
            return;
        }
    };
    // Long albums list their tracks over several pages.
    let mut extra = Vec::new();
    let mut next = album.tracks.next.clone();
    let mut pages = 0;
    while let Some(url) = next.take() {
        if pages >= 10 {
            break;
        }
        match client.get_json::<AlbumTrackPage>(&url, &[]).await {
            Ok(page) => {
                extra.extend(page.items);
                next = page.next;
                pages += 1;
            }
            Err(error) => {
                log::debug!("Album track pagination stopped early: {error}");
                break;
            }
        }
    }
    let artist_id = album.artists.first().and_then(|artist| artist.id.clone());
    let (summary, tracks) = album_tracks_from_response(album, extra);
    let _ = events.send(PlaybackEvent::AlbumPage {
        album_id: summary.id,
        name: summary.name,
        artist: summary.artist,
        artist_id,
        year: summary.year,
        image_url: summary.image_url,
        tracks,
        error: None,
    });
}

/// Builds an artist page. Spotify's own artist metadata (through Librespot)
/// still has an artist's top tracks and albums, which the Web API stopped
/// giving to apps in development mode in February 2026; the Web API's search
/// fills in whatever that misses.
async fn load_artist(
    client: &WebApiClient,
    session: &Session,
    artist_id: &str,
    name: &str,
    events: &Sender<PlaybackEvent>,
) {
    let mut name = name.to_owned();
    let mut image_url = None;
    let mut tracks = Vec::new();
    let mut albums = Vec::new();

    let metadata = match SpotifyId::from_base62(artist_id) {
        Ok(id) => ArtistMetadata::get(session, &SpotifyUri::Artist { id })
            .await
            .map_err(|error| log::debug!("Spotify artist metadata failed: {error}"))
            .ok(),
        Err(_) => None,
    };
    if let Some(artist) = &metadata {
        if !artist.name.trim().is_empty() {
            name = artist.name.clone();
        }
        image_url = metadata_image_url(&artist.portraits)
            .or_else(|| metadata_image_url(&artist.portrait_group));
        let top = artist.top_tracks.for_country(&session.country());
        let top_ids = top.iter().take(10).cloned().collect::<Vec<_>>();
        tracks = tracks_from_metadata(session, top_ids).await;
        let album_uris = artist
            .albums_current()
            .chain(artist.singles_current())
            .take(18)
            .cloned()
            .collect::<Vec<_>>();
        albums = albums_from_metadata(session, album_uris).await;
    }

    if image_url.is_none() || name.trim().is_empty() {
        let url = format!("https://api.spotify.com/v1/artists/{artist_id}");
        if let Ok(artist) = client.get_json::<SearchArtist>(&url, &[]).await {
            image_url = image_url.or_else(|| select_image(&artist.images));
            if name.trim().is_empty() {
                name = artist.name;
            }
        }
    }
    let quoted = name.replace('"', "");
    if tracks.is_empty() && !quoted.trim().is_empty() {
        let params = vec![
            ("q".to_owned(), format!("artist:\"{quoted}\"")),
            ("type".to_owned(), "track".to_owned()),
            ("limit".to_owned(), "10".to_owned()),
        ];
        if let Ok(response) = client
            .get_json::<SearchResponse>("https://api.spotify.com/v1/search", &params)
            .await
        {
            tracks = response
                .tracks
                .items
                .into_iter()
                .filter(|track| {
                    track
                        .artists
                        .iter()
                        .any(|artist| artist.id.as_deref() == Some(artist_id))
                })
                .map(map_search_track)
                .collect();
        }
    }
    if albums.is_empty() {
        let url = format!("https://api.spotify.com/v1/artists/{artist_id}/albums");
        let params = vec![
            ("include_groups".to_owned(), "album,single".to_owned()),
            ("limit".to_owned(), "10".to_owned()),
        ];
        albums = match client.get_json::<AlbumPage>(&url, &params).await {
            Ok(page) => page.items.into_iter().filter_map(SearchAlbum::into_album).collect(),
            Err(_) if !quoted.trim().is_empty() => {
                let params = vec![
                    ("q".to_owned(), format!("artist:\"{quoted}\"")),
                    ("type".to_owned(), "album".to_owned()),
                    ("limit".to_owned(), "10".to_owned()),
                ];
                client
                    .get_json::<AlbumSearchResponse>("https://api.spotify.com/v1/search", &params)
                    .await
                    .map(|response| {
                        response
                            .albums
                            .items
                            .into_iter()
                            .filter(|album| {
                                album
                                    .artists
                                    .iter()
                                    .any(|artist| artist.id.as_deref() == Some(artist_id))
                            })
                            .filter_map(SearchAlbum::into_album)
                            .collect()
                    })
                    .unwrap_or_default()
            }
            Err(_) => Vec::new(),
        };
    }

    let error = (tracks.is_empty() && albums.is_empty())
        .then(|| "Spotify did not return any songs or albums for this artist.".to_owned());
    let _ = events.send(PlaybackEvent::ArtistPage {
        artist_id: artist_id.to_owned(),
        name,
        image_url,
        tracks,
        albums,
        error,
    });
}

/// Starts a radio from a song, the way Spotify's own apps do: Spotify makes a
/// playlist of related songs for the seed, which Oynx then resolves.
async fn song_radio(session: &Session, track_id: &str) -> Result<Vec<SpotifyTrack>, String> {
    let id = SpotifyId::from_base62(track_id)
        .map_err(|error| format!("Invalid Spotify track ID: {error}"))?;
    let seed = SpotifyUri::Track { id };
    let response = session
        .spclient()
        .get_radio_for_track(&seed)
        .await
        .map_err(|error| format!("Spotify could not start a radio for this song: {error}"))?;
    let value: serde_json::Value = serde_json::from_slice(&response)
        .map_err(|error| format!("Could not read Spotify's radio response: {error}"))?;
    let playlist_uri = value
        .get("mediaItems")
        .and_then(|items| items.as_array())
        .and_then(|items| items.first())
        .and_then(|item| item.get("uri"))
        .and_then(|uri| uri.as_str())
        .ok_or_else(|| "Spotify has no radio for this song yet.".to_owned())?;
    let context = session
        .spclient()
        .get_context(playlist_uri)
        .await
        .map_err(|error| format!("Could not load the song radio: {error}"))?;
    let mut uris = context
        .pages
        .iter()
        .flat_map(|page| page.tracks.iter())
        .filter_map(|track| track.uri.clone())
        .collect::<Vec<_>>();
    if uris.is_empty() {
        // Some contexts only point at their first page; load it.
        let page_url = context
            .pages
            .iter()
            .find_map(|page| page.page_url.clone().or_else(|| page.next_page_url.clone()));
        if let Some(page_url) = page_url
            && let Ok(page) = session.spclient().get_next_page(&page_url).await
            && let Ok(page) = serde_json::from_slice::<serde_json::Value>(&page)
        {
            uris = page
                .get("tracks")
                .and_then(|tracks| tracks.as_array())
                .into_iter()
                .flatten()
                .filter_map(|track| track.get("uri").and_then(|uri| uri.as_str()))
                .map(str::to_owned)
                .collect();
        }
    }
    let mut ids = vec![seed];
    for uri in uris {
        if let Ok(uri @ SpotifyUri::Track { .. }) = SpotifyUri::from_uri(&uri)
            && !ids.contains(&uri)
        {
            ids.push(uri);
        }
        if ids.len() >= 50 {
            break;
        }
    }
    Ok(tracks_from_metadata(session, ids).await)
}

async fn track_metadata(session: &Session, track_id: &str) -> Option<TrackMetadata> {
    let id = SpotifyId::from_base62(track_id).ok()?;
    TrackMetadata::get(session, &SpotifyUri::Track { id })
        .await
        .map_err(|error| log::debug!("Spotify track metadata failed: {error}"))
        .ok()
}

/// Fetches Spotify's metadata for several tracks at once, keeping their order.
async fn tracks_from_metadata(session: &Session, uris: Vec<SpotifyUri>) -> Vec<SpotifyTrack> {
    let permits = Arc::new(Semaphore::new(8));
    let mut jobs = Vec::new();
    for uri in uris {
        let session = session.clone();
        let permits = permits.clone();
        jobs.push(tokio::spawn(async move {
            let _permit = permits.acquire_owned().await.ok()?;
            let track = TrackMetadata::get(&session, &uri).await.ok()?;
            spotify_track_from_metadata(&track)
        }));
    }
    let mut tracks = Vec::new();
    for job in jobs {
        if let Ok(Some(track)) = job.await {
            tracks.push(track);
        }
    }
    tracks
}

async fn albums_from_metadata(session: &Session, uris: Vec<SpotifyUri>) -> Vec<SpotifyAlbum> {
    let permits = Arc::new(Semaphore::new(8));
    let mut jobs = Vec::new();
    for uri in uris {
        let session = session.clone();
        let permits = permits.clone();
        jobs.push(tokio::spawn(async move {
            let _permit = permits.acquire_owned().await.ok()?;
            let album = AlbumMetadata::get(&session, &uri).await.ok()?;
            Some(SpotifyAlbum {
                id: album.id.to_id().ok()?,
                name: album.name.clone(),
                artist: album
                    .artists
                    .iter()
                    .map(|artist| artist.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                year: Some(album.date.0.year()).filter(|year| *year > 1000),
                image_url: metadata_image_url(&album.covers)
                    .or_else(|| metadata_image_url(&album.cover_group)),
            })
        }));
    }
    let mut albums = Vec::new();
    for job in jobs {
        if let Ok(Some(album)) = job.await {
            albums.push(album);
        }
    }
    albums
}

fn spotify_track_from_metadata(track: &TrackMetadata) -> Option<SpotifyTrack> {
    let id = track.id.to_id().ok()?;
    if track.name.trim().is_empty() {
        return None;
    }
    Some(SpotifyTrack {
        id,
        title: track.name.clone(),
        artist: track
            .artists
            .iter()
            .map(|artist| artist.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        artist_id: track.artists.first().and_then(|artist| artist.id.to_id().ok()),
        album: track.album.name.clone(),
        album_id: track.album.id.to_id().ok(),
        duration_ms: u32::try_from(track.duration).unwrap_or_default(),
        image_url: metadata_image_url(&track.album.covers)
            .or_else(|| metadata_image_url(&track.album.cover_group)),
    })
}

/// Picks a cover of about 300 pixels from Spotify's image metadata.
fn metadata_image_url(images: &librespot::metadata::image::Images) -> Option<String> {
    use librespot::metadata::image::ImageSize;
    let image = images
        .iter()
        .find(|image| image.size == ImageSize::DEFAULT)
        .or_else(|| images.iter().find(|image| image.size == ImageSize::LARGE))
        .or_else(|| images.first())?;
    let id = image.id.to_base16().ok()?;
    Some(format!("https://i.scdn.co/image/{id}"))
}

fn spotify_role_label(role: ArtistRole) -> Option<&'static str> {
    match role {
        ArtistRole::ARTIST_ROLE_FEATURED_ARTIST => Some("Featuring"),
        ArtistRole::ARTIST_ROLE_REMIXER => Some("Remixed by"),
        ArtistRole::ARTIST_ROLE_COMPOSER => Some("Composed by"),
        ArtistRole::ARTIST_ROLE_CONDUCTOR => Some("Conducted by"),
        ArtistRole::ARTIST_ROLE_ORCHESTRA => Some("Performed by"),
        _ => None,
    }
}

/// Puts a track after the current one, or at the end of the queue. With
/// nothing queued, the track simply starts playing.
fn enqueue(player: &mut ConnectedPlayer, track_id: String, next: bool, events: &Sender<PlaybackEvent>) {
    let Some(current) = player.current_index.filter(|index| *index < player.queue.len()) else {
        player.queue = vec![track_id.clone()];
        player.original_queue = player.queue.clone();
        player.current_index = Some(0);
        if let Err(error) = load_track(player, &track_id, true, 0) {
            let _ = events.send(PlaybackEvent::Error(error));
        }
        return;
    };
    let current_id = player.queue[current].clone();
    if next {
        player.queue.insert(current + 1, track_id.clone());
        let position = player
            .original_queue
            .iter()
            .position(|id| *id == current_id)
            .map_or(player.original_queue.len(), |index| index + 1);
        player.original_queue.insert(position, track_id);
    } else {
        player.queue.push(track_id.clone());
        player.original_queue.push(track_id);
    }
}

fn shuffle_queue(queue: Vec<String>, current_id: Option<&str>) -> Vec<String> {
    if queue.len() < 2 {
        return queue;
    }

    let mut seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or_default()
        ^ queue.len() as u64;
    let mut shuffled = queue;
    for index in (1..shuffled.len()).rev() {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let swap_index = ((seed >> 32) as usize) % (index + 1);
        shuffled.swap(index, swap_index);
    }

    if let Some(current_id) = current_id
        && let Some(current_index) = shuffled.iter().position(|id| id == current_id)
    {
        let mut index = 0;
        while index < current_index {
            shuffled.rotate_left(1);
            index += 1;
        }
    }
    shuffled
}

fn set_shuffle(player: &mut ConnectedPlayer, enabled: bool) {
    if player.shuffle == enabled {
        return;
    }
    player.shuffle = enabled;
    let current_id = player
        .current_index
        .and_then(|index| player.queue.get(index))
        .cloned();
    player.queue = if enabled {
        shuffle_queue(player.original_queue.clone(), current_id.as_deref())
    } else {
        player.original_queue.clone()
    };
    player.current_index = current_id
        .as_ref()
        .and_then(|id| player.queue.iter().position(|queued_id| queued_id == id));
}

fn move_in_queue(
    player: &mut ConnectedPlayer,
    direction: i8,
    from_track_end: bool,
    events: &Sender<PlaybackEvent>,
) {
    if player.queue.is_empty() {
        return;
    }

    if from_track_end && direction > 0 && player.repeat == RepeatMode::One {
        let Some(track_id) = player
            .current_index
            .and_then(|index| player.queue.get(index))
        else {
            return;
        };
        let track_id = track_id.clone();
        if let Err(error) = load_track(player, &track_id, true, 0) {
            let _ = events.send(PlaybackEvent::Error(error));
        }
        return;
    }

    let length = player.queue.len();
    let current = player.current_index;
    let next = match (direction, current) {
        (1, Some(index)) if index + 1 >= length => match player.repeat {
            RepeatMode::All => 0,
            RepeatMode::Off | RepeatMode::One => {
                player.player.pause();
                let _ = events.send(PlaybackEvent::QueueEnded);
                return;
            }
        },
        (1, Some(index)) => index + 1,
        (1, None) => 0,
        (-1, Some(0)) => match player.repeat {
            RepeatMode::All => player.queue.len() - 1,
            RepeatMode::Off | RepeatMode::One => {
                player.player.pause();
                let _ = events.send(PlaybackEvent::QueueEnded);
                return;
            }
        },
        (-1, Some(index)) => index - 1,
        (-1, None) => player.queue.len() - 1,
        _ => return,
    };

    player.current_index = Some(next);
    let track_id = player.queue[next].clone();
    if let Err(error) = load_track(player, &track_id, true, 0) {
        let _ = events.send(PlaybackEvent::Error(error));
    }
}

fn load_track(
    player: &mut ConnectedPlayer,
    track_id: &str,
    start_playing: bool,
    start_position_ms: u32,
) -> Result<(), String> {
    let spotify_id = SpotifyId::from_base62(track_id)
        .map_err(|error| format!("Invalid Spotify track ID: {error}"))?;
    player.player.load(
        SpotifyUri::Track { id: spotify_id },
        start_playing,
        start_position_ms,
    );
    Ok(())
}

fn forward_player_event(event: PlayerEvent, events: &Sender<PlaybackEvent>) {
    match event {
        PlayerEvent::Playing {
            track_id,
            position_ms,
            ..
        } => {
            let _ = events.send(PlaybackEvent::Position {
                track_id: track_id.to_id().unwrap_or_default(),
                position_ms,
                playing: true,
            });
        }
        PlayerEvent::Paused {
            track_id,
            position_ms,
            ..
        } => {
            let _ = events.send(PlaybackEvent::Position {
                track_id: track_id.to_id().unwrap_or_default(),
                position_ms,
                playing: false,
            });
        }
        PlayerEvent::PositionChanged {
            position_ms,
            track_id,
            ..
        } => {
            let _ = events.send(PlaybackEvent::Position {
                track_id: track_id.to_id().unwrap_or_default(),
                position_ms,
                playing: true,
            });
        }
        PlayerEvent::TrackChanged { audio_item } => {
            let _ = events.send(PlaybackEvent::TrackChanged {
                track_id: audio_item.track_id.to_id().unwrap_or_default(),
                title: audio_item.name,
                duration_ms: audio_item.duration_ms,
            });
        }
        PlayerEvent::Unavailable { track_id, .. } => {
            let _ = events.send(PlaybackEvent::Error(format!(
                "Spotify could not play track {}.",
                track_id.to_id().unwrap_or_default()
            )));
        }
        PlayerEvent::EndOfTrack { .. } => {
            let _ = events.send(PlaybackEvent::Ended);
        }
        PlayerEvent::SessionDisconnected { .. } => {
            let _ = events.send(PlaybackEvent::State(ConnectionState::Disconnected));
            let _ = events.send(PlaybackEvent::Error(
                "The Spotify session disconnected.".to_owned(),
            ));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_token_credentials_are_distinguished_from_password_credentials() {
        let token_credentials = Credentials::with_access_token("access-token");
        assert!(token_from_cached_credentials(&token_credentials).is_some());

        let password_credentials = Credentials::with_password("user", "password");
        assert!(token_from_cached_credentials(&password_credentials).is_none());
    }

    #[test]
    fn playlist_response_deserializes_the_fields_used_by_the_ui() {
        let response: PlaylistResponse = serde_json::from_str(
            r#"{
                "items": [{
                    "id": "playlist-id",
                    "name": "Late night",
                    "description": "A mix",
                    "tracks": { "total": 42 },
                    "owner": { "display_name": "Alex" }
                }]
            }"#,
        )
        .expect("playlist response should deserialize");
        assert_eq!(response.items.len(), 1);
        assert_eq!(response.items[0].track_count(), 42);
        assert_eq!(
            response.items[0]
                .owner
                .as_ref()
                .and_then(|owner| owner.display_name.as_deref()),
            Some("Alex")
        );
    }

    #[test]
    fn playlist_response_accepts_items_instead_of_tracks() {
        let response: PlaylistResponse = serde_json::from_str(
            r#"{
                "items": [{
                    "id": "playlist-id",
                    "name": "Late night",
                    "items": { "total": 17 },
                    "owner": { "display_name": "Alex" }
                }]
            }"#,
        )
        .expect("playlist response with items should deserialize");
        assert_eq!(response.items[0].track_count(), 17);
    }

    #[test]
    fn saved_tracks_response_deserializes_spotify_liked_songs() {
        let response: SavedTracksResponse = serde_json::from_str(
            r#"{
                "items": [
                    {
                        "track": {
                            "id": "track-id",
                            "name": "A saved song",
                            "duration_ms": 123000,
                            "artists": [{ "name": "An Artist" }],
                            "album": { "name": "An Album" }
                        }
                    },
                    { "track": null }
                ],
                "next": null
            }"#,
        )
        .expect("saved tracks response should deserialize");
        assert_eq!(response.items.len(), 2);
        assert_eq!(
            response.items[0]
                .track
                .as_ref()
                .map(|track| track.id.as_str()),
            Some("track-id")
        );
        assert!(response.items[1].track.is_none());
    }

    #[test]
    fn album_response_gives_every_track_the_album_cover_and_ids() {
        let album: AlbumResponse = serde_json::from_value(serde_json::json!({
            "id": "album-1",
            "name": "An Album",
            "release_date": "2019-11-29",
            "artists": [{"id": "artist-1", "name": "Artist"}],
            "images": [{"url": "https://i.scdn.co/image/cover", "width": 300, "height": 300}],
            "tracks": {
                "items": [
                    {"id": "track-1", "name": "One", "duration_ms": 1000,
                     "artists": [{"id": "artist-1", "name": "Artist"}, {"id": "artist-2", "name": "Guest"}]},
                    {"id": null, "name": "Unavailable", "duration_ms": 1000, "artists": []}
                ],
                "next": "https://api.spotify.com/v1/albums/album-1/tracks?offset=50"
            }
        }))
        .expect("album response should deserialize");
        let extra = vec![AlbumTrack {
            id: Some("track-2".to_owned()),
            name: "Two".to_owned(),
            duration_ms: 2000,
            artists: Vec::new(),
        }];
        let (summary, tracks) = album_tracks_from_response(album, extra);
        assert_eq!(summary.year, Some(2019));
        assert_eq!(summary.artist, "Artist");
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].artist, "Artist, Guest");
        assert_eq!(tracks[0].artist_id.as_deref(), Some("artist-1"));
        assert_eq!(tracks[1].album_id.as_deref(), Some("album-1"));
        assert_eq!(
            tracks[1].image_url.as_deref(),
            Some("https://i.scdn.co/image/cover")
        );
    }

    #[test]
    fn search_tracks_keep_artist_and_album_ids() {
        let track: SearchTrack = serde_json::from_value(serde_json::json!({
            "id": "track-1",
            "name": "Song",
            "duration_ms": 1000,
            "artists": [{"id": "artist-1", "name": "Artist"}],
            "album": {"id": "album-1", "name": "Album", "images": []}
        }))
        .expect("track should deserialize");
        let track = map_search_track(track);
        assert_eq!(track.artist_id.as_deref(), Some("artist-1"));
        assert_eq!(track.album_id.as_deref(), Some("album-1"));
    }

    #[test]
    fn optional_scopes_are_requested_but_not_required() {
        let requested = requested_web_api_scopes();
        assert!(requested.contains(&"user-read-recently-played"));
        assert!(requested.contains(&"playlist-modify-private"));
        let old_grant = WEB_API_SCOPES.iter().map(|scope| (*scope).to_owned()).collect::<Vec<_>>();
        assert!(has_scopes(&old_grant, WEB_API_SCOPES));
    }

    #[test]
    fn repeat_mode_cycles_through_all_states() {
        assert_eq!(RepeatMode::Off.next(), RepeatMode::All);
        assert_eq!(RepeatMode::All.next(), RepeatMode::One);
        assert_eq!(RepeatMode::One.next(), RepeatMode::Off);
    }

    #[test]
    fn page_response_preserves_next_links_and_totals() {
        let response: PageResponse = serde_json::from_str(
            r#"{
                "items": [{ "id": "first" }, { "id": "second" }],
                "next": "https://api.spotify.com/v1/me/playlists?offset=2",
                "total": 4
            }"#,
        )
        .expect("paged response should deserialize");
        assert_eq!(response.items.unwrap().len(), 2);
        assert!(response.next.is_some());
        assert_eq!(response.total, Some(4));
    }

    #[test]
    fn playlist_track_response_accepts_item_and_track_shapes() {
        let track = r#"{
            "id": "track-id",
            "name": "Track",
            "duration_ms": 1000,
            "artists": [{ "name": "Artist" }],
            "album": { "name": "Album" }
        }"#;
        let from_item: PlaylistItem = serde_json::from_str(&format!(r#"{{ "item": {track} }}"#))
            .expect("item-shaped playlist row should deserialize");
        let from_track: PlaylistItem = serde_json::from_str(&format!(r#"{{ "track": {track} }}"#))
            .expect("track-shaped playlist row should deserialize");
        assert_eq!(from_item.item.unwrap().id, "track-id");
        assert_eq!(from_track.track.unwrap().id, "track-id");
    }

    #[test]
    fn lrc_parser_handles_metadata_and_repeated_timestamps() {
        let lines = parse_lrc("[ar:Artist][00:01.50][00:03.00]Hello\n[00:05.00]World");
        assert_eq!(
            lines,
            vec![
                LyricLine {
                    timestamp_ms: 1_500,
                    text: "Hello".to_owned(),
                },
                LyricLine {
                    timestamp_ms: 3_000,
                    text: "Hello".to_owned(),
                },
                LyricLine {
                    timestamp_ms: 5_000,
                    text: "World".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn stored_web_token_round_trips_when_it_has_not_expired() {
        let token = OAuthToken {
            access_token: "access".to_owned(),
            refresh_token: "refresh".to_owned(),
            expires_at: Instant::now() + Duration::from_secs(120),
            token_type: "Bearer".to_owned(),
            scopes: vec!["playlist-read-private".to_owned()],
        };
        let stored = StoredOAuthToken::from_token("client", "redirect", &token);
        let restored = stored.into_token().expect("token should still be valid");
        assert_eq!(restored.access_token, "access");
        assert_eq!(restored.refresh_token, "refresh");
        assert_eq!(restored.scopes, vec!["playlist-read-private"]);
    }
}

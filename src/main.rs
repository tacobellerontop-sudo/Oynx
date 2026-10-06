// Release builds link against the Windows GUI subsystem so launching the app
// never opens a console window behind it. Debug builds keep the console so
// `cargo run` still shows env_logger output.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod credits;
mod lyrics;
mod spotify;
mod theme;
mod tray;
mod updater;

use eframe::egui;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    time::{Duration, Instant},
};

use crate::credits::{SongCredits, SongLink};
use crate::lyrics::LyricsSource;
use crate::spotify::{
    ConnectionState, LyricLine, PlaybackEvent, RepeatMode, SpotifyAlbum, SpotifyArtist,
    SpotifyClient, SpotifyConfig, SpotifyPlaylist, SpotifyTrack, TopRange,
};
use crate::audio::{AudioTaps, EQ_FREQUENCIES_HZ, EqualizerPreset, EqualizerSettings, MAX_EQ_GAIN_DB, MIN_EQ_GAIN_DB, NUM_BANDS, NUM_EQ_BANDS};
use crate::theme::{ColorRole, ThemeMode, ThemeSettings, pal};
use crate::tray::TrayAction;
use crate::updater::{CURRENT_VERSION, UpdateMode, UpdateStatus, Updater};

const INTER_FONT: &[u8] = include_bytes!("../assets/fonts/InterVariable.ttf");
const FAMILY_MEDIUM: &str = "Inter Medium";
const FAMILY_BOLD: &str = "Inter Bold";
const FAMILY_LIGHT: &str = "Inter Light";
const CUSTOM_FONT: &str = "Custom";

const VINYL: egui::Color32 = egui::Color32::from_rgb(13, 13, 14);

const RADIUS_XS: u8 = 4;
const RADIUS_SM: u8 = 6;
const RADIUS_MD: u8 = 8;
const RADIUS_LG: u8 = 12;
const SIDEBAR_WIDTH: f32 = 244.0;
const QUEUE_PANEL_WIDTH: f32 = 360.0;
const PLAYER_HEIGHT: f32 = 150.0;
const TITLE_BAR_HEIGHT: f32 = 60.0;
const BOTTOM_STRIP_HEIGHT: f32 = 50.0;
/// Left inset of page content inside the central column.
const CONTENT_INSET: f32 = 64.0;
const TRACK_ROW_HEIGHT: f32 = 56.0;
const SIDEBAR_WIDTH_RANGE: std::ops::RangeInclusive<f32> = 200.0..=380.0;
const QUEUE_WIDTH_RANGE: std::ops::RangeInclusive<f32> = 280.0..=520.0;
const PLAYER_HEIGHT_RANGE: std::ops::RangeInclusive<f32> = 150.0..=300.0;
/// The centre column never gets narrower than this; side panels give way first.
const MIN_CENTRAL_WIDTH: f32 = 440.0;
/// Content always keeps at least this much height above the player.
const MIN_CONTENT_HEIGHT: f32 = 200.0;
/// eframe storage key for [`UiPrefs`].
const UI_PREFS_KEY: &str = "oynx_ui_prefs";
/// How often automatic updates look for a new release.
const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
/// Record spin speed while playing, in radians per second (about 18 rpm).
const DISC_SPEED: f32 = std::f32::consts::TAU * 0.3;
/// Frame interval while the record is spinning (about 30 fps).
const DISC_FRAME_INTERVAL: Duration = Duration::from_millis(33);
/// How long lyrics stop following the song after the listener scrolls them.
const LYRICS_MANUAL_SCROLL_PAUSE: Duration = Duration::from_secs(5);
const MAX_PENDING_ARTWORK: usize = 64;
const ARTWORK_UPLOADS_PER_FRAME: usize = 4;
const MAX_EVENTS_PER_FRAME: usize = 128;
const ANIM_FAST: f32 = 0.12;
const ANIM_MEDIUM: f32 = 0.2;
const ANIM_SLOW: f32 = 0.32;
const PAGE_TRANSITION: f32 = 0.24;
const EQUALIZER_FRAME: Duration = Duration::from_millis(33);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Home,
    Search,
    Library,
    LikedSongs,
    Queue,
    Settings,
}

impl Section {
    fn label(self) -> &'static str {
        match self {
            Self::Home => "Home",
            Self::Search => "Search",
            Self::Library => "Library",
            Self::LikedSongs => "Liked",
            Self::Queue => "Queue",
            Self::Settings => "Settings",
        }
    }
}

/// User layout and update choices, saved through eframe's storage along with
/// the window's size and position.
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
struct UiPrefs {
    sidebar_width: f32,
    queue_width: f32,
    player_height: f32,
    update_mode: UpdateMode,
    /// Whether closing the window hides Oynx in the system tray instead of quitting.
    close_to_tray: bool,
    lyrics_source: LyricsSource,
}

impl Default for UiPrefs {
    fn default() -> Self {
        Self {
            sidebar_width: SIDEBAR_WIDTH,
            queue_width: QUEUE_PANEL_WIDTH,
            player_height: PLAYER_HEIGHT,
            update_mode: UpdateMode::default(),
            close_to_tray: true,
            lyrics_source: LyricsSource::Auto,
        }
    }
}

/// Section sizes that fit the current window. The saved preferences are not
/// changed, so a larger window (or monitor) gets the preferred sizes back.
#[derive(Clone, Copy)]
struct Layout {
    sidebar_width: f32,
    queue_width: f32,
    player_height: f32,
    /// Whether the queue panel fits beside the centre column.
    queue_fits: bool,
}

impl Layout {
    fn fit(prefs: &UiPrefs, window: egui::Vec2) -> Self {
        let clamp = |value: f32, range: &std::ops::RangeInclusive<f32>| value.clamp(*range.start(), *range.end());
        let sidebar_width = clamp(prefs.sidebar_width, &SIDEBAR_WIDTH_RANGE)
            .min(window.x - MIN_CENTRAL_WIDTH)
            .max(*SIDEBAR_WIDTH_RANGE.start());
        let queue_width = clamp(prefs.queue_width, &QUEUE_WIDTH_RANGE);
        let queue_fits = window.x - sidebar_width - queue_width >= MIN_CENTRAL_WIDTH;
        let body_height = window.y - TITLE_BAR_HEIGHT - BOTTOM_STRIP_HEIGHT;
        let player_height = clamp(prefs.player_height, &PLAYER_HEIGHT_RANGE)
            .min(body_height - MIN_CONTENT_HEIGHT)
            .max(*PLAYER_HEIGHT_RANGE.start());
        Self { sidebar_width, queue_width, player_height, queue_fits }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    General,
    Themes,
    Updates,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ThemeFile {
    BackgroundImage,
    Font,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RightPanelTab {
    Queue,
    Recent,
}

/// What the Now Playing view shows beside the waveform.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NowPlayingTab {
    Lyrics,
    Credits,
}

enum CreditsState {
    Loading,
    Ready(SongCredits),
    Failed(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SleepTimer {
    /// Pause when this moment arrives.
    At(Instant),
    /// Pause when the current song ends.
    EndOfTrack,
}

/// A page opened from a song or the library: its tracks live in `tracks`.
#[derive(Clone)]
enum DetailPage {
    Artist {
        id: String,
        name: String,
        image_url: Option<String>,
        albums: Vec<SpotifyAlbum>,
        loading: bool,
        error: Option<String>,
    },
    Album {
        id: String,
        name: String,
        artist: String,
        artist_id: Option<String>,
        year: Option<i32>,
        image_url: Option<String>,
        loading: bool,
        error: Option<String>,
    },
    Radio {
        seed_id: String,
        title: String,
        artist: String,
        image_url: Option<String>,
        loading: bool,
        error: Option<String>,
    },
    Top {
        range: TopRange,
        artists: Vec<SpotifyArtist>,
        loading: bool,
        error: Option<String>,
    },
}

impl DetailPage {
    /// Identifies the page for the page transition.
    fn key(&self) -> String {
        match self {
            Self::Artist { id, .. } => format!("artist:{id}"),
            Self::Album { id, .. } => format!("album:{id}"),
            Self::Radio { seed_id, .. } => format!("radio:{seed_id}"),
            Self::Top { range, .. } => format!("top:{range:?}"),
        }
    }

    fn source_label(&self) -> String {
        match self {
            Self::Artist { name, .. } => format!("Artist · {name}"),
            Self::Album { name, .. } => format!("Album · {name}"),
            Self::Radio { title, .. } => format!("Song radio · {title}"),
            Self::Top { range, .. } => format!("Your top songs · {}", range.label()),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Icon {
    Home,
    Search,
    Settings,
    Queue,
    Shuffle,
    Previous,
    Next,
    Play,
    Pause,
    Repeat,
    RepeatOne,
    Heart,
    HeartFilled,
    Close,
    Volume(u8),
    ChevronLeft,
    ChevronRight,
    Dots,
    Waveform,
    NowPlaying,
    Share,
    Sparkle,
    Minimize,
    Maximize,
    Restore,
    Note,
    Clock,
}

#[derive(Clone, Copy)]
struct Artwork {
    pattern: u8,
}

#[derive(Clone)]
struct Mix {
    title: String,
    subtitle: String,
    track_index: usize,
    artwork: Artwork,
}

#[derive(Clone)]
struct Track {
    title: String,
    artist: String,
    album: String,
    duration: String,
    artwork: Artwork,
    image_url: Option<String>,
    artist_id: Option<String>,
    album_id: Option<String>,
}

#[derive(Clone)]
struct Playlist {
    id: String,
    title: String,
    description: String,
    artwork: Artwork,
    image_url: Option<String>,
    owner_id: Option<String>,
    collaborative: bool,
}

#[derive(Clone, Serialize, Deserialize)]
struct SavedTrack {
    id: String,
    title: String,
    artist: String,
    album: String,
    duration_ms: u32,
    image_url: Option<String>,
    #[serde(default)]
    artist_id: Option<String>,
    #[serde(default)]
    album_id: Option<String>,
}

impl SavedTrack {
    fn from_track(track: &Track, id: String) -> Self {
        Self {
            id,
            title: track.title.clone(),
            artist: track.artist.clone(),
            album: track.album.clone(),
            duration_ms: track.duration_ms(),
            image_url: track.image_url.clone(),
            artist_id: track.artist_id.clone(),
            album_id: track.album_id.clone(),
        }
    }

    fn to_track(&self, index: usize) -> Track {
        Track {
            title: self.title.clone(),
            artist: self.artist.clone(),
            album: self.album.clone(),
            duration: format_duration(self.duration_ms),
            artwork: OynxApp::artwork_for_index(index),
            image_url: self.image_url.clone(),
            artist_id: self.artist_id.clone(),
            album_id: self.album_id.clone(),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct SavedSession {
    #[serde(default)]
    account: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    avatar_url: Option<String>,
    volume: f32,
    shuffle: bool,
    repeat: RepeatMode,
    position_ms: u32,
    queue_source: String,
    queue_selected: usize,
    queue: Vec<SavedTrack>,
}

impl Default for SavedSession {
    fn default() -> Self {
        Self {
            account: String::new(),
            display_name: String::new(),
            avatar_url: None,
            volume: 0.72,
            shuffle: false,
            repeat: RepeatMode::Off,
            position_ms: 0,
            queue_source: String::new(),
            queue_selected: 0,
            queue: Vec::new(),
        }
    }
}

impl SavedSession {
    fn path() -> std::path::PathBuf {
        SpotifyConfig::cache_path().join("session.json")
    }

    fn load() -> Self {
        let mut session: Self = std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|contents| serde_json::from_str(&contents).ok())
            .unwrap_or_default();
        if !session.volume.is_finite() {
            session.volume = 0.72;
        }
        session.volume = session.volume.clamp(0.0, 1.0);
        session.queue.truncate(2_000);
        if session.queue.is_empty() {
            session.position_ms = 0;
            session.queue_selected = 0;
        } else {
            session.queue_selected = session.queue_selected.min(session.queue.len() - 1);
        }
        session
    }

    fn save(&self) {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(contents) = serde_json::to_vec_pretty(self) {
            let temporary = path.with_extension("json.tmp");
            if std::fs::write(&temporary, contents).is_ok()
                && std::fs::rename(&temporary, &path).is_err()
            {
                let _ = std::fs::remove_file(&path);
                let _ = std::fs::rename(&temporary, &path);
            }
        }
    }
}

struct OynxApp {
    section: Section,
    queue_return_section: Section,
    search: String,
    mixes: Vec<Mix>,
    recommendations_loaded: bool,
    tracks: Vec<Track>,
    liked_songs: Vec<Track>,
    liked_spotify_ids: HashMap<usize, String>,
    liked_song_ids: HashSet<String>,
    liked_songs_loaded: bool,
    liked_songs_error: Option<String>,
    playlists: Vec<Playlist>,
    playlists_loaded: bool,
    playlist_name: Option<String>,
    playlist_loading: bool,
    playlist_error: Option<String>,
    selected_track: usize,
    liked_tracks: HashSet<usize>,
    playing: bool,
    progress: f32,
    volume: f32,
    shuffle: bool,
    repeat: RepeatMode,
    queue_active: bool,
    queue_tracks: Vec<Track>,
    queue_spotify_ids: HashMap<usize, String>,
    queue_selected: usize,
    queue_source: String,
    queue_panel_visible: bool,
    right_panel_tab: RightPanelTab,
    lyrics: Vec<LyricLine>,
    lyrics_synced: bool,
    lyrics_loading: bool,
    lyrics_error: Option<String>,
    /// Who provided the lyrics on screen, such as "LRCLIB".
    lyrics_provider: Option<String>,
    lyrics_track_id: Option<String>,
    position_ms: u32,
    artwork_textures: HashMap<String, egui::TextureHandle>,
    pending_artwork: HashSet<String>,
    artwork_retry_after: HashMap<String, Instant>,
    artwork_queue: VecDeque<(String, u32, u32, Vec<u8>)>,
    theme: ThemeSettings,
    /// The theme changed since it was last written to disk.
    theme_dirty: bool,
    /// The theme and dark/light choice the current palette and style were built from.
    applied_theme: Option<(ThemeSettings, bool)>,
    /// The font file the current font definitions were built from.
    applied_font: Option<Option<std::path::PathBuf>>,
    theme_message: Option<String>,
    /// An open file picker for the theme and where its answer arrives.
    theme_dialog: Option<(ThemeFile, std::sync::mpsc::Receiver<Option<std::path::PathBuf>>)>,
    background_texture: Option<egui::TextureHandle>,
    /// The image path and blur the background texture was made from.
    background_key: Option<(std::path::PathBuf, u32)>,
    background_load: Option<((std::path::PathBuf, u32), std::sync::mpsc::Receiver<Result<egui::ColorImage, String>>)>,
    /// Whether this window was created with a transparent surface; that can
    /// only be chosen at launch.
    transparent_window: bool,
    /// A see-through window was asked for but could not be opened on this PC.
    transparency_failed: bool,
    /// The window blur last applied to the native window.
    #[cfg_attr(not(windows), allow(dead_code))]
    blur_applied: Option<bool>,
    system_fonts: Vec<(&'static str, std::path::PathBuf)>,
    tray: Option<tray::Tray>,
    prefs: UiPrefs,
    settings_tab: SettingsTab,
    updater: Updater,
    started_at: Instant,
    /// Size of the monitor the window was last on, to notice moves between monitors.
    last_monitor_size: Option<egui::Vec2>,
    window_hidden: bool,
    quitting: bool,
    spotify: SpotifyClient,
    connection_state: ConnectionState,
    connection_error: Option<String>,
    account_name: String,
    /// Spotify display name; `account_name` holds the account ID used for session matching.
    display_name: Option<String>,
    avatar_url: Option<String>,
    disc_angle: f32,
    disc_last_frame: Option<Instant>,
    spotify_ids: HashMap<usize, String>,
    pending_playback: bool,
    pending_resume: bool,
    resume_position_ms: u32,
    last_session_save: Instant,
    spotify_client_id: String,
    spotify_redirect_uri: String,
    spotify_web_api_client_id: String,
    spotify_web_api_redirect_uri: String,
    settings_message: Option<String>,
    page_key: (Section, Option<String>),
    page_entered: Instant,
    audio: AudioTaps,
    /// UI copy of the equalizer curve; changes are pushed to `audio.equalizer`.
    eq_settings: EqualizerSettings,
    eq_unsaved: bool,
    /// Waveform tick levels shown on screen, easing towards `vis_targets`.
    vis_levels: Vec<f32>,
    vis_targets: Vec<f32>,
    vis_active: bool,
    vis_last_frame: Option<Instant>,
    queue_expanded: bool,
    /// Track and line the lyrics view last scrolled to.
    lyrics_follow: Option<(Option<String>, usize)>,
    /// While set, the listener is scrolling the lyrics and auto-follow is paused.
    lyrics_manual_until: Option<Instant>,
    /// An artist, album, radio or top-songs page open in the library.
    detail: Option<DetailPage>,
    now_playing_tab: NowPlayingTab,
    /// Song credits by Spotify track ID.
    credits: HashMap<String, CreditsState>,
    recent_tracks: Vec<Track>,
    recent_ids: Vec<String>,
    recent_loading: bool,
    recent_error: Option<String>,
    recent_loaded_at: Option<Instant>,
    sleep_timer: Option<SleepTimer>,
    /// A short confirmation, such as "Added to queue", and when it was shown.
    notice: Option<(String, Instant)>,
    /// Whether `tracks` holds the open detail page's songs yet.
    detail_has_tracks: bool,
}

impl OynxApp {
    fn new() -> Self {
        let mixes = vec![
            Mix {
                title: "Neon Afterglow".into(),
                subtitle: "Oynx Selects".into(),
                track_index: 0,
                artwork: Artwork {
                    pattern: 0,
                },
            },
            Mix {
                title: "Soft Focus".into(),
                subtitle: "A quieter kind of energy".into(),
                track_index: 1,
                artwork: Artwork {
                    pattern: 1,
                },
            },
            Mix {
                title: "Night Drive".into(),
                subtitle: "For the long way home".into(),
                track_index: 2,
                artwork: Artwork {
                    pattern: 2,
                },
            },
            Mix {
                title: "Sunday Static".into(),
                subtitle: "Warm noise, good company".into(),
                track_index: 3,
                artwork: Artwork {
                    pattern: 3,
                },
            },
            Mix {
                title: "Green Room".into(),
                subtitle: "A little bit of everywhere".into(),
                track_index: 4,
                artwork: Artwork {
                    pattern: 1,
                },
            },
            Mix {
                title: "Afterimage".into(),
                subtitle: "The songs that stay".into(),
                track_index: 5,
                artwork: Artwork {
                    pattern: 3,
                },
            },
        ];

        let tracks = vec![
            Track {
                title: "Blinding Lights".into(),
                artist: "The Weeknd".into(),
                album: "After Hours".into(),
                duration: "3:20".into(),
                artwork: Artwork {
                    pattern: 0,
                },
                image_url: None,
                artist_id: None,
                album_id: None,
            },
            Track {
                title: "Never Gonna Give You Up".into(),
                artist: "Rick Astley".into(),
                album: "Whenever You Need Somebody".into(),
                duration: "3:33".into(),
                artwork: Artwork {
                    pattern: 1,
                },
                image_url: None,
                artist_id: None,
                album_id: None,
            },
            Track {
                title: "All I Want".into(),
                artist: "Tania Bowra".into(),
                album: "Place In The Sun".into(),
                duration: "4:36".into(),
                artwork: Artwork {
                    pattern: 2,
                },
                image_url: None,
                artist_id: None,
                album_id: None,
            },
            Track {
                title: "Borderline".into(),
                artist: "Tame Impala".into(),
                album: "The Slow Rush".into(),
                duration: "3:57".into(),
                artwork: Artwork {
                    pattern: 3,
                },
                image_url: None,
                artist_id: None,
                album_id: None,
            },
            Track {
                title: "A New Error".into(),
                artist: "Moderat".into(),
                album: "II".into(),
                duration: "6:07".into(),
                artwork: Artwork {
                    pattern: 0,
                },
                image_url: None,
                artist_id: None,
                album_id: None,
            },
            Track {
                title: "Outro".into(),
                artist: "M83".into(),
                album: "Hurry Up, We're Dreaming".into(),
                duration: "4:07".into(),
                artwork: Artwork {
                    pattern: 1,
                },
                image_url: None,
                artist_id: None,
                album_id: None,
            },
            Track {
                title: "Billie Jean".into(),
                artist: "Michael Jackson".into(),
                album: "Thriller".into(),
                duration: "4:20".into(),
                artwork: Artwork {
                    pattern: 3,
                },
                image_url: None,
                artist_id: None,
                album_id: None,
            },
        ];

        let playlists = vec![
            Playlist {
                id: String::new(),
                title: "Late night coding".into(),
                description: "42 songs".into(),
                artwork: Artwork {
                    pattern: 2,
                },
                image_url: None,
                owner_id: None,
                collaborative: false,
            },
            Playlist {
                id: String::new(),
                title: "Windows at 2am".into(),
                description: "18 songs".into(),
                artwork: Artwork {
                    pattern: 0,
                },
                image_url: None,
                owner_id: None,
                collaborative: false,
            },
            Playlist {
                id: String::new(),
                title: "Focus / flow".into(),
                description: "31 songs".into(),
                artwork: Artwork {
                    pattern: 3,
                },
                image_url: None,
                owner_id: None,
                collaborative: false,
            },
            Playlist {
                id: String::new(),
                title: "New releases".into(),
                description: "12 songs".into(),
                artwork: Artwork {
                    pattern: 1,
                },
                image_url: None,
                owner_id: None,
                collaborative: false,
            },
        ];

        let spotify_config = SpotifyConfig::load();
        let saved_session = SavedSession::load();
        let eq_settings = EqualizerSettings::load(&equalizer_path());
        let audio = AudioTaps::new(eq_settings);
        let has_saved_queue = !saved_session.queue.is_empty();
        let restored_tracks = saved_session
            .queue
            .iter()
            .enumerate()
            .map(|(index, track)| track.to_track(index))
            .collect::<Vec<_>>();
        let mut restored_ids = HashMap::new();
        for (index, track) in saved_session.queue.iter().enumerate() {
            restored_ids.insert(index, track.id.clone());
        }
        let tracks = if has_saved_queue {
            restored_tracks
        } else {
            tracks
        };
        let spotify_ids = if has_saved_queue {
            restored_ids
        } else {
            HashMap::from([
                (0, "0VjIjW4GlUZAMYd2vXMi3b".to_owned()),
                (1, "4cOdK2wGLETKBW3PvgPWqT".to_owned()),
                (2, "2TpxZ7JUBn3uw46aR7qd6V".to_owned()),
                (3, "5hM5arv9KDbCHS0k9uqwjr".to_owned()),
                (4, "1fmoCZ6mtMiqA5GHWPcZz9".to_owned()),
                (5, "2QVmiA93GVhWNTWQctyY1K".to_owned()),
                (6, "5ChkMS8OtdzJeqyybCc9R5".to_owned()),
            ])
        };
        let selected_track = saved_session
            .queue_selected
            .min(tracks.len().saturating_sub(1));
        let resume_position_ms = saved_session.position_ms;
        let initial_position_ms = if has_saved_queue {
            resume_position_ms
        } else {
            0
        };
        let initial_progress = tracks
            .get(selected_track)
            .filter(|track| track.duration_ms() > 0)
            .map(|track| (initial_position_ms as f32 / track.duration_ms() as f32).clamp(0.0, 1.0))
            .unwrap_or(0.0);

        Self {
            section: Section::Home,
            queue_return_section: Section::Home,
            search: String::new(),
            mixes,
            recommendations_loaded: false,
            tracks,
            liked_songs: Vec::new(),
            liked_spotify_ids: HashMap::new(),
            liked_song_ids: HashSet::new(),
            liked_songs_loaded: false,
            liked_songs_error: None,
            playlists,
            playlists_loaded: false,
            playlist_name: None,
            playlist_loading: false,
            playlist_error: None,
            selected_track,
            liked_tracks: HashSet::new(),
            playing: false,
            progress: initial_progress,
            volume: saved_session.volume,
            shuffle: saved_session.shuffle,
            repeat: saved_session.repeat,
            queue_active: false,
            queue_tracks: Vec::new(),
            queue_spotify_ids: HashMap::new(),
            queue_selected: saved_session.queue_selected,
            queue_source: if saved_session.queue_source.is_empty() {
                "Starter catalog".to_owned()
            } else {
                saved_session.queue_source.clone()
            },
            queue_panel_visible: true,
            right_panel_tab: RightPanelTab::Queue,
            lyrics: Vec::new(),
            lyrics_synced: false,
            lyrics_loading: false,
            lyrics_error: None,
            lyrics_provider: None,
            lyrics_track_id: None,
            position_ms: initial_position_ms,
            artwork_textures: HashMap::new(),
            pending_artwork: HashSet::new(),
            artwork_retry_after: HashMap::new(),
            artwork_queue: VecDeque::new(),
            theme: ThemeSettings::load(&theme_path()),
            theme_dirty: false,
            applied_theme: None,
            applied_font: None,
            theme_message: None,
            theme_dialog: None,
            background_texture: None,
            background_key: None,
            background_load: None,
            transparent_window: false,
            transparency_failed: false,
            blur_applied: None,
            system_fonts: theme::installed_system_fonts(),
            tray: None,
            prefs: UiPrefs::default(),
            settings_tab: SettingsTab::General,
            updater: Updater::new(),
            started_at: Instant::now(),
            last_monitor_size: None,
            window_hidden: false,
            quitting: false,
            spotify: SpotifyClient::new(audio.clone()),
            connection_state: ConnectionState::Disconnected,
            connection_error: None,
            account_name: if saved_session.account.is_empty() {
                "Alex Morgan".to_owned()
            } else {
                saved_session.account.clone()
            },
            display_name: Some(saved_session.display_name.clone())
                .filter(|name| !name.is_empty() && !saved_session.account.is_empty()),
            avatar_url: saved_session.avatar_url.clone(),
            disc_angle: 0.0,
            disc_last_frame: None,
            spotify_ids,
            pending_playback: false,
            pending_resume: has_saved_queue,
            resume_position_ms,
            last_session_save: Instant::now(),
            spotify_client_id: spotify_config.client_id,
            spotify_redirect_uri: spotify_config.redirect_uri,
            spotify_web_api_client_id: spotify_config.web_api_client_id,
            spotify_web_api_redirect_uri: spotify_config.web_api_redirect_uri,
            settings_message: None,
            page_key: (Section::Home, None),
            page_entered: Instant::now(),
            audio: audio.clone(),
            eq_settings,
            eq_unsaved: false,
            vis_levels: Vec::new(),
            vis_targets: Vec::new(),
            vis_active: false,
            vis_last_frame: None,
            queue_expanded: false,
            lyrics_follow: None,
            lyrics_manual_until: None,
            detail: None,
            now_playing_tab: NowPlayingTab::Lyrics,
            credits: HashMap::new(),
            recent_tracks: Vec::new(),
            recent_ids: Vec::new(),
            recent_loading: false,
            recent_error: None,
            recent_loaded_at: None,
            sleep_timer: None,
            notice: None,
            detail_has_tracks: false,
        }
    }

    fn current_track(&self) -> &Track {
        if self.queue_active && !self.queue_tracks.is_empty() {
            let index = self.queue_selected.min(self.queue_tracks.len() - 1);
            return &self.queue_tracks[index];
        }
        &self.tracks[self.selected_track]
    }

    fn source_label(&self) -> String {
        if let Some(detail) = &self.detail {
            return detail.source_label();
        }
        if let Some(name) = &self.playlist_name {
            return format!("Playlist · {name}");
        }
        match self.section {
            Section::Home => "Made for you".to_owned(),
            Section::Search => "Search results".to_owned(),
            Section::Library => "Your library".to_owned(),
            Section::LikedSongs => "Liked Songs".to_owned(),
            Section::Queue => self.queue_source.clone(),
            Section::Settings => "Oynx".to_owned(),
        }
    }

    fn spotify_id_for(&self, index: usize) -> Option<String> {
        self.spotify_ids.get(&index).cloned()
    }

    fn spotify_queue(&self) -> Vec<String> {
        (0..self.tracks.len())
            .filter_map(|index| self.spotify_id_for(index))
            .collect()
    }

    fn snapshot_queue(&mut self) {
        let artwork_urls = self
            .tracks
            .iter()
            .filter_map(|track| track.image_url.clone())
            .collect::<Vec<_>>();
        for url in artwork_urls {
            self.request_artwork(&url);
        }
        self.queue_tracks = self.tracks.clone();
        self.queue_spotify_ids = self.spotify_ids.clone();
        self.queue_selected = self.selected_track;
        self.queue_source = self.source_label();
    }

    fn request_artwork(&mut self, url: &str) {
        if let Some(retry_after) = self.artwork_retry_after.get(url).copied() {
            if Instant::now() < retry_after {
                return;
            }
            self.artwork_retry_after.remove(url);
        }
        if url.is_empty()
            || self.window_hidden
            || self.connection_state != ConnectionState::Ready
            || self.artwork_textures.contains_key(url)
            || self.pending_artwork.len() >= MAX_PENDING_ARTWORK
            || !self.pending_artwork.insert(url.to_owned())
        {
            return;
        }
        self.spotify.load_artwork(url.to_owned());
    }

    fn save_session(&self) {
        if !self.queue_active || self.queue_tracks.is_empty() {
            return;
        }
        let queue = self
            .queue_tracks
            .iter()
            .enumerate()
            .filter_map(|(index, track)| {
                self.queue_spotify_ids
                    .get(&index)
                    .cloned()
                    .map(|id| SavedTrack::from_track(track, id))
            })
            .collect();
        SavedSession {
            account: if self.account_name == "Alex Morgan" {
                String::new()
            } else {
                self.account_name.clone()
            },
            display_name: self.display_name.clone().unwrap_or_default(),
            avatar_url: self.avatar_url.clone(),
            volume: self.volume,
            shuffle: self.shuffle,
            repeat: self.repeat,
            position_ms: self.position_ms,
            queue_source: self.queue_source.clone(),
            queue_selected: self.queue_selected,
            queue,
        }
        .save();
    }

    fn request_playback(&mut self, start_playing: bool) {
        if start_playing {
            self.pending_playback = true;
        }
        let Some(track_id) = self.spotify_id_for(self.selected_track) else {
            self.pending_playback = false;
            return;
        };

        match self.connection_state {
            ConnectionState::Ready => {
                self.snapshot_queue();
                self.spotify
                    .load(track_id, self.spotify_queue(), start_playing, 0);
                self.queue_active = true;
                self.pending_playback = false;
            }
            ConnectionState::Disconnected => {
                self.connection_error = None;
                self.spotify.login_with_config(
                    Some(self.spotify_client_id.clone()),
                    Some(self.spotify_redirect_uri.clone()),
                    Some(self.spotify_web_api_client_id.clone()),
                    Some(self.spotify_web_api_redirect_uri.clone()),
                );
            }
            ConnectionState::Authenticating | ConnectionState::Connecting => {}
        }
    }

    fn play_track(&mut self, index: usize) {
        if index < self.tracks.len() {
            self.selected_track = index;
            self.playing = true;
            self.progress = 0.0;
            self.request_playback(true);
            if let Some(track_id) = self.spotify_id_for(index) {
                self.request_lyrics_for_id(track_id, false);
            }
        }
    }

    fn next_track(&mut self) {
        if self.tracks.is_empty() {
            return;
        }
        if self.connection_state == ConnectionState::Ready && self.queue_active {
            self.spotify.next();
            self.playing = true;
            self.progress = 0.0;
        } else {
            self.selected_track = (self.selected_track + 1) % self.tracks.len();
            self.playing = true;
            self.progress = 0.0;
            self.request_playback(true);
        }
    }

    fn previous_track(&mut self) {
        if self.tracks.is_empty() {
            return;
        }
        if self.connection_state == ConnectionState::Ready && self.queue_active {
            self.spotify.previous();
            self.playing = true;
            self.progress = 0.0;
        } else {
            self.selected_track = if self.selected_track == 0 {
                self.tracks.len() - 1
            } else {
                self.selected_track - 1
            };
            self.playing = true;
            self.progress = 0.0;
            self.request_playback(true);
        }
    }

    fn toggle_shuffle(&mut self) {
        self.shuffle = !self.shuffle;
        if self.connection_state == ConnectionState::Ready {
            self.spotify.set_shuffle(self.shuffle);
        }
    }

    fn cycle_repeat(&mut self) {
        self.repeat = self.repeat.next();
        if self.connection_state == ConnectionState::Ready {
            self.spotify.cycle_repeat();
        }
    }

    fn toggle_playback(&mut self) {
        if self.connection_state == ConnectionState::Ready {
            if self.playing {
                self.spotify.pause();
            } else {
                self.spotify.play();
            }
        }
        self.playing = !self.playing;
    }

    fn request_lyrics_for_id(&mut self, track_id: String, force: bool) {
        if !force && self.lyrics_track_id.as_deref() == Some(track_id.as_str()) {
            return;
        }
        let index = if self.queue_active {
            self.queue_spotify_ids
                .iter()
                .find_map(|(index, id)| (id == &track_id).then_some(*index))
        } else {
            self.spotify_ids
                .iter()
                .find_map(|(index, id)| (id == &track_id).then_some(*index))
        };
        let Some(track) = index.and_then(|index| {
            if self.queue_active {
                self.queue_tracks.get(index)
            } else {
                self.tracks.get(index)
            }
        }) else {
            return;
        };
        let track = track.clone();
        self.lyrics_track_id = Some(track_id.clone());
        self.lyrics.clear();
        self.lyrics_synced = false;
        self.lyrics_loading = true;
        self.lyrics_error = None;
        self.lyrics_provider = None;
        let duration_ms = track.duration_ms();
        self.spotify.load_lyrics(
            track_id,
            track.artist,
            track.title,
            track.album,
            duration_ms,
            self.prefs.lyrics_source,
        );
    }

    /// Switches where lyrics come from and reloads the current song's lyrics.
    fn set_lyrics_source(&mut self, source: LyricsSource) {
        if self.prefs.lyrics_source == source {
            return;
        }
        self.prefs.lyrics_source = source;
        if let Some(track_id) = self.current_track_id() {
            self.request_lyrics_for_id(track_id, true);
        }
    }

    fn request_lyrics_for_current(&mut self) {
        let track_id = if self.queue_active {
            self.queue_spotify_ids.get(&self.queue_selected).cloned()
        } else {
            self.spotify_id_for(self.selected_track)
        };
        if let Some(track_id) = track_id {
            self.request_lyrics_for_id(track_id, false);
        }
    }

    fn set_volume(&mut self, volume: f32) {
        self.volume = volume;
        if self.connection_state == ConnectionState::Ready {
            self.spotify.set_volume(volume);
        }
    }

    fn poll_spotify(&mut self, ctx: &egui::Context) -> bool {
        let mut changed = false;
        for _ in 0..MAX_EVENTS_PER_FRAME {
            let Some(event) = self.spotify.poll() else {
                break;
            };
            changed = true;
            match event {
                PlaybackEvent::State(state) => {
                    self.connection_state = state;
                    if state == ConnectionState::Disconnected {
                        self.save_session();
                        self.display_name = None;
                        self.avatar_url = None;
                        self.playlists_loaded = false;
                        self.recommendations_loaded = false;
                        self.liked_songs_loaded = false;
                        self.liked_song_ids.clear();
                        self.liked_songs_error = None;
                        self.queue_active = false;
                        self.queue_tracks.clear();
                        self.queue_spotify_ids.clear();
                        self.queue_selected = 0;
                        self.queue_source = "No active queue".to_owned();
                        self.playlist_name = None;
                        self.playlist_loading = false;
                        self.playlist_error = None;
                        self.detail = None;
                        self.credits.clear();
                        self.recent_tracks.clear();
                        self.recent_ids.clear();
                        self.recent_loaded_at = None;
                        self.recent_loading = false;
                        self.sleep_timer = None;
                        self.pending_artwork.clear();
                        self.artwork_retry_after.clear();
                        self.artwork_queue.clear();
                    }
                    if matches!(
                        state,
                        ConnectionState::Authenticating | ConnectionState::Connecting
                    ) {
                        self.connection_error = None;
                    }
                }
                PlaybackEvent::Ready { username } => {
                    let previous_account = self.account_name.clone();
                    let should_resume = self.pending_resume
                        && (previous_account == "Alex Morgan" || previous_account == username);
                    self.connection_state = ConnectionState::Ready;
                    self.account_name = username;
                    if previous_account != self.account_name {
                        self.display_name = None;
                        self.avatar_url = None;
                    }
                    self.connection_error = None;
                    self.playing = false;
                    self.queue_active = false;
                    if !should_resume {
                        self.progress = 0.0;
                        self.position_ms = 0;
                        self.queue_tracks.clear();
                        self.queue_spotify_ids.clear();
                        self.queue_selected = 0;
                        self.queue_source = "No active queue".to_owned();
                    }
                    self.mixes.clear();
                    self.playlists.clear();
                    self.recommendations_loaded = false;
                    self.liked_song_ids.clear();
                    self.liked_songs_error = None;
                    self.playlists_loaded = false;
                    self.playlist_name = None;
                    self.playlist_loading = false;
                    self.playlist_error = None;
                    self.detail = None;
                    self.recent_loaded_at = None;
                    self.recent_loading = false;
                    self.spotify.set_volume(self.volume);
                    self.spotify.set_shuffle(self.shuffle);
                    self.spotify.set_repeat(self.repeat);
                    self.spotify.load_recommendations();
                    self.spotify.load_playlists();
                    self.spotify.load_liked_songs();
                    self.spotify.load_profile();
                    if self.pending_playback && self.spotify_id_for(self.selected_track).is_some() {
                        self.request_playback(true);
                    } else if should_resume
                        && let Some(track_id) = self.spotify_id_for(self.selected_track)
                    {
                        self.snapshot_queue();
                        let queue = self.spotify_queue();
                        self.spotify
                            .load(track_id, queue, false, self.resume_position_ms);
                        self.queue_active = true;
                        self.position_ms = self.resume_position_ms;
                        let duration = self.current_track().duration_ms();
                        if duration > 0 {
                            self.progress =
                                (self.position_ms as f32 / duration as f32).clamp(0.0, 1.0);
                        }
                        self.request_lyrics_for_current();
                    } else if self.section == Section::Search && !self.search.trim().is_empty() {
                        let query = self.search.trim().to_owned();
                        self.spotify.search(query);
                    }
                    self.pending_playback = false;
                    self.pending_resume = false;
                    self.save_session();
                }
                PlaybackEvent::Position {
                    track_id,
                    position_ms,
                    playing,
                } => {
                    self.playing = playing;
                    self.position_ms = position_ms;
                    let index = if self.queue_active {
                        self.queue_spotify_ids
                            .iter()
                            .find_map(|(index, id)| (id == &track_id).then_some(*index))
                    } else {
                        self.spotify_ids
                            .iter()
                            .find_map(|(index, id)| (id == &track_id).then_some(*index))
                    };
                    if let Some(index) = index {
                        if self.queue_active {
                            self.queue_selected = index;
                        } else {
                            self.selected_track = index;
                        }
                    }
                    if self.lyrics_track_id.as_deref() != Some(track_id.as_str()) {
                        self.request_lyrics_for_id(track_id.clone(), false);
                    }
                    let duration = self.current_track().duration_ms();
                    if duration > 0 {
                        self.progress = (position_ms as f32 / duration as f32).clamp(0.0, 1.0);
                    }
                }
                PlaybackEvent::TrackChanged {
                    track_id,
                    title,
                    duration_ms,
                } => {
                    if self.lyrics_track_id.as_deref() != Some(track_id.as_str()) {
                        self.request_lyrics_for_id(track_id.clone(), false);
                    }
                    let index = if self.queue_active {
                        self.queue_spotify_ids
                            .iter()
                            .find_map(|(index, id)| (id == &track_id).then_some(*index))
                    } else {
                        self.spotify_ids
                            .iter()
                            .find_map(|(index, id)| (id == &track_id).then_some(*index))
                    };
                    if let Some(index) = index {
                        if self.queue_active {
                            self.queue_selected = index;
                            if let Some(track) = self.queue_tracks.get_mut(index) {
                                track.title = title;
                                track.duration = format_duration(duration_ms);
                            }
                        } else {
                            self.selected_track = index;
                            self.tracks[index].title = title;
                            self.tracks[index].duration = format_duration(duration_ms);
                        }
                    }
                }
                PlaybackEvent::SearchResults { tracks } => {
                    self.apply_search_results(tracks);
                }
                PlaybackEvent::Recommendations { tracks } => {
                    self.apply_recommendations(tracks);
                }
                PlaybackEvent::Playlists { playlists } => {
                    self.apply_playlists(playlists);
                }
                PlaybackEvent::PlaylistTracks { tracks } => {
                    self.apply_playlist_tracks(tracks);
                }
                PlaybackEvent::LikedSongs { tracks } => {
                    self.apply_liked_songs(tracks);
                }
                PlaybackEvent::LikedSongsError { message } => {
                    self.liked_songs_error = Some(message);
                }
                PlaybackEvent::Profile {
                    display_name,
                    image_url,
                } => {
                    self.display_name = Some(display_name);
                    self.avatar_url = image_url;
                    self.save_session();
                }
                PlaybackEvent::QueueEnded => {
                    self.save_session();
                    self.queue_active = false;
                    self.playing = false;
                    self.progress = 1.0;
                }
                PlaybackEvent::LibraryUpdate {
                    track_id,
                    saved,
                    error,
                } => {
                    if let Some(error) = error {
                        self.set_liked_state(&track_id, !saved);
                        self.connection_error =
                            Some(format!("Could not update your Spotify library: {error}"));
                    } else {
                        self.set_liked_state(&track_id, saved);
                    }
                }
                PlaybackEvent::Lyrics {
                    track_id,
                    lines,
                    synced,
                    provider,
                    error,
                } => {
                    if self.lyrics_track_id.as_deref() == Some(track_id.as_str()) {
                        self.lyrics_loading = false;
                        self.lyrics = lines;
                        self.lyrics_synced = synced;
                        self.lyrics_provider = provider;
                        self.lyrics_error = error;
                    }
                }
                PlaybackEvent::ArtworkReady {
                    url,
                    width,
                    height,
                    rgba,
                } => {
                    self.artwork_retry_after.remove(&url);
                    // The URL stays in `pending_artwork` until its texture is uploaded, so
                    // queued images are not requested again and count towards the cap.
                    if self.window_hidden {
                        self.pending_artwork.remove(&url);
                    } else {
                        self.artwork_queue.push_back((url, width, height, rgba));
                    }
                }
                PlaybackEvent::ArtworkError { url } => {
                    self.pending_artwork.remove(&url);
                    self.artwork_retry_after
                        .insert(url, Instant::now() + Duration::from_secs(30));
                }
                PlaybackEvent::ArtistPage {
                    artist_id,
                    name,
                    image_url,
                    tracks,
                    albums,
                    error,
                } => {
                    if let Some(DetailPage::Artist { id, .. }) = &self.detail
                        && *id == artist_id
                    {
                        if let Some(url) = &image_url {
                            self.request_artwork(url);
                        }
                        for album in &albums {
                            if let Some(url) = &album.image_url {
                                self.request_artwork(url);
                            }
                        }
                        self.apply_detail_tracks(tracks);
                        self.detail = Some(DetailPage::Artist {
                            id: artist_id,
                            name,
                            image_url,
                            albums,
                            loading: false,
                            error,
                        });
                    }
                }
                PlaybackEvent::AlbumPage {
                    album_id,
                    name,
                    artist,
                    artist_id,
                    year,
                    image_url,
                    tracks,
                    error,
                } => {
                    if let Some(DetailPage::Album {
                        id,
                        name: open_name,
                        artist: open_artist,
                        ..
                    }) = self.detail.clone()
                        && id == album_id
                    {
                        // A failed load keeps the name and artist the page opened with.
                        let name = if name.is_empty() { open_name } else { name };
                        let artist = if artist.is_empty() { open_artist } else { artist };
                        if let Some(url) = &image_url {
                            self.request_artwork(url);
                        }
                        self.apply_detail_tracks(tracks);
                        self.detail = Some(DetailPage::Album {
                            id: album_id,
                            name,
                            artist,
                            artist_id,
                            year,
                            image_url,
                            loading: false,
                            error,
                        });
                    }
                }
                PlaybackEvent::RecentlyPlayed { tracks, error } => {
                    self.recent_loading = false;
                    self.recent_error = error;
                    if self.recent_error.is_none() {
                        for track in &tracks {
                            if let Some(url) = &track.image_url {
                                self.request_artwork(url);
                            }
                        }
                        self.recent_ids = tracks.iter().map(|track| track.id.clone()).collect();
                        self.recent_tracks = Self::map_spotify_tracks(tracks).0;
                    }
                }
                PlaybackEvent::TopItems {
                    range,
                    tracks,
                    artists,
                    error,
                } => {
                    if let Some(DetailPage::Top { range: open_range, .. }) = &self.detail
                        && *open_range == range
                    {
                        for artist in &artists {
                            if let Some(url) = &artist.image_url {
                                self.request_artwork(url);
                            }
                        }
                        self.apply_detail_tracks(tracks);
                        self.detail = Some(DetailPage::Top {
                            range,
                            artists,
                            loading: false,
                            error,
                        });
                    }
                }
                PlaybackEvent::Credits { track_id, result } => {
                    let state = match result {
                        Ok(credits) => CreditsState::Ready(credits),
                        Err(error) => CreditsState::Failed(error),
                    };
                    self.credits.insert(track_id, state);
                }
                PlaybackEvent::Radio {
                    seed_track_id,
                    tracks,
                    error,
                } => {
                    if let Some(DetailPage::Radio {
                        seed_id,
                        title,
                        artist,
                        image_url,
                        ..
                    }) = self.detail.clone()
                        && seed_id == seed_track_id
                    {
                        let start = error.is_none() && !tracks.is_empty();
                        if start {
                            self.apply_detail_tracks(tracks);
                        }
                        self.detail = Some(DetailPage::Radio {
                            seed_id,
                            title,
                            artist,
                            image_url,
                            loading: false,
                            error,
                        });
                        if start {
                            self.play_track(0);
                        }
                    }
                }
                PlaybackEvent::Notice(message) => {
                    self.show_notice(&message);
                }
                PlaybackEvent::Ended if self.sleep_timer == Some(SleepTimer::EndOfTrack) => {
                    self.sleep_timer = None;
                    self.playing = false;
                    self.save_session();
                    self.show_notice("Sleep timer paused playback");
                }
                PlaybackEvent::Ended => {
                    let has_queue = if self.queue_active {
                        !self.queue_spotify_ids.is_empty()
                    } else {
                        !self.spotify_queue().is_empty()
                    };
                    if self.connection_state == ConnectionState::Ready && has_queue {
                        self.spotify.next();
                        self.playing = true;
                    } else {
                        self.playing = false;
                    }
                }
                PlaybackEvent::Error(message) => {
                    self.playlist_loading = false;
                    if self.playlist_name.is_some() {
                        self.playlist_error = Some(message.clone());
                    }
                    if matches!(
                        self.connection_state,
                        ConnectionState::Authenticating | ConnectionState::Connecting
                    ) {
                        self.connection_state = ConnectionState::Disconnected;
                    }
                    self.connection_error = Some(message);
                }
            }
        }

        for _ in 0..ARTWORK_UPLOADS_PER_FRAME {
            let Some((url, width, height, rgba)) = self.artwork_queue.pop_front() else {
                break;
            };
            self.pending_artwork.remove(&url);
            let width = width as usize;
            let height = height as usize;
            if width == 0
                || height == 0
                || width > 2_048
                || height > 2_048
                || width
                    .checked_mul(height)
                    .and_then(|pixels| pixels.checked_mul(4))
                    != Some(rgba.len())
            {
                self.artwork_retry_after
                    .insert(url, Instant::now() + Duration::from_secs(30));
                continue;
            }
            let color = egui::ColorImage::from_rgba_unmultiplied([width, height], &rgba);
            let texture = ctx.load_texture(url.clone(), color, egui::TextureOptions::LINEAR);
            self.artwork_textures.insert(url, texture);
        }
        changed || !self.artwork_queue.is_empty()
    }

    fn apply_search_results(&mut self, spotify_tracks: Vec<SpotifyTrack>) {
        if self.detail.is_some() {
            return;
        }
        if spotify_tracks.is_empty() {
            self.connection_error = Some("Spotify returned no tracks for that search.".into());
            return;
        }
        self.replace_tracks(spotify_tracks);
        self.playlist_name = None;
        self.playlist_loading = false;
    }

    fn apply_recommendations(&mut self, spotify_tracks: Vec<SpotifyTrack>) {
        if (self.section == Section::LikedSongs && self.liked_songs_loaded) || self.detail.is_some() {
            return;
        }
        if spotify_tracks.is_empty() {
            self.connection_error =
                Some("Spotify did not return any personalized recommendations yet.".into());
            return;
        }
        self.replace_tracks(spotify_tracks);
        self.recommendations_loaded = true;
        self.mixes = self
            .tracks
            .iter()
            .take(6)
            .enumerate()
            .map(|(index, track)| Mix {
                title: track.title.clone(),
                subtitle: track.artist.clone(),
                track_index: index,
                artwork: Self::artwork_for_index(index),
            })
            .collect();
        self.playlist_name = None;
        self.playlist_loading = false;
    }

    fn apply_playlists(&mut self, spotify_playlists: Vec<SpotifyPlaylist>) {
        for playlist in &spotify_playlists {
            if let Some(url) = &playlist.image_url {
                self.request_artwork(url);
            }
        }
        self.playlists_loaded = true;
        self.playlists = spotify_playlists
            .into_iter()
            .enumerate()
            .map(|(index, playlist)| Playlist {
                id: playlist.id,
                title: playlist.name,
                description: if playlist.description.trim().is_empty() {
                    format!("{} songs · {}", playlist.track_count, playlist.owner)
                } else {
                    format!(
                        "{} · {} songs · {}",
                        playlist.description, playlist.track_count, playlist.owner
                    )
                },
                artwork: Self::artwork_for_index(index),
                image_url: playlist.image_url,
                owner_id: playlist.owner_id,
                collaborative: playlist.collaborative,
            })
            .collect();
    }

    fn apply_playlist_tracks(&mut self, spotify_tracks: Vec<SpotifyTrack>) {
        if self.detail.is_some() {
            return;
        }
        self.playlist_loading = false;
        self.playlist_error = None;
        if spotify_tracks.is_empty() {
            self.connection_error = Some("That Spotify playlist has no playable tracks.".into());
            return;
        }
        self.replace_tracks(spotify_tracks);
    }

    fn map_spotify_tracks(
        spotify_tracks: Vec<SpotifyTrack>,
    ) -> (Vec<Track>, HashMap<usize, String>) {
        let mut spotify_ids = HashMap::new();
        let tracks = spotify_tracks
            .into_iter()
            .enumerate()
            .map(|(index, track)| {
                spotify_ids.insert(index, track.id);
                Track {
                    title: track.title,
                    artist: track.artist,
                    album: track.album,
                    duration: format_duration(track.duration_ms),
                    artwork: Self::artwork_for_index(index),
                    image_url: track.image_url,
                    artist_id: track.artist_id,
                    album_id: track.album_id,
                }
            })
            .collect();
        (tracks, spotify_ids)
    }

    fn replace_tracks(&mut self, spotify_tracks: Vec<SpotifyTrack>) {
        for track in &spotify_tracks {
            if let Some(url) = &track.image_url {
                self.request_artwork(url);
            }
        }
        let liked_spotify_ids = self.liked_song_ids.clone();
        let (tracks, spotify_ids) = Self::map_spotify_tracks(spotify_tracks);
        self.tracks = tracks;
        self.spotify_ids = spotify_ids;
        self.liked_tracks = self
            .spotify_ids
            .iter()
            .filter_map(|(index, id)| liked_spotify_ids.contains(id).then_some(*index))
            .collect();
        self.selected_track = 0;
        if !self.queue_active {
            self.playing = false;
            self.progress = 0.0;
        }
        self.connection_error = None;
    }

    fn activate_liked_songs(&mut self) {
        if self.liked_songs.is_empty() {
            return;
        }
        self.tracks = self.liked_songs.clone();
        self.spotify_ids = self.liked_spotify_ids.clone();
        self.liked_tracks = self
            .spotify_ids
            .iter()
            .filter_map(|(index, id)| self.liked_song_ids.contains(id).then_some(*index))
            .collect();
        self.selected_track = 0;
        if !self.queue_active {
            self.playing = false;
            self.progress = 0.0;
        }
    }

    fn apply_liked_songs(&mut self, spotify_tracks: Vec<SpotifyTrack>) {
        for track in &spotify_tracks {
            if let Some(url) = &track.image_url {
                self.request_artwork(url);
            }
        }
        let current_id = if self.section == Section::LikedSongs {
            if self.queue_active {
                self.queue_spotify_ids.get(&self.queue_selected).cloned()
            } else {
                self.spotify_id_for(self.selected_track)
            }
        } else {
            None
        };
        let (tracks, ids) = Self::map_spotify_tracks(spotify_tracks);
        self.liked_songs_loaded = true;
        self.liked_songs_error = None;
        self.liked_songs = tracks;
        self.liked_song_ids = ids.values().cloned().collect();
        self.liked_spotify_ids = ids;
        if self.section == Section::LikedSongs && !self.liked_songs.is_empty() {
            self.tracks = self.liked_songs.clone();
            self.spotify_ids = self.liked_spotify_ids.clone();
            if let Some(current_id) = current_id
                && let Some(index) = self
                    .spotify_ids
                    .iter()
                    .find_map(|(index, id)| (id == &current_id).then_some(*index))
            {
                self.selected_track = index;
            } else {
                self.selected_track = 0;
                if !self.queue_active {
                    self.playing = false;
                    self.progress = 0.0;
                }
            }
        }
        self.liked_tracks = self
            .spotify_ids
            .iter()
            .filter_map(|(index, id)| self.liked_song_ids.contains(id).then_some(*index))
            .collect();
    }

    fn open_liked_songs(&mut self) {
        self.section = Section::LikedSongs;
        self.liked_songs_error = None;
        self.playlist_name = None;
        self.playlist_loading = false;
        if self.liked_songs_loaded {
            self.activate_liked_songs();
        } else {
            self.spotify.load_liked_songs();
        }
    }

    fn open_playlist(&mut self, playlist: &Playlist) {
        if playlist.id.is_empty() {
            self.section = Section::Library;
            return;
        }
        self.detail = None;
        self.playlist_name = Some(playlist.title.clone());
        self.playlist_loading = true;
        self.playlist_error = None;
        self.section = Section::Library;
        self.connection_error = None;
        self.spotify.load_playlist(playlist.id.clone());
    }

    fn spotify_ready(&mut self) -> bool {
        let ready = self.connection_state == ConnectionState::Ready;
        if !ready {
            self.connection_error = Some("Sign in to Spotify first.".to_owned());
        }
        ready
    }

    fn open_detail(&mut self, detail: DetailPage) {
        self.section = Section::Library;
        self.playlist_name = None;
        self.playlist_loading = false;
        self.playlist_error = None;
        self.connection_error = None;
        self.detail = Some(detail);
        self.detail_has_tracks = false;
    }

    /// Shows a detail page's songs. `tracks` is never left empty, because the
    /// player falls back to it for the current song.
    fn apply_detail_tracks(&mut self, tracks: Vec<SpotifyTrack>) {
        if tracks.is_empty() {
            self.detail_has_tracks = false;
        } else {
            self.replace_tracks(tracks);
            self.detail_has_tracks = true;
        }
    }

    fn open_artist(&mut self, artist_id: String, name: String) {
        if !self.spotify_ready() {
            return;
        }
        self.open_detail(DetailPage::Artist {
            id: artist_id.clone(),
            name: name.clone(),
            image_url: None,
            albums: Vec::new(),
            loading: true,
            error: None,
        });
        self.spotify.load_artist(artist_id, name);
    }

    fn open_album(&mut self, album_id: String, name: String, artist: String) {
        if !self.spotify_ready() {
            return;
        }
        self.open_detail(DetailPage::Album {
            id: album_id.clone(),
            name,
            artist,
            artist_id: None,
            year: None,
            image_url: None,
            loading: true,
            error: None,
        });
        self.spotify.load_album(album_id);
    }

    fn open_top(&mut self, range: TopRange) {
        if !self.spotify_ready() {
            return;
        }
        let artists = match &self.detail {
            Some(DetailPage::Top { artists, .. }) => artists.clone(),
            _ => Vec::new(),
        };
        self.open_detail(DetailPage::Top {
            range,
            artists,
            loading: true,
            error: None,
        });
        self.spotify.load_top(range);
    }

    fn start_radio(&mut self, track: &Track, track_id: String) {
        if !self.spotify_ready() {
            return;
        }
        self.open_detail(DetailPage::Radio {
            seed_id: track_id.clone(),
            title: track.title.clone(),
            artist: track.artist.clone(),
            image_url: track.image_url.clone(),
            loading: true,
            error: None,
        });
        self.spotify.start_radio(track_id);
    }

    /// Runs a Spotify search from anywhere, such as a name in the song credits.
    fn search_for(&mut self, query: &str) {
        let query = query.trim();
        if query.is_empty() || !self.spotify_ready() {
            return;
        }
        self.detail = None;
        self.playlist_name = None;
        self.section = Section::Search;
        self.search = query.to_owned();
        self.spotify.search(query.to_owned());
    }

    /// Plays a list that is not the page on screen, such as Recently played.
    fn play_list(&mut self, tracks: Vec<Track>, ids: Vec<String>, index: usize, source: &str) {
        let Some(track_id) = ids.get(index).cloned() else {
            return;
        };
        if !self.spotify_ready() {
            return;
        }
        for url in tracks.iter().filter_map(|track| track.image_url.clone()) {
            self.request_artwork(&url);
        }
        self.queue_tracks = tracks;
        self.queue_spotify_ids = ids.iter().cloned().enumerate().collect();
        self.queue_selected = index;
        self.queue_source = source.to_owned();
        self.queue_active = true;
        self.playing = true;
        self.progress = 0.0;
        self.spotify.load(track_id.clone(), ids, true, 0);
        self.request_lyrics_for_id(track_id, true);
    }

    /// Adds a song after the current one (`next`) or to the end of the queue.
    fn enqueue_track(&mut self, track: Track, track_id: String, next: bool) {
        if !self.spotify_ready() {
            return;
        }
        if !self.queue_active || self.queue_tracks.is_empty() {
            self.play_list(vec![track], vec![track_id], 0, "Queue");
            return;
        }
        let mut ids = (0..self.queue_tracks.len())
            .map(|index| self.queue_spotify_ids.get(&index).cloned())
            .collect::<Vec<_>>();
        let position = if next {
            (self.queue_selected + 1).min(self.queue_tracks.len())
        } else {
            self.queue_tracks.len()
        };
        if let Some(url) = &track.image_url {
            self.request_artwork(url);
        }
        self.queue_tracks.insert(position, track);
        ids.insert(position, Some(track_id.clone()));
        self.queue_spotify_ids = ids
            .into_iter()
            .enumerate()
            .filter_map(|(index, id)| id.map(|id| (index, id)))
            .collect();
        self.spotify.enqueue(track_id, next);
        self.show_notice(if next { "Playing next" } else { "Added to queue" });
    }

    /// Playlists the listener can add songs to: their own and collaborative ones.
    fn editable_playlists(&self) -> Vec<Playlist> {
        self.playlists
            .iter()
            .filter(|playlist| {
                !playlist.id.is_empty()
                    && (playlist.collaborative
                        || playlist.owner_id.as_deref() == Some(self.account_name.as_str()))
            })
            .cloned()
            .collect()
    }

    fn show_notice(&mut self, message: &str) {
        self.notice = Some((message.to_owned(), Instant::now()));
    }

    fn request_credits_for_current(&mut self) {
        let Some(track_id) = self.current_track_id() else {
            return;
        };
        // A failed lookup stays failed until the listener asks to try again.
        if self.connection_state != ConnectionState::Ready || self.credits.contains_key(&track_id) {
            return;
        }
        let track = self.current_track().clone();
        self.credits.insert(track_id.clone(), CreditsState::Loading);
        let duration_ms = track.duration_ms();
        self.spotify
            .load_credits(track_id, track.title, track.artist, duration_ms);
    }

    /// The actions for one song, shared by its right-click menu and the player's menu.
    fn track_menu(&mut self, ui: &mut egui::Ui, track: &Track, track_id: Option<&str>) {
        self.track_menu_items(ui, track, track_id, false);
    }

    /// `playing_now` leaves out the queue actions, which make no sense for the
    /// song that is already playing.
    fn track_menu_items(&mut self, ui: &mut egui::Ui, track: &Track, track_id: Option<&str>, playing_now: bool) {
        ui.spacing_mut().item_spacing.y = 2.0;
        let ready = self.connection_state == ConnectionState::Ready;
        if let Some(track_id) = track_id.filter(|_| ready) {
            if !playing_now && menu_item(ui, Icon::Next, "Play next", false).clicked() {
                self.enqueue_track(track.clone(), track_id.to_owned(), true);
                ui.close();
            }
            if !playing_now && menu_item(ui, Icon::Queue, "Add to queue", false).clicked() {
                self.enqueue_track(track.clone(), track_id.to_owned(), false);
                ui.close();
            }
            if menu_item(ui, Icon::Sparkle, "Start song radio", false).clicked() {
                self.start_radio(track, track_id.to_owned());
                ui.close();
            }
            let playlists = self.editable_playlists();
            if !playlists.is_empty() {
                submenu(ui, Icon::Note, "Add to playlist", |ui| {
                    ui.set_min_width(200.0);
                    egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                        for playlist in playlists {
                            if menu_item(ui, Icon::Note, &playlist.title, false).clicked() {
                                self.spotify.add_to_playlist(
                                    playlist.id.clone(),
                                    playlist.title.clone(),
                                    track_id.to_owned(),
                                );
                                ui.close();
                            }
                        }
                    });
                });
            }
        }
        if ready
            && let Some(artist_id) = track.artist_id.clone()
            && menu_item(ui, Icon::Search, "Go to artist", false).clicked()
        {
            let name = track.artist.split(", ").next().unwrap_or_default().to_owned();
            self.open_artist(artist_id, name);
            ui.close();
        }
        if ready
            && let Some(album_id) = track.album_id.clone()
            && menu_item(ui, Icon::Note, "Go to album", false).clicked()
        {
            self.open_album(album_id, track.album.clone(), track.artist.clone());
            ui.close();
        }
        if let Some(track_id) = track_id {
            let link = format!("https://open.spotify.com/track/{track_id}");
            if menu_item(ui, Icon::Share, "Copy song link", false).clicked() {
                ui.ctx().copy_text(link.clone());
                self.show_notice("Song link copied");
                ui.close();
            }
            if menu_item(ui, Icon::Share, "Open in Spotify", false).clicked() {
                ui.ctx().open_url(egui::OpenUrl::new_tab(link));
                ui.close();
            }
        }
    }

    fn artwork_for_index(index: usize) -> Artwork {
        match index % 4 {
            0 => Artwork {
                pattern: 0,
            },
            1 => Artwork {
                pattern: 1,
            },
            2 => Artwork {
                pattern: 2,
            },
            _ => Artwork {
                pattern: 3,
            },
        }
    }

    fn set_liked_state(&mut self, track_id: &str, saved: bool) {
        if saved {
            self.liked_song_ids.insert(track_id.to_owned());
        } else {
            self.liked_song_ids.remove(track_id);
        }
        self.liked_tracks = self
            .spotify_ids
            .iter()
            .filter_map(|(index, id)| self.liked_song_ids.contains(id).then_some(*index))
            .collect();
    }

    fn toggle_like(&mut self, index: usize) {
        let Some(track_id) = self.spotify_id_for(index) else {
            if !self.liked_tracks.remove(&index) {
                self.liked_tracks.insert(index);
            }
            return;
        };
        let saved = !self.liked_song_ids.contains(&track_id);
        self.set_liked_state(&track_id, saved);
        if self.connection_state == ConnectionState::Ready {
            self.spotify.set_track_saved(track_id, saved);
        }
    }

    fn track_matches(track: &Track, query: &str) -> bool {
        query.is_empty()
            || track.title.to_lowercase().contains(query)
            || track.artist.to_lowercase().contains(query)
            || track.album.to_lowercase().contains(query)
    }

    /// Rebuilds the fonts, palette and egui style whenever the theme or the
    /// Windows light/dark mode changed since they were last applied.
    fn apply_theme(&mut self, ctx: &egui::Context) {
        if self.applied_font.as_ref() != Some(&self.theme.font) {
            let custom = self.theme.font.as_deref().and_then(|path| match theme::load_font(path) {
                Ok(bytes) => Some(bytes),
                Err(error) => {
                    self.theme_message = Some(error);
                    None
                }
            });
            if custom.is_none() && self.theme.font.is_some() {
                self.theme.font = None;
                self.theme_dirty = true;
            }
            Self::apply_fonts(ctx, custom);
            self.applied_font = Some(self.theme.font.clone());
        }
        let dark = self.theme.is_dark(ctx.system_theme());
        if self
            .applied_theme
            .as_ref()
            .is_none_or(|(applied, applied_dark)| *applied != self.theme || *applied_dark != dark)
        {
            let palette = self.theme.palette(dark);
            theme::set_palette(palette);
            Self::apply_visuals(ctx, &palette);
            self.applied_theme = Some((self.theme.clone(), dark));
        }
    }

    /// Registers Inter in its weights, with a custom font (when chosen) in
    /// front of it so Inter still covers any characters the custom font lacks.
    fn apply_fonts(ctx: &egui::Context, custom: Option<Vec<u8>>) {
        let mut fonts = egui::FontDefinitions::default();
        let inter = |weight: f32| {
            std::sync::Arc::new(egui::FontData::from_static(INTER_FONT).tweak(
                egui::epaint::text::FontTweak {
                    coords: egui::epaint::text::VariationCoords::new([(*b"wght", weight)]),
                    ..Default::default()
                },
            ))
        };
        fonts.font_data.insert("Inter".to_owned(), inter(400.0));
        fonts.font_data.insert(FAMILY_LIGHT.to_owned(), inter(300.0));
        fonts.font_data.insert(FAMILY_MEDIUM.to_owned(), inter(460.0));
        fonts.font_data.insert(FAMILY_BOLD.to_owned(), inter(520.0));
        let fallbacks = fonts
            .families
            .get(&egui::FontFamily::Proportional)
            .cloned()
            .unwrap_or_default();
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts
                .families
                .entry(family)
                .or_default()
                .insert(0, "Inter".to_owned());
        }
        for name in [FAMILY_LIGHT, FAMILY_MEDIUM, FAMILY_BOLD] {
            let mut family = vec![name.to_owned()];
            family.extend(fallbacks.iter().cloned());
            fonts
                .families
                .insert(egui::FontFamily::Name(name.into()), family);
        }
        if let Some(bytes) = custom {
            fonts
                .font_data
                .insert(CUSTOM_FONT.to_owned(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
            for (family, names) in fonts.families.iter_mut() {
                if *family != egui::FontFamily::Monospace {
                    names.insert(0, CUSTOM_FONT.to_owned());
                }
            }
        }
        ctx.set_fonts(fonts);
    }

    fn apply_visuals(ctx: &egui::Context, palette: &theme::Palette) {
        let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
        let mut visuals = if palette.dark { egui::Visuals::dark() } else { egui::Visuals::light() };

        visuals.override_text_color = Some(pal().text);
        visuals.weak_text_color = Some(pal().muted);
        visuals.panel_fill = pal().background;
        visuals.window_fill = pal().surface_raised;
        visuals.window_stroke = egui::Stroke::new(1.0, pal().border);
        visuals.window_corner_radius = egui::CornerRadius::same(RADIUS_MD);
        visuals.menu_corner_radius = egui::CornerRadius::same(RADIUS_MD);
        visuals.extreme_bg_color = pal().surface_raised;
        visuals.faint_bg_color = pal().surface;
        visuals.text_edit_bg_color = Some(pal().surface_raised);
        visuals.hyperlink_color = pal().accent;
        visuals.selection.bg_fill = pal().accent.gamma_multiply(0.35);
        visuals.selection.stroke = egui::Stroke::new(1.0, pal().accent);
        visuals.slider_trailing_fill = true;

        let radius = egui::CornerRadius::same(RADIUS_SM);
        visuals.widgets.noninteractive.bg_fill = egui::Color32::TRANSPARENT;
        visuals.widgets.noninteractive.weak_bg_fill = egui::Color32::TRANSPARENT;
        visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, pal().border);
        visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, pal().text);
        visuals.widgets.noninteractive.corner_radius = radius;
        visuals.widgets.inactive.bg_fill = pal().surface_raised;
        visuals.widgets.inactive.weak_bg_fill = pal().surface_raised;
        visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, pal().border);
        visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, pal().text);
        visuals.widgets.inactive.corner_radius = radius;
        visuals.widgets.hovered.bg_fill = pal().surface_hover;
        visuals.widgets.hovered.weak_bg_fill = pal().surface_hover;
        visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, pal().subtle);
        visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, pal().text);
        visuals.widgets.hovered.corner_radius = radius;
        visuals.widgets.hovered.expansion = 0.0;
        visuals.widgets.active.bg_fill = pal().surface_hover;
        visuals.widgets.active.weak_bg_fill = pal().surface_hover;
        visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, pal().accent);
        visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, pal().text);
        visuals.widgets.active.corner_radius = radius;
        visuals.widgets.active.expansion = 0.0;
        visuals.widgets.open = visuals.widgets.hovered;

        style.visuals = visuals;
        style.interaction.selectable_labels = false;
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.scroll = egui::style::ScrollStyle::floating();
        style
            .text_styles
            .insert(egui::TextStyle::Heading, font_bold(28.0));
        style
            .text_styles
            .insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
        style
            .text_styles
            .insert(egui::TextStyle::Button, font_medium(13.0));
        style
            .text_styles
            .insert(egui::TextStyle::Small, egui::FontId::proportional(11.0));

        ctx.set_global_style(style);
    }

    // ----------------------------------------------------------------------
    // Shared painting helpers
    // ----------------------------------------------------------------------

    fn paint_placeholder_art(
        painter: &egui::Painter,
        rect: egui::Rect,
        artwork: &Artwork,
        radius: egui::CornerRadius,
    ) {
        let size = rect.width().min(rect.height());
        // A slightly different grey per placeholder keeps rows of them readable.
        let tone = [0.0, 0.04, 0.08, 0.02][(artwork.pattern % 4) as usize];
        painter.rect_filled(rect, radius, pal().placeholder_art.lerp_to_gamma(pal().text, tone));
        paint_icon(painter, Icon::Note, rect.center(), size * 0.32, pal().text.gamma_multiply(0.16));
    }

    fn paint_liked_tile(painter: &egui::Painter, rect: egui::Rect, radius: egui::CornerRadius) {
        painter.rect_filled(rect, radius, pal().liked_tile);
        paint_icon(
            painter,
            Icon::HeartFilled,
            rect.center(),
            rect.width() * 0.42,
            pal().text,
        );
    }

    fn paint_artwork(
        &mut self,
        ui: &egui::Ui,
        rect: egui::Rect,
        artwork: &Artwork,
        image_url: Option<&str>,
        radius: egui::CornerRadius,
    ) {
        if let Some(url) = image_url {
            self.request_artwork(url);
            // Cross-fade from the placeholder once the texture arrives. Art that is
            // already loaded the first time it is seen appears without a fade.
            let loaded = self.artwork_textures.contains_key(url);
            let fade = animate(ui, egui::Id::new(("artwork_fade", url)), loaded, ANIM_SLOW);
            if let Some(texture) = self.artwork_textures.get(url) {
                if fade < 1.0 {
                    Self::paint_placeholder_art(ui.painter(), rect, artwork, radius);
                }
                egui::Image::from_texture(texture)
                    .corner_radius(radius)
                    .tint(egui::Color32::WHITE.gamma_multiply(fade))
                    .paint_at(ui, rect);
                return;
            }
        }
        Self::paint_placeholder_art(ui.painter(), rect, artwork, radius);
    }

    /// The name to show for the signed-in user: the Spotify display name when
    /// known, otherwise the account ID.
    fn account_label(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.account_name)
    }

    fn account_initial(&self) -> String {
        self.account_label()
            .chars()
            .find(|c| c.is_alphanumeric())
            .map(|c| c.to_uppercase().collect())
            .unwrap_or_else(|| "?".to_owned())
    }

    /// Paints the profile picture as a circle, or the user's initial as a fallback.
    fn paint_avatar(&mut self, ui: &egui::Ui, center: egui::Pos2, radius: f32) {
        let rect = egui::Rect::from_center_size(center, egui::vec2(radius, radius) * 2.0);
        if let Some(url) = self.avatar_url.clone() {
            self.request_artwork(&url);
            if let Some(texture) = self.artwork_textures.get(&url) {
                egui::Image::from_texture(texture)
                    .corner_radius(radius)
                    .paint_at(ui, rect);
                return;
            }
        }
        ui.painter().circle_filled(center, radius, pal().accent_dark);
        ui.painter().text(
            center,
            egui::Align2::CENTER_CENTER,
            self.account_initial(),
            font_bold(radius * 0.8),
            pal().accent,
        );
    }

    fn is_current_track(&self, index: usize) -> bool {
        let current_id = if self.queue_active && !self.queue_tracks.is_empty() {
            self.queue_spotify_ids.get(&self.queue_selected)
        } else {
            self.spotify_ids.get(&self.selected_track)
        };
        match (current_id, self.spotify_ids.get(&index)) {
            (Some(current), Some(id)) => current == id,
            _ => !self.queue_active && index == self.selected_track,
        }
    }

    fn page_scrolls_itself(&self) -> bool {
        match self.section {
            Section::LikedSongs | Section::Queue | Section::Home => true,
            Section::Library => self.detail.is_none() && self.playlist_name.is_some(),
            Section::Search => !self.search.trim().is_empty(),
            Section::Settings => false,
        }
    }

    fn open_section(&mut self, item: Section) {
        if item != Section::Library {
            self.detail = None;
        }
        if item == Section::LikedSongs {
            self.open_liked_songs();
        } else {
            let leaving_liked = self.section == Section::LikedSongs;
            self.section = item;
            if leaving_liked || item == Section::Home {
                self.spotify.load_recommendations();
            }
        }
    }

    fn toggle_queue_view(&mut self, wide_layout: bool) {
        if wide_layout {
            self.queue_panel_visible = !self.queue_panel_visible;
        } else if self.section == Section::Queue {
            self.section = self.queue_return_section;
        } else {
            self.queue_return_section = self.section;
            self.section = Section::Queue;
        }
    }

    // ----------------------------------------------------------------------
    // Sidebar
    // ----------------------------------------------------------------------

    // ----------------------------------------------------------------------
    // Header and page scaffolding
    // ----------------------------------------------------------------------

    fn page_title(ui: &mut egui::Ui, title: &str) {
        ui.label(egui::RichText::new(title).font(font_light(32.0)).color(pal().text));
        ui.add_space(18.0);
    }

    fn section_heading(ui: &mut egui::Ui, title: &str) {
        ui.label(egui::RichText::new(title).font(font_bold(21.0)).color(pal().text));
        ui.add_space(12.0);
    }

    fn muted_note(ui: &mut egui::Ui, text: &str) {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(text).size(14.0).color(pal().muted));
        ui.add_space(4.0);
    }

    fn draw_page(&mut self, ui: &mut egui::Ui) {
        self.draw_notice(ui);
        self.draw_connection_banner(ui);
        match self.section {
            Section::Home => self.draw_now_playing(ui),
            Section::Search => self.draw_search(ui),
            Section::Library => self.draw_library(ui),
            Section::LikedSongs => self.draw_liked_songs(ui),
            Section::Queue => self.draw_queue(ui),
            Section::Settings => self.draw_settings(ui),
        }
    }

    // ----------------------------------------------------------------------
    // Home
    // ----------------------------------------------------------------------

    /// Paints a square-art card and returns its response.
    fn draw_media_card(
        &mut self,
        ui: &mut egui::Ui,
        width: f32,
        artwork: &Artwork,
        image_url: Option<&str>,
        title: &str,
        subtitle: &str,
        show_play: bool,
    ) -> egui::Response {
        let pad = 10.0;
        let art_size = width - pad * 2.0;
        let height = pad + art_size + 10.0 + 20.0 + 18.0 + pad;
        let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
        let hover = animate(
            ui,
            response.id.with("hover"),
            ui.rect_contains_pointer(rect),
            ANIM_MEDIUM,
        );
        if hover > 0.0 {
            ui.painter()
                .rect_filled(rect, RADIUS_MD, pal().surface_raised.gamma_multiply(hover));
        }
        let art = egui::Rect::from_min_size(rect.min + egui::vec2(pad, pad), egui::vec2(art_size, art_size));
        self.paint_artwork(ui, art, artwork, image_url, egui::CornerRadius::same(RADIUS_SM));
        if show_play && hover > 0.0 {
            // The play button rises into place and fades in.
            let center = art.right_bottom() - egui::vec2(28.0, 28.0 - 8.0 * (1.0 - hover));
            ui.painter().circle_filled(
                center + egui::vec2(0.0, 2.0),
                23.0,
                egui::Color32::from_black_alpha(90).gamma_multiply(hover),
            );
            ui.painter()
                .circle_filled(center, 22.0, pal().accent.gamma_multiply(hover));
            paint_icon(
                ui.painter(),
                Icon::Play,
                center + egui::vec2(1.0, 0.0),
                16.0,
                pal().on_accent.gamma_multiply(hover),
            );
        }
        paint_text(
            ui.painter(),
            egui::pos2(art.left(), art.bottom() + 10.0),
            egui::Align2::LEFT_TOP,
            title,
            font_bold(14.0),
            pal().text,
            art_size,
        );
        paint_text(
            ui.painter(),
            egui::pos2(art.left(), art.bottom() + 31.0),
            egui::Align2::LEFT_TOP,
            subtitle,
            egui::FontId::proportional(12.0),
            pal().muted,
            art_size,
        );
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    }

    fn card_grid_columns(available_width: f32, gap: f32) -> (usize, f32) {
        let columns = ((available_width + gap) / (180.0 + gap)).floor().clamp(1.0, 6.0) as usize;
        let width = (available_width - gap * (columns as f32 - 1.0)) / columns as f32;
        (columns, width)
    }

    fn draw_mixes(&mut self, ui: &mut egui::Ui) {
        Self::section_heading(ui, "Made for you");
        if self.connection_state != ConnectionState::Ready {
            Self::muted_note(ui, "Sign in to load recommendations based on your Spotify listening.");
            return;
        }
        if !self.recommendations_loaded {
            Self::muted_note(ui, "Loading your Spotify recommendations…");
            return;
        }
        if self.mixes.is_empty() {
            Self::muted_note(
                ui,
                "Spotify needs a little more listening history before making recommendations.",
            );
            return;
        }
        let gap = 8.0;
        let (columns, width) = Self::card_grid_columns(ui.available_width(), gap);
        let indices = (0..self.mixes.len()).collect::<Vec<_>>();
        for row in indices.chunks(columns) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for &index in row {
                    let mix = self.mixes[index].clone();
                    let image_url = self
                        .tracks
                        .get(mix.track_index)
                        .and_then(|track| track.image_url.clone());
                    if self
                        .draw_media_card(
                            ui,
                            width,
                            &mix.artwork,
                            image_url.as_deref(),
                            &mix.title,
                            &mix.subtitle,
                            true,
                        )
                        .clicked()
                    {
                        self.play_track(mix.track_index);
                    }
                }
            });
        }
    }

    // ----------------------------------------------------------------------
    // Track table
    // ----------------------------------------------------------------------

    fn track_columns(width: f32) -> TrackColumns {
        let right = 104.0;
        let album = if width > 640.0 {
            ((width - 48.0 - right) * 0.36).max(0.0)
        } else {
            0.0
        };
        TrackColumns { width, album }
    }

    fn draw_track_table_header(ui: &mut egui::Ui, columns: TrackColumns) {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(columns.width, 34.0), egui::Sense::hover());
        let painter = ui.painter();
        let color = pal().subtle;
        let font = font_medium(12.0);
        let y = rect.center().y;
        painter.text(egui::pos2(rect.left() + 24.0, y), egui::Align2::CENTER_CENTER, "#", font.clone(), color);
        painter.text(egui::pos2(rect.left() + 48.0, y), egui::Align2::LEFT_CENTER, "Title", font.clone(), color);
        if columns.album > 0.0 {
            painter.text(
                egui::pos2(columns.album_left(rect), y),
                egui::Align2::LEFT_CENTER,
                "Album",
                font,
                color,
            );
        }
        paint_icon(painter, Icon::Clock, egui::pos2(rect.right() - 32.0, y), 14.0, color);
        painter.hline(rect.x_range(), rect.bottom(), egui::Stroke::new(1.0, pal().border));
        ui.add_space(8.0);
    }

    fn draw_track_table(&mut self, ui: &mut egui::Ui, indices: &[usize], scroll: bool) {
        let columns = Self::track_columns(ui.available_width());
        Self::draw_track_table_header(ui, columns);
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            if scroll {
                egui::ScrollArea::vertical()
                    .id_salt("track_table")
                    .auto_shrink([false, false])
                    .show_rows(ui, TRACK_ROW_HEIGHT, indices.len(), |ui, rows| {
                        for row in rows {
                            if let Some(&index) = indices.get(row) {
                                self.draw_track_row(ui, index, columns);
                            }
                        }
                    });
            } else {
                for &index in indices {
                    self.draw_track_row(ui, index, columns);
                }
            }
        });
    }

    fn draw_track_row(&mut self, ui: &mut egui::Ui, index: usize, columns: TrackColumns) {
        let track = self.tracks[index].clone();
        let liked = self.liked_tracks.contains(&index);
        let current = self.is_current_track(index);
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(columns.width, TRACK_ROW_HEIGHT),
            egui::Sense::click(),
        );
        let hovered = ui.rect_contains_pointer(rect);
        let hover = animate(ui, response.id.with("hover"), hovered, ANIM_FAST);
        let current_t = animate(ui, response.id.with("current"), current, ANIM_MEDIUM);
        if hover > 0.0 {
            ui.painter()
                .rect_filled(rect, RADIUS_SM, pal().surface_raised.gamma_multiply(hover));
        }
        let y = rect.center().y;

        // Cross-fade the index/equalizer with the hover play icon.
        let index_center = egui::pos2(rect.left() + 24.0, y);
        if hover > 0.0 {
            paint_icon(ui.painter(), Icon::Play, index_center, 13.0, pal().text.gamma_multiply(hover));
        }
        if hover < 1.0 {
            let rest = 1.0 - hover;
            if current {
                let time = self.playing.then(|| ui.input(|input| input.time));
                if time.is_some() {
                    ui.ctx().request_repaint_after(EQUALIZER_FRAME);
                }
                paint_equalizer(
                    ui.painter(),
                    index_center,
                    14.0,
                    pal().accent.gamma_multiply(rest * current_t),
                    time,
                );
            }
            if current_t < 1.0 {
                ui.painter().text(
                    index_center,
                    egui::Align2::CENTER_CENTER,
                    (index + 1).to_string(),
                    egui::FontId::proportional(14.0),
                    pal().muted.gamma_multiply(rest * (1.0 - current_t)),
                );
            }
        }

        let art = egui::Rect::from_min_size(egui::pos2(rect.left() + 48.0, y - 20.0), egui::vec2(40.0, 40.0));
        self.paint_artwork(
            ui,
            art,
            &track.artwork,
            track.image_url.as_deref(),
            egui::CornerRadius::same(RADIUS_XS),
        );
        let text_x = art.right() + 12.0;
        let title_right = if columns.album > 0.0 {
            columns.album_left(rect) - 16.0
        } else {
            rect.right() - 104.0
        };
        let painter = ui.painter();
        paint_text(
            painter,
            egui::pos2(text_x, y - 1.0),
            egui::Align2::LEFT_BOTTOM,
            &track.title,
            font_medium(14.0),
            mix(pal().text, pal().accent, current_t),
            title_right - text_x,
        );
        paint_text(
            painter,
            egui::pos2(text_x, y + 2.0),
            egui::Align2::LEFT_TOP,
            &track.artist,
            egui::FontId::proportional(12.0),
            mix(pal().muted, pal().text, hover),
            title_right - text_x,
        );
        if columns.album > 0.0 {
            paint_text(
                painter,
                egui::pos2(columns.album_left(rect), y),
                egui::Align2::LEFT_CENTER,
                &track.album,
                egui::FontId::proportional(13.0),
                pal().muted,
                columns.album - 16.0,
            );
        }
        // On hover the duration makes way for a "more" button with the song's actions.
        let more_rect = egui::Rect::from_center_size(egui::pos2(rect.right() - 30.0, y), egui::vec2(30.0, 30.0));
        let more_id = ui.id().with(("more", index));
        let menu_open = egui::Popup::is_id_open(ui.ctx(), more_id.with("popup"));
        if !hovered && !menu_open {
            painter.text(
                egui::pos2(rect.right() - 16.0, y),
                egui::Align2::RIGHT_CENTER,
                &track.duration,
                egui::FontId::proportional(13.0),
                pal().muted,
            );
        }
        let track_id = self.spotify_id_for(index);
        let mut more_clicked = false;
        if hovered || menu_open {
            let more = icon_button_at(ui, more_id, more_rect, Icon::Dots, 16.0, false)
                .on_hover_text("More options");
            more_clicked = more.clicked();
            egui::Popup::from_toggle_button_response(&more)
                .kind(egui::PopupKind::Menu)
                .align(egui::RectAlign::BOTTOM_END)
                .gap(4.0)
                .width(220.0)
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .frame(menu_frame())
                .show(|ui| self.track_menu(ui, &track, track_id.as_deref()));
        }
        egui::Popup::context_menu(&response)
            .width(220.0)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .frame(menu_frame())
            .show(|ui| self.track_menu(ui, &track, track_id.as_deref()));

        let mut like_clicked = more_clicked;
        let like_id = ui.id().with(("like", index));
        let like_scale = pop_scale(ui, like_id.with("pop"), liked, 0.35);
        let heart_alpha = if liked { 1.0 } else { hover };
        if liked || hovered {
            let like_rect = egui::Rect::from_center_size(
                egui::pos2(rect.right() - 80.0, y),
                egui::vec2(30.0, 30.0),
            );
            let like_response = ui.interact(like_rect, like_id, egui::Sense::click());
            let color = if liked {
                pal().accent
            } else {
                mix(pal().muted, pal().text, hover_t(ui, &like_response))
            };
            paint_icon(
                ui.painter(),
                if liked { Icon::HeartFilled } else { Icon::Heart },
                like_rect.center(),
                16.0 * like_scale,
                color.gamma_multiply(heart_alpha),
            );
            let like_response = like_response
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text(if liked {
                    "Remove from your Liked Songs"
                } else {
                    "Save to your Liked Songs"
                });
            if like_response.clicked() {
                like_clicked = true;
                self.toggle_like(index);
            }
        } else if hover > 0.0 {
            // Let the outline heart fade out instead of vanishing with the hover.
            paint_icon(
                ui.painter(),
                Icon::Heart,
                egui::pos2(rect.right() - 80.0, y),
                16.0,
                pal().muted.gamma_multiply(hover),
            );
        }

        if response.clicked() && !like_clicked {
            self.play_track(index);
        }
    }

    fn draw_compact_track(&mut self, ui: &mut egui::Ui, track: &Track, current: bool) -> egui::Response {
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), 56.0),
            egui::Sense::click(),
        );
        let hover = hover_t(ui, &response);
        if hover > 0.0 {
            ui.painter()
                .rect_filled(rect, RADIUS_SM, pal().surface_raised.gamma_multiply(hover));
        }
        let art = egui::Rect::from_min_size(rect.left_top() + egui::vec2(8.0, 8.0), egui::vec2(40.0, 40.0));
        self.paint_artwork(
            ui,
            art,
            &track.artwork,
            track.image_url.as_deref(),
            egui::CornerRadius::same(RADIUS_XS),
        );
        let painter = ui.painter();
        let duration = painter.text(
            rect.right_center() - egui::vec2(10.0, 0.0),
            egui::Align2::RIGHT_CENTER,
            &track.duration,
            egui::FontId::proportional(12.0),
            pal().subtle,
        );
        let text_x = art.right() + 12.0;
        let max_width = duration.left() - text_x - 10.0;
        paint_text(
            painter,
            egui::pos2(text_x, rect.center().y - 1.0),
            egui::Align2::LEFT_BOTTOM,
            &track.title,
            font_medium(13.0),
            if current { pal().accent } else { pal().text },
            max_width,
        );
        paint_text(
            painter,
            egui::pos2(text_x, rect.center().y + 2.0),
            egui::Align2::LEFT_TOP,
            &track.artist,
            egui::FontId::proportional(12.0),
            pal().muted,
            max_width,
        );
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    }

    // ----------------------------------------------------------------------
    // Collections (Liked Songs, playlists, library)
    // ----------------------------------------------------------------------

    fn draw_collection_header(
        &mut self,
        ui: &mut egui::Ui,
        art: Option<(Artwork, Option<String>)>,
        kind: &str,
        title: &str,
        subtitle: &str,
    ) {
        let height = 168.0;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::hover());
        let art_rect = egui::Rect::from_min_size(rect.min, egui::vec2(height, height));
        let radius = egui::CornerRadius::same(RADIUS_MD);
        match art {
            Some((artwork, url)) => self.paint_artwork(ui, art_rect, &artwork, url.as_deref(), radius),
            None => Self::paint_liked_tile(ui.painter(), art_rect, radius),
        }
        let painter = ui.painter();
        let text_x = art_rect.right() + 24.0;
        let max_width = rect.right() - text_x;
        let title_size = if title.chars().count() > 22 { 34.0 } else { 46.0 };
        painter.text(
            egui::pos2(text_x, rect.bottom() - 24.0 - title_size - 14.0),
            egui::Align2::LEFT_BOTTOM,
            kind,
            font_medium(13.0),
            pal().text,
        );
        paint_text(
            painter,
            egui::pos2(text_x, rect.bottom() - 30.0),
            egui::Align2::LEFT_BOTTOM,
            title,
            font_light(title_size),
            pal().text,
            max_width,
        );
        paint_text(
            painter,
            egui::pos2(text_x, rect.bottom() - 4.0),
            egui::Align2::LEFT_BOTTOM,
            subtitle,
            egui::FontId::proportional(13.0),
            pal().muted,
            max_width,
        );
        ui.add_space(20.0);
    }

    fn draw_playlist_page(&mut self, ui: &mut egui::Ui, name: &str) {
        if text_icon_button(ui, Icon::ChevronLeft, "Playlists").clicked() {
            self.playlist_name = None;
            self.playlist_loading = false;
            self.playlist_error = None;
            return;
        }
        ui.add_space(12.0);
        let playlist = self.playlists.iter().find(|playlist| playlist.title == name).cloned();
        let subtitle = if self.playlist_loading {
            "Loading tracks…".to_owned()
        } else {
            format!("{} songs", self.tracks.len())
        };
        let art = playlist
            .as_ref()
            .map(|playlist| (playlist.artwork, playlist.image_url.clone()))
            .or(Some((Self::artwork_for_index(0), None)));
        self.draw_collection_header(ui, art, "Playlist", name, &subtitle);

        if self.playlist_loading {
            Self::muted_note(ui, "Loading playlist tracks…");
        } else if let Some(error) = self.playlist_error.clone() {
            Self::muted_note(ui, &error);
        } else if self.tracks.is_empty() {
            Self::muted_note(ui, "This playlist has no playable tracks.");
        } else {
            if play_circle_button(ui, 52.0).on_hover_text("Play").clicked() {
                self.play_track(0);
            }
            ui.add_space(16.0);
            let indices = (0..self.tracks.len()).collect::<Vec<_>>();
            self.draw_track_table(ui, &indices, true);
        }
    }

    fn draw_liked_songs(&mut self, ui: &mut egui::Ui) {
        if self.liked_songs_loaded
            && !self.liked_songs.is_empty()
            && (self.tracks.len() != self.liked_songs.len()
                || self.spotify_ids != self.liked_spotify_ids)
        {
            self.activate_liked_songs();
        }
        let subtitle = if self.liked_songs_loaded {
            format!("{} songs", self.liked_songs.len())
        } else {
            "Every track you’ve saved, in one place".to_owned()
        };
        self.draw_collection_header(ui, None, "Playlist", "Liked Songs", &subtitle);

        let ready = self.connection_state == ConnectionState::Ready;
        ui.horizontal(|ui| {
            if !self.liked_songs.is_empty()
                && play_circle_button(ui, 52.0).on_hover_text("Play all").clicked()
            {
                self.play_track(0);
            }
            if ready && self.liked_songs_loaded {
                ui.add_space(8.0);
                if pill_button(ui, "Refresh", ButtonKind::Ghost).clicked() {
                    self.liked_songs_loaded = false;
                    self.liked_songs_error = None;
                    self.spotify.load_liked_songs();
                }
            }
        });
        ui.add_space(16.0);

        if let Some(error) = self.liked_songs_error.clone() {
            let mut retry = false;
            egui::Frame::new()
                .fill(pal().surface_raised)
                .corner_radius(RADIUS_MD)
                .inner_margin(egui::Margin::symmetric(16, 12))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.set_max_width(ui.available_width() - 100.0);
                            ui.label(
                                egui::RichText::new("Liked Songs could not be loaded")
                                    .font(font_medium(13.0))
                                    .color(pal().warning),
                            );
                            ui.label(egui::RichText::new(&error).size(12.0).color(pal().muted));
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            retry = pill_button(ui, "Retry", ButtonKind::Secondary).clicked();
                        });
                    });
                });
            if retry {
                self.liked_songs_loaded = false;
                self.liked_songs_error = None;
                self.spotify.load_liked_songs();
            }
            ui.add_space(14.0);
        }

        if !ready {
            Self::muted_note(ui, "Sign in to load your saved Spotify tracks.");
        } else if !self.liked_songs_loaded {
            if self.liked_songs_error.is_none() {
                Self::muted_note(ui, "Loading your Liked Songs…");
            }
        } else if self.liked_songs.is_empty() {
            Self::muted_note(ui, "You haven’t saved any Spotify tracks yet.");
        } else {
            let indices = (0..self.tracks.len()).collect::<Vec<_>>();
            self.draw_track_table(ui, &indices, true);
        }
    }

    /// Square cards in a grid, such as albums or artists. Returns the clicked index.
    fn draw_card_grid(&mut self, ui: &mut egui::Ui, cards: &[(String, String, Option<String>)]) -> Option<usize> {
        let gap = 8.0;
        let (columns, width) = Self::card_grid_columns(ui.available_width(), gap);
        let mut clicked = None;
        let indices = (0..cards.len()).collect::<Vec<_>>();
        for row in indices.chunks(columns) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for &index in row {
                    let (title, subtitle, image_url) = &cards[index];
                    if self
                        .draw_media_card(
                            ui,
                            width,
                            &Self::artwork_for_index(index),
                            image_url.as_deref(),
                            title,
                            subtitle,
                            false,
                        )
                        .clicked()
                    {
                        clicked = Some(index);
                    }
                }
            });
        }
        clicked
    }

    fn draw_detail_page(&mut self, ui: &mut egui::Ui) {
        let Some(detail) = self.detail.clone() else {
            return;
        };
        if text_icon_button(ui, Icon::ChevronLeft, "Your Library").clicked() {
            self.detail = None;
            return;
        }
        ui.add_space(12.0);
        let song_count = |count: usize| {
            if count == 1 { "1 song".to_owned() } else { format!("{count} songs") }
        };
        let (loading, error) = match &detail {
            DetailPage::Artist { loading, error, .. }
            | DetailPage::Album { loading, error, .. }
            | DetailPage::Radio { loading, error, .. }
            | DetailPage::Top { loading, error, .. } => (*loading, error.clone()),
        };
        let mut tracks_heading = None;
        match &detail {
            DetailPage::Artist { name, image_url, .. } => {
                self.draw_collection_header(
                    ui,
                    Some((Self::artwork_for_index(1), image_url.clone())),
                    "Artist",
                    name,
                    "Popular songs, albums and singles",
                );
                tracks_heading = Some("Popular");
            }
            DetailPage::Album {
                name,
                artist,
                artist_id,
                year,
                image_url,
                ..
            } => {
                let mut subtitle = artist.clone();
                if let Some(year) = year {
                    subtitle.push_str(&format!(" · {year}"));
                }
                if self.detail_has_tracks {
                    subtitle.push_str(&format!(" · {}", song_count(self.tracks.len())));
                }
                self.draw_collection_header(
                    ui,
                    Some((Self::artwork_for_index(2), image_url.clone())),
                    "Album",
                    name,
                    &subtitle,
                );
                if let Some(artist_id) = artist_id.clone()
                    && text_link(ui, &format!("More by {}", artist.split(", ").next().unwrap_or_default()))
                        .clicked()
                {
                    let name = artist.split(", ").next().unwrap_or_default().to_owned();
                    self.open_artist(artist_id, name);
                    return;
                }
                ui.add_space(8.0);
            }
            DetailPage::Radio {
                title,
                artist,
                image_url,
                ..
            } => {
                self.draw_collection_header(
                    ui,
                    Some((Self::artwork_for_index(3), image_url.clone())),
                    "Song radio",
                    &format!("{title} Radio"),
                    &format!("Songs like {title} by {artist}, picked by Spotify"),
                );
            }
            DetailPage::Top { range, artists, .. } => {
                Self::page_title(ui, "Your top songs and artists");
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    for option in TopRange::ALL {
                        if chip(ui, option.label(), option == *range).clicked() && option != *range {
                            self.open_top(option);
                        }
                    }
                });
                ui.add_space(24.0);
                if !loading && !artists.is_empty() {
                    Self::section_heading(ui, "Top artists");
                    let cards = artists
                        .iter()
                        .take(12)
                        .map(|artist| (artist.name.clone(), "Artist".to_owned(), artist.image_url.clone()))
                        .collect::<Vec<_>>();
                    if let Some(index) = self.draw_card_grid(ui, &cards) {
                        let artist = artists[index].clone();
                        self.open_artist(artist.id, artist.name);
                        return;
                    }
                    ui.add_space(28.0);
                }
                tracks_heading = Some("Top songs");
            }
        }

        if loading {
            Self::muted_note(ui, "Loading…");
            return;
        }
        if let Some(error) = &error {
            Self::muted_note(ui, error);
        }
        if self.detail_has_tracks && !self.tracks.is_empty() {
            if play_circle_button(ui, 52.0).on_hover_text("Play").clicked() {
                self.play_track(0);
            }
            ui.add_space(16.0);
            if let Some(heading) = tracks_heading {
                Self::section_heading(ui, heading);
            }
            let indices = (0..self.tracks.len()).collect::<Vec<_>>();
            self.draw_track_table(ui, &indices, false);
        }
        if let DetailPage::Artist { albums, .. } = &detail
            && !albums.is_empty()
        {
            ui.add_space(32.0);
            Self::section_heading(ui, "Albums and singles");
            let cards = albums
                .iter()
                .map(|album| {
                    let subtitle = album
                        .year
                        .map(|year| year.to_string())
                        .unwrap_or_else(|| album.artist.clone());
                    (album.name.clone(), subtitle, album.image_url.clone())
                })
                .collect::<Vec<_>>();
            if let Some(index) = self.draw_card_grid(ui, &cards) {
                let album = albums[index].clone();
                self.open_album(album.id, album.name, album.artist);
            }
        }
        ui.add_space(24.0);
    }

    fn draw_library(&mut self, ui: &mut egui::Ui) {
        if self.detail.is_some() {
            self.draw_detail_page(ui);
            return;
        }
        if let Some(name) = self.playlist_name.clone() {
            self.draw_playlist_page(ui, &name);
            return;
        }
        let ready = self.connection_state == ConnectionState::Ready;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Your Library").font(font_light(32.0)).color(pal().text));
            if ready {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if pill_button(ui, "Refresh", ButtonKind::Ghost).clicked() {
                        self.spotify.load_playlists();
                    }
                });
            }
        });
        ui.add_space(18.0);

        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), 96.0),
            egui::Sense::click(),
        );
        ui.painter().rect_filled(
            rect,
            RADIUS_LG,
            if response.hovered() { pal().surface_hover } else { pal().surface_raised },
        );
        let art = egui::Rect::from_min_size(rect.min + egui::vec2(16.0, 16.0), egui::vec2(64.0, 64.0));
        Self::paint_liked_tile(ui.painter(), art, egui::CornerRadius::same(RADIUS_SM));
        ui.painter().text(
            egui::pos2(art.right() + 18.0, rect.center().y - 2.0),
            egui::Align2::LEFT_BOTTOM,
            "Liked Songs",
            font_bold(18.0),
            pal().text,
        );
        let liked_count = self.liked_songs.len();
        ui.painter().text(
            egui::pos2(art.right() + 18.0, rect.center().y + 3.0),
            egui::Align2::LEFT_TOP,
            if liked_count > 0 {
                format!("{liked_count} saved songs")
            } else {
                "Everything you save, in one place".to_owned()
            },
            egui::FontId::proportional(13.0),
            pal().muted,
        );
        paint_icon(
            ui.painter(),
            Icon::ChevronRight,
            rect.right_center() - egui::vec2(28.0, 0.0),
            16.0,
            if response.hovered() { pal().text } else { pal().muted },
        );
        if response
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            self.open_liked_songs();
        }

        ui.add_space(10.0);
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), 96.0),
            egui::Sense::click(),
        );
        ui.painter().rect_filled(
            rect,
            RADIUS_LG,
            if response.hovered() { pal().surface_hover } else { pal().surface_raised },
        );
        let art = egui::Rect::from_min_size(rect.min + egui::vec2(16.0, 16.0), egui::vec2(64.0, 64.0));
        ui.painter().rect_filled(art, RADIUS_SM, pal().accent_dark);
        paint_icon(ui.painter(), Icon::Sparkle, art.center(), 26.0, pal().accent);
        ui.painter().text(
            egui::pos2(art.right() + 18.0, rect.center().y - 2.0),
            egui::Align2::LEFT_BOTTOM,
            "Your top songs and artists",
            font_bold(18.0),
            pal().text,
        );
        ui.painter().text(
            egui::pos2(art.right() + 18.0, rect.center().y + 3.0),
            egui::Align2::LEFT_TOP,
            "What you’ve played most over the last month, six months and year",
            egui::FontId::proportional(13.0),
            pal().muted,
        );
        paint_icon(
            ui.painter(),
            Icon::ChevronRight,
            rect.right_center() - egui::vec2(28.0, 0.0),
            16.0,
            if response.hovered() { pal().text } else { pal().muted },
        );
        if response
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            self.open_top(TopRange::Short);
        }

        ui.add_space(32.0);
        self.draw_mixes(ui);
        ui.add_space(32.0);
        Self::section_heading(ui, "Playlists");
        if !ready {
            Self::muted_note(ui, "Sign in to load your Spotify playlists.");
        } else if !self.playlists_loaded {
            Self::muted_note(ui, "Loading your Spotify playlists…");
        } else if self.playlists.is_empty() {
            Self::muted_note(ui, "Your Spotify account has no playlists yet.");
        } else {
            let gap = 8.0;
            let (columns, width) = Self::card_grid_columns(ui.available_width(), gap);
            let indices = (0..self.playlists.len()).collect::<Vec<_>>();
            for row in indices.chunks(columns) {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    for &index in row {
                        let playlist = self.playlists[index].clone();
                        let subtitle = if playlist.description.trim().is_empty() {
                            "Playlist"
                        } else {
                            playlist.description.as_str()
                        };
                        if self
                            .draw_media_card(
                                ui,
                                width,
                                &playlist.artwork,
                                playlist.image_url.as_deref(),
                                &playlist.title,
                                subtitle,
                                false,
                            )
                            .clicked()
                        {
                            self.open_playlist(&playlist);
                        }
                    }
                });
            }
        }
    }

    // ----------------------------------------------------------------------
    // Search, queue, settings
    // ----------------------------------------------------------------------

    fn draw_search_field(&mut self, ui: &mut egui::Ui) {
        let width = ui.available_width().min(520.0);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 46.0), egui::Sense::hover());
        let edit_id = egui::Id::new("search_field");
        let focused = ui.memory(|memory| memory.has_focus(edit_id));
        let hovered = ui.rect_contains_pointer(rect);
        ui.painter().rect(
            rect,
            23,
            if focused || hovered { pal().surface_hover } else { pal().surface_raised },
            egui::Stroke::new(1.0, if focused { pal().subtle } else { egui::Color32::TRANSPARENT }),
            egui::StrokeKind::Inside,
        );
        paint_icon(
            ui.painter(),
            Icon::Search,
            rect.left_center() + egui::vec2(22.0, 0.0),
            16.0,
            if focused { pal().text } else { pal().muted },
        );
        let has_query = !self.search.is_empty();
        let edit_rect = egui::Rect::from_min_max(
            egui::pos2(rect.left() + 42.0, rect.top()),
            egui::pos2(rect.right() - if has_query { 42.0 } else { 18.0 }, rect.bottom()),
        );
        let response = ui.put(
            edit_rect,
            egui::TextEdit::singleline(&mut self.search)
                .id(edit_id)
                .hint_text("What do you want to play?")
                .font(egui::FontId::proportional(14.5))
                .frame(egui::Frame::new())
                .vertical_align(egui::Align::Center)
                .desired_width(f32::INFINITY),
        );
        if response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            let query = self.search.trim().to_owned();
            if !query.is_empty() && self.connection_state == ConnectionState::Ready {
                self.spotify.search(query);
            }
        }
        if has_query {
            let clear = egui::Rect::from_center_size(
                rect.right_center() - egui::vec2(22.0, 0.0),
                egui::vec2(30.0, 30.0),
            );
            if icon_button_at(ui, edit_id.with("clear"), clear, Icon::Close, 12.0, false)
                .on_hover_text("Clear search")
                .clicked()
            {
                self.search.clear();
            }
        }
    }

    fn draw_search(&mut self, ui: &mut egui::Ui) {
        Self::page_title(ui, "Search");
        self.draw_search_field(ui);
        ui.add_space(24.0);
        let query = self.search.trim().to_lowercase();
        if query.is_empty() {
            egui::Frame::new()
                .fill(pal().surface_raised)
                .corner_radius(RADIUS_LG)
                .inner_margin(egui::Margin::same(24))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        egui::RichText::new("Find your next repeat")
                            .font(font_bold(22.0))
                            .color(pal().text),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(
                            "Search your library by track, artist, or album. Press Enter to search all of Spotify.",
                        )
                        .size(13.0)
                        .color(pal().muted),
                    );
                    ui.add_space(16.0);
                    ui.horizontal_wrapped(|ui| {
                        for suggestion in ["electronic", "focus", "late night", "new releases"] {
                            if chip(ui, suggestion, false).clicked() {
                                self.search = suggestion.to_owned();
                            }
                        }
                    });
                });
            ui.add_space(32.0);
            Self::section_heading(ui, "Recently played");
            let indices = (0..self.tracks.len().min(10)).collect::<Vec<_>>();
            if !indices.is_empty() {
                self.draw_track_table(ui, &indices, false);
            }
            return;
        }

        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(
                    egui::RichText::new(format!("Results for “{}”", self.search.trim()))
                        .font(font_bold(18.0))
                        .color(pal().text),
                );
                ui.label(
                    egui::RichText::new("Matching tracks from your current selection")
                        .size(12.0)
                        .color(pal().muted),
                );
            });
            if self.connection_state == ConnectionState::Ready {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if pill_button(ui, "Search Spotify", ButtonKind::Primary).clicked() {
                        self.spotify.search(query.clone());
                    }
                });
            }
        });
        ui.add_space(18.0);
        let matching = self
            .tracks
            .iter()
            .enumerate()
            .filter_map(|(index, track)| Self::track_matches(track, &query).then_some(index))
            .collect::<Vec<_>>();
        if matching.is_empty() {
            Self::muted_note(
                ui,
                "No tracks match that search yet. Press Enter or use Search Spotify to look further.",
            );
        } else {
            self.draw_track_table(ui, &matching, true);
        }
    }

    fn play_queue_track(&mut self, index: usize) {
        if self.queue_active {
            let Some(track_id) = self.queue_spotify_ids.get(&index).cloned() else {
                return;
            };
            self.queue_selected = index;
            self.playing = true;
            self.progress = 0.0;
            if self.connection_state == ConnectionState::Ready {
                let queue = (0..self.queue_tracks.len())
                    .filter_map(|index| self.queue_spotify_ids.get(&index).cloned())
                    .collect();
                self.spotify.load(track_id, queue, true, 0);
            }
        } else {
            self.play_track(index);
        }
    }

    fn queue_track(&self, index: usize) -> Option<Track> {
        if self.queue_active && !self.queue_tracks.is_empty() {
            self.queue_tracks.get(index).cloned()
        } else {
            self.tracks.get(index).cloned()
        }
    }

    fn draw_queue(&mut self, ui: &mut egui::Ui) {
        let queue_len = if self.queue_active && !self.queue_tracks.is_empty() {
            self.queue_tracks.len()
        } else {
            self.tracks.len()
        };
        let source = if self.queue_active {
            self.queue_source.clone()
        } else {
            "Current selection".to_owned()
        };
        if text_icon_button(ui, Icon::ChevronLeft, "Back").clicked() {
            self.section = self.queue_return_section;
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Queue").font(font_light(32.0)).color(pal().text));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(format!("{queue_len} tracks"))
                        .size(13.0)
                        .color(pal().muted),
                );
            });
        });
        ui.label(egui::RichText::new(format!("Playing from {source}")).size(12.0).color(pal().muted));
        ui.add_space(16.0);
        if queue_len == 0 {
            Self::muted_note(ui, "Your playback queue is empty.");
            return;
        }
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            egui::ScrollArea::vertical()
                .id_salt("queue_page")
                .auto_shrink([false, false])
                .show_rows(ui, 56.0, queue_len, |ui, rows| {
                    for index in rows {
                        let Some(track) = self.queue_track(index) else {
                            continue;
                        };
                        let current = self.queue_active && self.queue_selected == index;
                        if self.draw_compact_track(ui, &track, current).clicked() {
                            self.play_queue_track(index);
                        }
                    }
                });
        });
    }

    fn draw_connection_banner(&mut self, ui: &mut egui::Ui) {
        if self.connection_state == ConnectionState::Ready {
            let Some(error) = self.connection_error.clone() else {
                return;
            };
            let mut dismiss = false;
            egui::Frame::new()
                .fill(pal().surface_raised)
                .corner_radius(RADIUS_MD)
                .inner_margin(egui::Margin::symmetric(16, 10))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let (dot, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                        ui.painter().circle_filled(dot.center(), 4.0, pal().warning);
                        ui.vertical(|ui| {
                            ui.set_max_width(ui.available_width() - 110.0);
                            ui.label(
                                egui::RichText::new("Spotify data issue")
                                    .font(font_medium(13.0))
                                    .color(pal().text),
                            );
                            ui.label(egui::RichText::new(error).size(12.0).color(pal().muted));
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            dismiss = pill_button(ui, "Dismiss", ButtonKind::Ghost).clicked();
                        });
                    });
                });
            if dismiss {
                self.connection_error = None;
            }
            ui.add_space(18.0);
            return;
        }

        let state = self.connection_state;
        let error = self.connection_error.clone();
        let (title, detail, action) = match state {
            ConnectionState::Authenticating => (
                "Opening Spotify sign-in",
                "Your browser will ask you to authorize Oynx.",
                "Waiting for Spotify…",
            ),
            ConnectionState::Connecting => (
                "Connecting to Spotify",
                "Setting up your secure playback session.",
                "Connecting…",
            ),
            ConnectionState::Disconnected => (
                "Connect Spotify to listen",
                "Sign in securely in your browser. Oynx never sees your Spotify password.",
                "Sign in with Spotify",
            ),
            ConnectionState::Ready => unreachable!(),
        };

        egui::Frame::new()
            .fill(pal().surface_raised)
            .corner_radius(RADIUS_LG)
            .inner_margin(egui::Margin::symmetric(18, 16))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(44.0, 44.0), egui::Sense::hover());
                    ui.painter().circle_filled(rect.center(), 22.0, pal().accent_dark);
                    paint_icon(ui.painter(), Icon::Note, rect.center(), 20.0, pal().accent);
                    ui.add_space(8.0);
                    ui.vertical(|ui| {
                        ui.set_max_width(ui.available_width() - 200.0);
                        ui.label(egui::RichText::new(title).font(font_bold(15.0)).color(pal().text));
                        ui.label(
                            egui::RichText::new(error.as_deref().unwrap_or(detail))
                                .size(12.0)
                                .color(if error.is_some() { pal().danger } else { pal().muted }),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let kind = if state == ConnectionState::Disconnected {
                            ButtonKind::Primary
                        } else {
                            ButtonKind::Secondary
                        };
                        if pill_button(ui, action, kind).clicked()
                            && state == ConnectionState::Disconnected
                        {
                            self.connection_error = None;
                            self.spotify.login_with_config(
                                Some(self.spotify_client_id.clone()),
                                Some(self.spotify_redirect_uri.clone()),
                                Some(self.spotify_web_api_client_id.clone()),
                                Some(self.spotify_web_api_redirect_uri.clone()),
                            );
                        }
                    });
                });
            });
        ui.add_space(24.0);
    }

    /// A short confirmation floating under the title bar that fades after a few seconds.
    fn draw_notice(&mut self, ui: &mut egui::Ui) {
        const SHOWN_FOR: f32 = 3.0;
        let Some((message, shown_at)) = self.notice.clone() else {
            return;
        };
        let age = shown_at.elapsed().as_secs_f32();
        if age >= SHOWN_FOR {
            self.notice = None;
            return;
        }
        ui.ctx().request_repaint_after(Duration::from_millis(100));
        let fade = ((SHOWN_FOR - age) / 0.4).clamp(0.0, 1.0);
        egui::Area::new(egui::Id::new("oynx_notice"))
            .order(egui::Order::Foreground)
            .interactable(false)
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, TITLE_BAR_HEIGHT + 12.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::new()
                    .fill(pal().surface_raised.gamma_multiply(fade))
                    .stroke(egui::Stroke::new(1.0, pal().border.gamma_multiply(fade)))
                    .corner_radius(RADIUS_LG)
                    .inner_margin(egui::Margin::symmetric(16, 9))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (dot, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                            ui.painter().circle_filled(dot.center(), 4.0, pal().accent.gamma_multiply(fade));
                            ui.label(
                                egui::RichText::new(message)
                                    .font(font_medium(13.0))
                                    .color(pal().text.gamma_multiply(fade)),
                            );
                        });
                    });
            });
    }

    fn settings_field(ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str) {
        ui.label(egui::RichText::new(label).font(font_medium(12.0)).color(pal().muted));
        ui.add_space(2.0);
        ui.add(
            egui::TextEdit::singleline(value)
                .hint_text(hint)
                .font(egui::FontId::proportional(14.0))
                .margin(egui::Margin::symmetric(12, 9))
                .desired_width(f32::INFINITY),
        );
        ui.add_space(14.0);
    }

    fn settings_card(ui: &mut egui::Ui, title: &str, add_contents: impl FnOnce(&mut egui::Ui)) {
        egui::Frame::new()
            .fill(pal().surface_raised.gamma_multiply(0.7))
            .stroke(egui::Stroke::new(1.0, pal().border))
            .corner_radius(RADIUS_LG)
            .inner_margin(egui::Margin::same(24))
            .show(ui, |ui| {
                ui.set_width(ui.available_width().min(680.0));
                ui.label(egui::RichText::new(title).font(font_bold(18.0)).color(pal().text));
                ui.add_space(6.0);
                add_contents(ui);
            });
    }

    fn draw_settings(&mut self, ui: &mut egui::Ui) {
        Self::page_title(ui, "Settings");
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            for (tab, label) in [
                (SettingsTab::General, "General"),
                (SettingsTab::Themes, "Themes"),
                (SettingsTab::Updates, "Updates"),
            ] {
                if chip(ui, label, self.settings_tab == tab).clicked() {
                    self.settings_tab = tab;
                }
            }
        });
        ui.add_space(20.0);
        match self.settings_tab {
            SettingsTab::General => self.draw_general_settings(ui),
            SettingsTab::Themes => self.draw_theme_settings(ui),
            SettingsTab::Updates => self.draw_update_settings(ui),
        }
    }

    fn draw_update_settings(&mut self, ui: &mut egui::Ui) {
        let status = self.updater.status();
        let blocker = updater::install_blocker();
        let ctx = ui.ctx().clone();
        let mut restart = false;
        Self::settings_card(ui, "Updates", |ui| {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(format!("Oynx {CURRENT_VERSION}"))
                    .font(font_medium(15.0))
                    .color(pal().text),
            );
            let checked = self.updater.last_checked().map(|at| {
                let minutes = at.elapsed().as_secs() / 60;
                match minutes {
                    0 => "just now".to_owned(),
                    1 => "1 minute ago".to_owned(),
                    m if m < 60 => format!("{m} minutes ago"),
                    m => format!("{} hours ago", m / 60),
                }
            });
            let (line, color) = match &status {
                UpdateStatus::Idle => (
                    checked.map_or("Not checked yet".to_owned(), |when| format!("Last checked {when}")),
                    pal().muted,
                ),
                UpdateStatus::Checking => ("Checking for updates…".to_owned(), pal().muted),
                UpdateStatus::UpToDate => (
                    format!("You're up to date · checked {}", checked.unwrap_or_else(|| "just now".to_owned())),
                    pal().muted,
                ),
                UpdateStatus::Available(release) => (format!("Version {} is available", release.version), pal().text),
                UpdateStatus::Downloading { release, downloaded, total } => (
                    format!(
                        "Downloading {}… {:.0}%",
                        release.version,
                        *downloaded as f32 / (*total).max(1) as f32 * 100.0
                    ),
                    pal().text,
                ),
                UpdateStatus::Ready(release) => (
                    format!(
                        "Version {} is downloaded and installs when you quit Oynx, or restart now to update straight away.",
                        release.version
                    ),
                    pal().text,
                ),
                UpdateStatus::Failed(error) => (error.clone(), pal().danger),
            };
            ui.label(egui::RichText::new(line).size(12.5).color(color));
            if let UpdateStatus::Downloading { downloaded, total, .. } = &status {
                ui.add_space(8.0);
                let (bar, _) = ui.allocate_exact_size(egui::vec2(ui.available_width().min(360.0), 3.0), egui::Sense::hover());
                ui.painter().rect_filled(bar, 2, pal().text.gamma_multiply(0.15));
                let mut filled = bar;
                filled.set_right(bar.left() + bar.width() * (*downloaded as f32 / (*total).max(1) as f32).min(1.0));
                ui.painter().rect_filled(filled, 2, pal().text);
            }
            ui.add_space(14.0);
            ui.horizontal(|ui| match &status {
                UpdateStatus::Checking | UpdateStatus::Downloading { .. } => {
                    pill_button(ui, "Working…", ButtonKind::Secondary);
                }
                UpdateStatus::Ready(_) => {
                    restart = pill_button(ui, "Restart now", ButtonKind::Primary).clicked();
                }
                UpdateStatus::Available(release) => {
                    if blocker.is_none()
                        && pill_button(ui, &format!("Download and install {}", release.version), ButtonKind::Primary).clicked()
                    {
                        self.updater.install(&ctx, release.clone());
                    }
                    if pill_button(ui, "Release notes", ButtonKind::Ghost).clicked() {
                        ctx.open_url(egui::OpenUrl::new_tab(&release.page_url));
                    }
                }
                _ => {
                    if pill_button(ui, "Check for updates", ButtonKind::Primary).clicked() {
                        self.updater.check(&ctx, false);
                    }
                    if pill_button(ui, "All releases", ButtonKind::Ghost).clicked() {
                        ctx.open_url(egui::OpenUrl::new_tab(updater::RELEASES_PAGE_URL));
                    }
                }
            });
            if let Some(reason) = &blocker {
                ui.add_space(10.0);
                ui.label(egui::RichText::new(reason).size(12.0).color(pal().subtle));
            }
        });
        if restart {
            self.restart_to_update(&ctx);
        }

        ui.add_space(16.0);
        Self::settings_card(ui, "Update mode", |ui| {
            ui.add_space(4.0);
            ui.spacing_mut().item_spacing.y = 4.0;
            for (mode, title, detail) in [
                (
                    UpdateMode::Automatic,
                    "Automatic",
                    "Check in the background and install new versions. They take effect the next time Oynx starts.",
                ),
                (
                    UpdateMode::Manual,
                    "Manual",
                    "Only check when you press Check for updates, and choose when to install.",
                ),
            ] {
                if option_row(ui, title, detail, self.prefs.update_mode == mode).clicked() {
                    self.prefs.update_mode = mode;
                }
            }
        });
    }

    /// Runs the downloaded installer and closes this copy; the installer starts
    /// the new version when it finishes.
    fn restart_to_update(&mut self, ctx: &egui::Context) {
        match self.updater.install_now() {
            Ok(()) => {
                self.save_session();
                self.quitting = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Err(error) => self.connection_error = Some(error),
        }
    }

    fn save_theme(&mut self) {
        if !self.theme_dirty {
            return;
        }
        match self.theme.save(&theme_path()) {
            Ok(()) => self.theme_dirty = false,
            Err(error) => log::warn!("{error}"),
        }
    }

    /// Paints the window background: the theme colour, the background image
    /// over it, both faded by the window opacity when the window is see-through.
    fn paint_background(&self, ui: &egui::Ui, rect: egui::Rect) {
        let opacity = if self.transparent_window { self.theme.window_opacity } else { 1.0 };
        let painter = ui.painter();
        painter.rect_filled(rect, 0, pal().background.gamma_multiply(opacity));
        if let Some(texture) = &self.background_texture {
            // Cover the window, cropping whichever side of the image overflows.
            let size = texture.size_vec2();
            let scale = (rect.width() / size.x).max(rect.height() / size.y);
            let visible = rect.size() / (size * scale);
            let uv = egui::Rect::from_center_size(egui::pos2(0.5, 0.5), visible);
            painter.image(
                texture.id(),
                rect,
                uv,
                egui::Color32::WHITE.gamma_multiply(self.theme.image_strength * opacity),
            );
        }
    }

    /// Loads the chosen background image off the UI thread, one load at a
    /// time, so dragging the blur slider never stalls a frame.
    fn update_background_image(&mut self, ctx: &egui::Context) {
        if let Some((key, receiver)) = &self.background_load {
            match receiver.try_recv() {
                Ok(result) => {
                    let key = key.clone();
                    self.background_load = None;
                    match result {
                        Ok(image) => {
                            self.background_texture =
                                Some(ctx.load_texture("theme-background", image, egui::TextureOptions::LINEAR));
                            self.background_key = Some(key);
                        }
                        Err(error) => {
                            self.theme_message = Some(error);
                            self.theme.background_image = None;
                            self.theme_dirty = true;
                        }
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.background_load = None,
            }
        }
        let wanted = self
            .theme
            .background_image
            .clone()
            .map(|path| (path, self.theme.image_blur.round() as u32));
        if wanted == self.background_key {
            return;
        }
        let Some(key) = wanted else {
            self.background_texture = None;
            self.background_key = None;
            return;
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        let (path, blur) = key.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = sender.send(theme::load_background_image(&path, blur as f32));
            ctx.request_repaint();
        });
        self.background_load = Some((key, receiver));
    }

    /// Turns the native acrylic blur behind the window on or off to match the theme.
    #[cfg(windows)]
    fn apply_window_blur(&mut self, frame: &eframe::Frame) {
        if !self.transparent_window {
            return;
        }
        let blur = self.theme.window_blur;
        if self.blur_applied == Some(blur) {
            return;
        }
        self.blur_applied = Some(blur);
        let result = if blur {
            window_vibrancy::apply_acrylic(frame, None).or_else(|_| window_vibrancy::apply_blur(frame, None))
        } else {
            let _ = window_vibrancy::clear_blur(frame);
            window_vibrancy::clear_acrylic(frame)
        };
        if let Err(error) = result {
            log::warn!("Could not change the window blur: {error}");
        }
    }

    #[cfg(not(windows))]
    fn apply_window_blur(&mut self, _frame: &eframe::Frame) {}

    fn open_theme_file_dialog(&mut self, ctx: &egui::Context, kind: ThemeFile) {
        if self.theme_dialog.is_some() {
            return;
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let dialog = match kind {
                ThemeFile::BackgroundImage => rfd::FileDialog::new()
                    .set_title("Choose a background image")
                    .add_filter("Images", &["png", "jpg", "jpeg", "webp", "bmp"]),
                ThemeFile::Font => rfd::FileDialog::new()
                    .set_title("Choose a font")
                    .add_filter("Fonts", &["ttf", "otf", "ttc"]),
            };
            let _ = sender.send(dialog.pick_file());
            ctx.request_repaint();
        });
        self.theme_dialog = Some((kind, receiver));
    }

    fn poll_theme_dialog(&mut self) {
        let Some((kind, receiver)) = &self.theme_dialog else {
            return;
        };
        let picked = match receiver.try_recv() {
            Ok(picked) => picked,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
        };
        let kind = *kind;
        self.theme_dialog = None;
        let Some(path) = picked else {
            return;
        };
        match kind {
            ThemeFile::BackgroundImage => {
                self.theme.background_image = Some(path);
                self.theme_message = None;
            }
            ThemeFile::Font => match theme::load_font(&path) {
                Ok(_) => {
                    self.theme.font = Some(path);
                    self.theme_message = None;
                }
                Err(error) => self.theme_message = Some(error),
            },
        }
        self.theme_dirty = true;
    }

    /// Starts a fresh copy of Oynx and closes this one, for settings that can
    /// only take effect when the window is created.
    fn restart_app(&mut self, ctx: &egui::Context) {
        self.save_theme();
        self.save_session();
        match std::env::current_exe().and_then(|exe| std::process::Command::new(exe).spawn()) {
            Ok(_) => {
                self.quitting = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Err(error) => self.theme_message = Some(format!("Could not restart Oynx: {error}")),
        }
    }

    fn draw_theme_settings(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let before = self.theme.clone();
        let dark = self.theme.is_dark(ctx.system_theme());

        Self::settings_card(ui, "Mode", |ui| {
            ui.add_space(4.0);
            ui.spacing_mut().item_spacing.y = 4.0;
            for (mode, title, detail) in [
                (ThemeMode::Dark, "Dark", "Light text on a dark background."),
                (ThemeMode::Light, "Light", "Dark text on a light background."),
                (ThemeMode::System, "Match Windows", "Follows the app mode in Windows settings."),
            ] {
                if option_row(ui, title, detail, self.theme.mode == mode).clicked() {
                    self.theme.mode = mode;
                }
            }
        });

        ui.add_space(16.0);
        let palette = self.theme.palette(dark);
        let card_title = if dark { "Colours for dark mode" } else { "Colours for light mode" };
        Self::settings_card(ui, card_title, |ui| {
            ui.label(
                egui::RichText::new("Dark and light mode each keep their own colours. Start from a preset or pick any colour yourself.")
                    .size(13.0)
                    .color(pal().muted),
            );
            ui.add_space(14.0);
            let presets = if dark { theme::DARK_PRESETS } else { theme::LIGHT_PRESETS };
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                let colors = self.theme.colors_mut(dark);
                if chip(ui, "Oynx", colors.is_empty()).clicked() {
                    *colors = theme::ColorOverrides::default();
                }
                for preset in presets {
                    if chip(ui, preset.name, *colors == preset.colors).clicked() {
                        *colors = preset.colors;
                    }
                }
            });
            ui.add_space(14.0);
            ui.spacing_mut().item_spacing.y = 2.0;
            for role in ColorRole::ALL {
                let current = match role {
                    ColorRole::Text => palette.text,
                    ColorRole::SecondaryText => palette.muted,
                    ColorRole::Background => palette.background,
                    ColorRole::Panels => palette.surface_raised,
                    ColorRole::Accent => palette.accent,
                    ColorRole::Visualizer => palette.visualizer,
                };
                let colors = self.theme.colors_mut(dark);
                let custom = colors.get(role).is_some();
                theme_row(ui, role.label(), role.detail(), |ui| {
                    let mut rgb = [current.r(), current.g(), current.b()];
                    ui.spacing_mut().interact_size = egui::vec2(44.0, 26.0);
                    let swatch = egui::color_picker::color_edit_button_srgb(ui, &mut rgb);
                    if swatch.changed() {
                        colors.set(role, Some(rgb));
                    }
                    // Repaint the swatch rounded and outlined so dark colours
                    // stay visible against the card.
                    let painter = ui.painter();
                    painter.rect_filled(swatch.rect, RADIUS_SM, current);
                    painter.rect_stroke(
                        swatch.rect,
                        RADIUS_SM,
                        egui::Stroke::new(1.0, mix(pal().subtle, pal().text, hover_t(ui, &swatch))),
                        egui::StrokeKind::Inside,
                    );
                    if custom
                        && text_link(ui, "Reset")
                            .on_hover_text("Use the preset colour")
                            .clicked()
                    {
                        colors.set(role, None);
                    }
                });
            }
        });

        ui.add_space(16.0);
        let picking = self.theme_dialog.is_some();
        Self::settings_card(ui, "Background image", |ui| {
            let detail = self
                .theme
                .background_image
                .as_deref()
                .map_or_else(|| "None chosen".to_owned(), theme::file_name);
            let mut choose = false;
            let mut remove = false;
            theme_row(ui, "Image", &detail, |ui| {
                if self.theme.background_image.is_some() {
                    remove = pill_button(ui, "Remove", ButtonKind::Ghost).clicked();
                }
                choose = pill_button(ui, "Choose image…", ButtonKind::Secondary).clicked();
            });
            if choose && !picking {
                self.open_theme_file_dialog(&ctx, ThemeFile::BackgroundImage);
            }
            if remove {
                self.theme.background_image = None;
            }
            if self.theme.background_image.is_some() {
                ui.add_space(10.0);
                theme_slider(ui, "Visibility", &mut self.theme.image_strength, 0.05..=1.0, |value| {
                    format!("{:.0}%", value * 100.0)
                });
                ui.add_space(8.0);
                theme_slider(ui, "Blur", &mut self.theme.image_blur, 0.0..=theme::MAX_IMAGE_BLUR, |value| {
                    if value < 0.5 { "Off".to_owned() } else { format!("{value:.0} px") }
                });
                if self.background_load.is_some() {
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new("Loading image…").size(12.0).color(pal().subtle));
                }
            }
        });

        ui.add_space(16.0);
        Self::settings_card(ui, "Font", |ui| {
            ui.label(
                egui::RichText::new("Used for all text in Oynx. Pick a font that comes with Windows or any TrueType or OpenType file.")
                    .size(13.0)
                    .color(pal().muted),
            );
            ui.add_space(14.0);
            let mut choose = false;
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                if chip(ui, "Inter", self.theme.font.is_none()).clicked() {
                    self.theme.font = None;
                }
                let mut matched = self.theme.font.is_none();
                for (name, path) in &self.system_fonts {
                    let selected = self.theme.font.as_ref() == Some(path);
                    matched |= selected;
                    if chip(ui, name, selected).clicked() {
                        self.theme.font = Some(path.clone());
                    }
                }
                if !matched && let Some(path) = &self.theme.font {
                    chip(ui, &theme::file_name(path), true);
                }
                choose = chip(ui, "Choose a font file…", false).clicked();
            });
            if choose && !picking {
                self.open_theme_file_dialog(&ctx, ThemeFile::Font);
            }
        });

        ui.add_space(16.0);
        let mut restart = false;
        Self::settings_card(ui, "Window", |ui| {
            ui.add_space(4.0);
            theme_slider(
                ui,
                "Opacity",
                &mut self.theme.window_opacity,
                theme::MIN_WINDOW_OPACITY..=1.0,
                |value| format!("{:.0}%", value * 100.0),
            );
            ui.add_space(12.0);
            let mut blur = self.theme.window_blur;
            theme_row(
                ui,
                "Blur behind the window",
                "Frosts whatever is behind Oynx when the window is see-through.",
                |ui| {
                    if toggle_switch(ui, blur).clicked() {
                        blur = !blur;
                    }
                },
            );
            self.theme.window_blur = blur;
            if !cfg!(windows) && blur {
                ui.label(egui::RichText::new("Window blur is only available on Windows.").size(12.0).color(pal().subtle));
            }
            if self.theme.wants_transparent_window() && self.transparency_failed {
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new(
                        "This PC's graphics driver couldn't open a see-through window, so Oynx is using a normal one.",
                    )
                    .size(12.0)
                    .color(pal().warning),
                );
            } else if self.theme.wants_transparent_window() && !self.transparent_window {
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Restart Oynx to make the window see-through.")
                            .size(12.0)
                            .color(pal().warning),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        restart = pill_button(ui, "Restart now", ButtonKind::Primary).clicked();
                    });
                });
            }
        });
        if restart {
            self.restart_app(&ctx);
        }

        ui.add_space(16.0);
        ui.horizontal(|ui| {
            if pill_button(ui, "Restore the default theme", ButtonKind::Secondary).clicked() {
                self.theme = ThemeSettings::default();
                self.theme_message = None;
            }
        });
        if let Some(message) = &self.theme_message {
            ui.add_space(10.0);
            ui.label(egui::RichText::new(message).size(12.0).color(pal().danger));
        }

        if self.theme != before {
            self.theme_dirty = true;
        }
    }

    fn draw_general_settings(&mut self, ui: &mut egui::Ui) {
        let connected = self.connection_state == ConnectionState::Ready;
        let mut account_action = false;
        Self::settings_card(ui, "Account", |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let (avatar, _) = ui.allocate_exact_size(egui::vec2(48.0, 48.0), egui::Sense::hover());
                self.paint_avatar(ui, avatar.center(), 24.0);
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(self.account_label())
                            .font(font_medium(15.0))
                            .color(pal().text),
                    );
                    ui.label(
                        egui::RichText::new(match self.connection_state {
                            ConnectionState::Ready => "Connected to Spotify",
                            ConnectionState::Authenticating | ConnectionState::Connecting => {
                                "Connecting…"
                            }
                            ConnectionState::Disconnected => "Not connected",
                        })
                        .size(12.0)
                        .color(pal().muted),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = if connected { "Sign out" } else { "Sign in with Spotify" };
                    let kind = if connected { ButtonKind::Secondary } else { ButtonKind::Primary };
                    account_action = self.connection_state != ConnectionState::Authenticating
                        && self.connection_state != ConnectionState::Connecting
                        && pill_button(ui, label, kind).clicked();
                });
            });
        });
        if account_action {
            if connected {
                self.spotify.logout();
            } else {
                self.connection_error = None;
                self.spotify.login_with_config(
                    Some(self.spotify_client_id.clone()),
                    Some(self.spotify_redirect_uri.clone()),
                    Some(self.spotify_web_api_client_id.clone()),
                    Some(self.spotify_web_api_redirect_uri.clone()),
                );
            }
        }
        ui.add_space(16.0);

        let mut save = false;
        let mut reset = false;
        Self::settings_card(ui, "Spotify connection", |ui| {
            ui.label(
                egui::RichText::new(
                    "Oynx uses PKCE and never needs a client secret. Configure a separate Web API app to avoid Spotify's shared rate-limit bucket; Oynx will open one browser authorization for playback and another for the Web API.",
                )
                .size(13.0)
                .color(pal().muted),
            );
            ui.add_space(20.0);
            Self::settings_field(
                ui,
                "Streaming client ID",
                &mut self.spotify_client_id,
                "Spotify application client ID",
            );
            Self::settings_field(
                ui,
                "Streaming redirect URI",
                &mut self.spotify_redirect_uri,
                "http://127.0.0.1:8898/login",
            );
            Self::settings_field(
                ui,
                "Web API client ID (recommended)",
                &mut self.spotify_web_api_client_id,
                "Separate Spotify app client ID (optional)",
            );
            ui.label(
                egui::RichText::new(
                    "Spotify rate limits are per app, so a separate Web API app avoids the shared Librespot limit.",
                )
                .size(12.0)
                .color(pal().subtle),
            );
            if self.spotify_web_api_client_id.trim().is_empty() {
                ui.label(
                    egui::RichText::new(
                        "No separate Web API app is configured; playlist requests share the streaming app's rate limit.",
                    )
                    .size(12.0)
                    .color(pal().warning),
                );
            }
            ui.add_space(14.0);
            Self::settings_field(
                ui,
                "Web API redirect URI",
                &mut self.spotify_web_api_redirect_uri,
                "Separate app default: http://127.0.0.1:8989/login",
            );
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                save = pill_button(ui, "Save changes", ButtonKind::Primary).clicked();
                reset = pill_button(ui, "Reset defaults", ButtonKind::Secondary).clicked();
            });
            if let Some(message) = &self.settings_message {
                ui.add_space(10.0);
                ui.label(egui::RichText::new(message).size(12.0).color(pal().accent));
            }
        });

        if save {
            let client_id = self.spotify_client_id.trim().to_owned();
            let redirect_uri = self.spotify_redirect_uri.trim().to_owned();
            let web_api_client_id = self.spotify_web_api_client_id.trim().to_owned();
            let web_api_redirect_uri = self.spotify_web_api_redirect_uri.trim().to_owned();
            if client_id.is_empty() || redirect_uri.is_empty() {
                self.settings_message =
                    Some("Streaming client ID and redirect URI cannot be empty.".into());
            } else {
                let config = SpotifyConfig {
                    client_id,
                    redirect_uri,
                    web_api_client_id,
                    web_api_redirect_uri,
                };
                match config.save() {
                    Ok(()) => {
                        self.spotify.logout();
                        self.connection_error = None;
                        self.settings_message =
                            Some("Settings saved. Sign in again to apply them.".into());
                    }
                    Err(error) => self.settings_message = Some(error),
                }
            }
        }
        if reset {
            let config = SpotifyConfig::default();
            self.spotify_client_id = config.client_id.clone();
            self.spotify_redirect_uri = config.redirect_uri.clone();
            self.spotify_web_api_client_id = config.web_api_client_id.clone();
            self.spotify_web_api_redirect_uri = config.web_api_redirect_uri.clone();
            self.spotify.logout();
            self.connection_error = None;
            self.settings_message = match config.save() {
                Ok(()) => Some("Defaults restored. Sign in again to apply them.".into()),
                Err(error) => Some(error),
            };
        }

        ui.add_space(16.0);
        Self::settings_card(ui, "Lyrics", |ui| {
            ui.add_space(4.0);
            ui.spacing_mut().item_spacing.y = 4.0;
            for source in LyricsSource::ALL {
                if option_row(ui, source.label(), source.detail(), self.prefs.lyrics_source == source).clicked() {
                    self.set_lyrics_source(source);
                }
            }
        });

        ui.add_space(16.0);
        let tray_available = self.tray.is_some();
        Self::settings_card(ui, "When closing", |ui| {
            ui.add_space(4.0);
            ui.spacing_mut().item_spacing.y = 4.0;
            for (close_to_tray, title, detail) in [
                (
                    true,
                    "Keep running in the system tray",
                    "Closing the window hides Oynx in the tray so music keeps playing. Quit from the tray icon.",
                ),
                (
                    false,
                    "Quit Oynx",
                    "Closing the window stops playback and exits.",
                ),
            ] {
                if option_row(ui, title, detail, self.prefs.close_to_tray == close_to_tray).clicked() {
                    self.prefs.close_to_tray = close_to_tray;
                }
            }
            if !tray_available {
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new("The system tray isn't available, so closing the window quits Oynx.")
                        .size(12.0)
                        .color(pal().subtle),
                );
            }
        });

        ui.add_space(16.0);
        Self::settings_card(ui, "Local data", |ui| {
            ui.label(
                egui::RichText::new(
                    "Credentials and audio cache are managed by Librespot in the local Oynx data directory.",
                )
                .size(13.0)
                .color(pal().muted),
            );
            ui.add_space(14.0);
            for (label, path) in [
                ("Config", SpotifyConfig::path()),
                ("Cache", SpotifyConfig::cache_path()),
            ] {
                ui.label(egui::RichText::new(label).font(font_medium(12.0)).color(pal().muted));
                ui.label(
                    egui::RichText::new(path.display().to_string())
                        .monospace()
                        .size(12.0)
                        .color(pal().text),
                );
                ui.add_space(10.0);
            }
            ui.label(
                egui::RichText::new(
                    "OAuth scopes: streaming, playlists, recommendations, and library read/write",
                )
                .size(12.0)
                .color(pal().subtle),
            );
        });
    }

    // ----------------------------------------------------------------------
    // Right panel
    // ----------------------------------------------------------------------

    // ----------------------------------------------------------------------
    // Player bar
    // ----------------------------------------------------------------------

    /// The now-playing record: fine grooves, a fixed sheen, and the album art as
    /// the centre label. The disc turns by `angle` (radians).
    fn paint_vinyl(
        &mut self,
        ui: &egui::Ui,
        center: egui::Pos2,
        radius: f32,
        image_url: Option<&str>,
        angle: f32,
    ) {
        let label_radius = radius * 0.4;
        let texture_id = image_url.and_then(|url| {
            self.request_artwork(url);
            self.artwork_textures.get(url).map(|texture| texture.id())
        });
        let painter = ui.painter();
        painter.circle_filled(center + egui::vec2(0.0, 3.0), radius + 2.0, egui::Color32::from_black_alpha(140));
        painter.circle_filled(center, radius, VINYL);
        // Fine grooves; every few rings is slightly brighter, like a real record.
        let mut groove_radius = label_radius + 6.0;
        let mut ring = 0;
        while groove_radius < radius - 4.0 {
            let alpha = if ring % 5 == 0 { 20 } else { 9 };
            painter.circle_stroke(
                center,
                groove_radius,
                egui::Stroke::new(0.7, egui::Color32::from_white_alpha(alpha)),
            );
            groove_radius += 3.2;
            ring += 1;
        }
        // Fixed reflections: they stay put while the grooves and label turn.
        for (from, to) in [(-72.0_f32, -28.0_f32), (108.0, 152.0)] {
            let points = (0..=16)
                .map(|step| {
                    let a = (from + (to - from) * step as f32 / 16.0).to_radians();
                    center + egui::vec2(a.cos(), a.sin()) * radius * 0.74
                })
                .collect();
            painter.add(egui::Shape::line(
                points,
                egui::Stroke::new(radius * 0.36, egui::Color32::from_white_alpha(5)),
            ));
        }
        painter.circle_stroke(
            center,
            radius - 0.5,
            egui::Stroke::new(1.0, egui::Color32::from_white_alpha(28)),
        );

        match texture_id {
            Some(texture_id) => paint_textured_circle(painter, texture_id, center, label_radius, angle),
            None => {
                painter.circle_filled(center, label_radius, egui::Color32::from_rgb(14, 14, 15));
                paint_icon(
                    painter,
                    Icon::Sparkle,
                    center,
                    label_radius * 0.9,
                    egui::Color32::from_rgb(210, 210, 210),
                );
            }
        }
        painter.circle_stroke(
            center,
            label_radius,
            egui::Stroke::new(1.0, egui::Color32::from_white_alpha(46)),
        );
        if texture_id.is_some() {
            painter.circle_filled(center, radius * 0.035, pal().background);
        }
    }

    // ----------------------------------------------------------------------
    // Window chrome: custom title bar and resize edges (the window is undecorated)
    // ----------------------------------------------------------------------

    fn draw_title_bar(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        let ctx = ui.ctx().clone();
        let maximized = ctx.input(|input| input.viewport().maximized.unwrap_or(false));
        let drag = ui.interact(rect, ui.id().with("title_drag"), egui::Sense::click_and_drag());
        if drag.drag_started_by(egui::PointerButton::Primary) {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        if drag.double_clicked() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
        }
        paint_text_spaced(
            ui.painter(),
            egui::pos2(rect.left() + 40.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            "oynx",
            font_light(21.0),
            pal().text,
            7.0,
        );

        if let UpdateStatus::Ready(release) = self.updater.status() {
            let pill = egui::Rect::from_min_size(
                egui::pos2(rect.right() - 36.0 - 2.0 * 54.0 - 30.0 - 170.0, rect.center().y - 15.0),
                egui::vec2(170.0, 30.0),
            );
            let mut pill_ui = ui.new_child(egui::UiBuilder::new().max_rect(pill));
            if pill_button(&mut pill_ui, "Restart to update", ButtonKind::Secondary)
                .on_hover_text(format!("Oynx {} is ready to install", release.version))
                .clicked()
            {
                self.restart_to_update(&ctx);
            }
        }

        let size = egui::vec2(46.0, 34.0);
        let buttons = [
            (Icon::Close, "Close", 0),
            (
                if maximized { Icon::Restore } else { Icon::Maximize },
                if maximized { "Restore" } else { "Maximize" },
                1,
            ),
            (Icon::Minimize, "Minimize", 2),
        ];
        for (icon, label, slot) in buttons {
            let center = egui::pos2(rect.right() - 36.0 - slot as f32 * 54.0, rect.center().y);
            let button = egui::Rect::from_center_size(center, size);
            let response = ui
                .interact(button, ui.id().with(("window_button", slot)), egui::Sense::click())
                .on_hover_text(label);
            let hover = hover_t(ui, &response);
            let hover_fill = if icon == Icon::Close {
                egui::Color32::from_rgb(170, 44, 38)
            } else {
                pal().surface_hover
            };
            if hover > 0.0 {
                ui.painter()
                    .rect_filled(button, RADIUS_SM, hover_fill.gamma_multiply(hover));
            }
            paint_icon(ui.painter(), icon, center, 15.0, mix(pal().muted, pal().text, hover));
            if response.clicked() {
                ctx.send_viewport_cmd(match icon {
                    Icon::Close => egui::ViewportCommand::Close,
                    Icon::Minimize => egui::ViewportCommand::Minimized(true),
                    _ => egui::ViewportCommand::Maximized(!maximized),
                });
            }
        }
    }

    /// Invisible grab zones along the window edges, since the OS frame is hidden.
    fn handle_resize_edges(ui: &egui::Ui) {
        let ctx = ui.ctx();
        let (maximized, fullscreen) = ctx.input(|input| {
            (
                input.viewport().maximized.unwrap_or(false),
                input.viewport().fullscreen.unwrap_or(false),
            )
        });
        if maximized || fullscreen {
            return;
        }
        use egui::CursorIcon as Cursor;
        use egui::viewport::ResizeDirection as Direction;
        let r = ctx.viewport_rect();
        let edge = 5.0;
        let corner = 12.0;
        let zones = [
            (
                egui::Rect::from_min_size(r.left_top(), egui::vec2(corner, corner)),
                Direction::NorthWest,
                Cursor::ResizeNorthWest,
            ),
            (
                egui::Rect::from_min_size(r.right_top() - egui::vec2(corner, 0.0), egui::vec2(corner, corner)),
                Direction::NorthEast,
                Cursor::ResizeNorthEast,
            ),
            (
                egui::Rect::from_min_size(r.left_bottom() - egui::vec2(0.0, corner), egui::vec2(corner, corner)),
                Direction::SouthWest,
                Cursor::ResizeSouthWest,
            ),
            (
                egui::Rect::from_min_max(r.right_bottom() - egui::vec2(corner, corner), r.right_bottom()),
                Direction::SouthEast,
                Cursor::ResizeSouthEast,
            ),
            (
                egui::Rect::from_min_max(
                    egui::pos2(r.left() + corner, r.top()),
                    egui::pos2(r.right() - corner, r.top() + edge),
                ),
                Direction::North,
                Cursor::ResizeNorth,
            ),
            (
                egui::Rect::from_min_max(
                    egui::pos2(r.left() + corner, r.bottom() - edge),
                    egui::pos2(r.right() - corner, r.bottom()),
                ),
                Direction::South,
                Cursor::ResizeSouth,
            ),
            (
                egui::Rect::from_min_max(
                    egui::pos2(r.left(), r.top() + corner),
                    egui::pos2(r.left() + edge, r.bottom() - corner),
                ),
                Direction::West,
                Cursor::ResizeWest,
            ),
            (
                egui::Rect::from_min_max(
                    egui::pos2(r.right() - edge, r.top() + corner),
                    egui::pos2(r.right(), r.bottom() - corner),
                ),
                Direction::East,
                Cursor::ResizeEast,
            ),
        ];
        for (index, (rect, direction, cursor)) in zones.into_iter().enumerate() {
            let response = ui
                .interact(rect, egui::Id::new(("resize_edge", index)), egui::Sense::drag())
                .on_hover_cursor(cursor);
            if response.drag_started() {
                ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(direction));
            }
        }
    }

    // ----------------------------------------------------------------------
    // Sidebar
    // ----------------------------------------------------------------------

    fn draw_nav_item(&mut self, ui: &mut egui::Ui, item: Section) {
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), 44.0),
            egui::Sense::click(),
        );
        let selected = self.section == item;
        let hover = hover_t(ui, &response);
        let select = animate(ui, response.id.with("selected"), selected, ANIM_MEDIUM);
        let fill = select.max(hover * 0.5);
        if fill > 0.0 {
            ui.painter()
                .rect_filled(rect, RADIUS_MD, pal().surface_raised.gamma_multiply(fill));
        }
        let icon = match item {
            Section::Home => Icon::Home,
            Section::Search => Icon::Search,
            Section::Library => Icon::Note,
            Section::LikedSongs => Icon::Heart,
            Section::Queue => Icon::Queue,
            Section::Settings => Icon::Settings,
        };
        let color = mix(pal().text.gamma_multiply(0.72), pal().text, select.max(hover));
        paint_icon(
            ui.painter(),
            icon,
            rect.left_center() + egui::vec2(28.0, 0.0),
            19.0,
            color,
        );
        ui.painter().text(
            rect.left_center() + egui::vec2(64.0, 0.0),
            egui::Align2::LEFT_CENTER,
            item.label(),
            egui::FontId::proportional(14.5),
            color,
        );
        if response
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            self.open_section(item);
        }
    }

    fn draw_sidebar(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        let disc_radius = ((rect.width() - 36.0) * 0.5)
            .min(120.0)
            .min(((rect.height() - 330.0) * 0.5).max(56.0));
        let disc_center = egui::pos2(
            rect.left() + 18.0 + disc_radius,
            rect.bottom() - disc_radius - 14.0,
        );
        let divider_bottom = disc_center.y - disc_radius - 24.0;
        ui.painter().vline(
            rect.right() - 0.5,
            rect.top()..=divider_bottom,
            egui::Stroke::new(1.0, pal().border),
        );

        let mut nav = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(egui::Rect::from_min_max(
                    rect.min + egui::vec2(18.0, 20.0),
                    egui::pos2(rect.right() - 18.0, divider_bottom),
                ))
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        nav.spacing_mut().item_spacing.y = 8.0;
        for item in [
            Section::Home,
            Section::Search,
            Section::Library,
            Section::LikedSongs,
            Section::Settings,
        ] {
            self.draw_nav_item(&mut nav, item);
        }

        // The now-playing record; clicking it opens the Now Playing view.
        let track = self.current_track().clone();
        self.paint_vinyl(
            ui,
            disc_center,
            disc_radius,
            track.image_url.as_deref(),
            self.disc_angle,
        );
        let disc_rect = egui::Rect::from_center_size(disc_center, egui::vec2(disc_radius, disc_radius) * 2.0);
        if ui
            .interact(disc_rect, ui.id().with("disc"), egui::Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text("Now playing")
            .clicked()
        {
            self.open_section(Section::Home);
        }
    }

    /// Advances the record by real elapsed time, so it spins at the same speed at any frame rate.
    fn advance_disc(&mut self) {
        let now = Instant::now();
        if self.playing
            && let Some(last) = self.disc_last_frame
        {
            let elapsed = now.duration_since(last).as_secs_f32().min(0.25);
            self.disc_angle = (self.disc_angle + elapsed * DISC_SPEED) % std::f32::consts::TAU;
        }
        self.disc_last_frame = Some(now);
    }

    // ----------------------------------------------------------------------
    // Now Playing (home)
    // ----------------------------------------------------------------------

    fn draw_now_playing(&mut self, ui: &mut egui::Ui) {
        let rect = ui.available_rect_before_wrap();
        ui.allocate_rect(rect, egui::Sense::hover());
        paint_text_spaced(
            ui.painter(),
            rect.left_top() + egui::vec2(0.0, 6.0),
            egui::Align2::LEFT_TOP,
            "NOW PLAYING",
            egui::FontId::proportional(10.5),
            pal().subtle,
            2.6,
        );
        let tabs = egui::Rect::from_min_size(
            rect.left_top() + egui::vec2(132.0, -2.0),
            egui::vec2((rect.width() - 132.0).max(160.0), 30.0),
        );
        let mut tabs_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(tabs)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        tabs_ui.spacing_mut().item_spacing.x = 6.0;
        for (tab, label) in [(NowPlayingTab::Lyrics, "Lyrics"), (NowPlayingTab::Credits, "Credits")] {
            if chip(&mut tabs_ui, label, self.now_playing_tab == tab).clicked() {
                self.now_playing_tab = tab;
            }
        }
        if self.now_playing_tab == NowPlayingTab::Lyrics {
            // Where the lyrics came from, and a menu to pick another source.
            tabs_ui.add_space(6.0);
            let label = match (&self.lyrics_provider, self.prefs.lyrics_source) {
                (Some(provider), _) => format!("From {provider}"),
                (None, LyricsSource::Auto) => "Choose lyrics source".to_owned(),
                (None, source) => format!("From {}", source.label()),
            };
            let source_button = text_link(&mut tabs_ui, &label).on_hover_text("Choose where lyrics come from");
            egui::Popup::from_toggle_button_response(&source_button)
                .kind(egui::PopupKind::Menu)
                .align(egui::RectAlign::BOTTOM_START)
                .gap(4.0)
                .width(230.0)
                .close_behavior(egui::PopupCloseBehavior::CloseOnClick)
                .frame(menu_frame())
                .show(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    for source in LyricsSource::ALL {
                        if menu_item(ui, Icon::Note, source.label(), self.prefs.lyrics_source == source)
                            .on_hover_text(source.detail())
                            .clicked()
                        {
                            self.set_lyrics_source(source);
                        }
                    }
                });
        }
        let show_visualizer = rect.width() >= 520.0;
        let visualizer_x = rect.left() + (rect.width() * 0.76).min(rect.width() - 110.0);
        let lyrics_rect = egui::Rect::from_min_max(
            egui::pos2(rect.left(), rect.top() + 58.0),
            egui::pos2(
                if show_visualizer {
                    visualizer_x - 48.0
                } else {
                    rect.right()
                },
                rect.bottom(),
            ),
        );
        match self.now_playing_tab {
            NowPlayingTab::Lyrics => self.draw_lyrics(ui, lyrics_rect),
            NowPlayingTab::Credits => self.draw_credits(ui, lyrics_rect),
        }
        if show_visualizer {
            self.paint_visualizer(
                ui,
                egui::Rect::from_min_max(
                    egui::pos2(visualizer_x, rect.top() + 30.0),
                    egui::pos2(rect.right(), rect.bottom() - 6.0),
                ),
            );
        }
    }

    /// Who made the current song and how it connects to others: Oynx's take on
    /// Spotify's SongDNA, from Spotify's metadata and MusicBrainz.
    fn draw_credits(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        self.request_credits_for_current();
        let track = self.current_track().clone();
        let track_id = self.current_track_id();
        let mut panel = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        let ui = &mut panel;
        let mut search = None;
        let mut retry = false;
        egui::ScrollArea::vertical()
            .id_salt("credits")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_width(ui.available_width() - 12.0);
                ui.add(egui::Label::new(
                    egui::RichText::new(&track.title).font(font_light(30.0)).color(pal().text),
                ));
                ui.add_space(4.0);
                ui.add(egui::Label::new(
                    egui::RichText::new(&track.artist).font(font_light(21.0)).color(pal().muted),
                ));
                ui.add_space(26.0);
                let state = track_id.as_ref().and_then(|id| self.credits.get(id));
                match state {
                    None => Self::muted_note(
                        ui,
                        if self.connection_state == ConnectionState::Ready {
                            "Play a song to see who made it."
                        } else {
                            "Sign in to Spotify to see song credits."
                        },
                    ),
                    Some(CreditsState::Loading) => Self::muted_note(ui, "Finding who made this song…"),
                    Some(CreditsState::Failed(error)) => {
                        Self::muted_note(ui, error);
                        ui.add_space(6.0);
                        retry = pill_button(ui, "Try again", ButtonKind::Ghost).clicked();
                    }
                    Some(CreditsState::Ready(credits)) => {
                        let credits = credits.clone();
                        if credits.is_empty() {
                            Self::muted_note(ui, "MusicBrainz has no credits for this song yet.");
                        }
                        for group in &credits.credits {
                            credits_heading(ui, &group.role);
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = egui::vec2(18.0, 6.0);
                                for name in &group.names {
                                    if credit_link(ui, name, 17.0, pal().text)
                                        .on_hover_text("Search Spotify")
                                        .clicked()
                                    {
                                        // Instrument and vocal credits read "Name (guitar)".
                                        let name = name.split(" (").next().unwrap_or(name);
                                        search = Some(name.to_owned());
                                    }
                                }
                            });
                            ui.add_space(18.0);
                        }
                        for (heading, links) in [
                            ("Samples", &credits.samples),
                            ("Sampled in", &credits.sampled_by),
                            ("Other versions", &credits.versions),
                        ] {
                            if links.is_empty() {
                                continue;
                            }
                            credits_heading(ui, heading);
                            for link in links {
                                if song_link_row(ui, link).clicked() {
                                    search = Some(format!("{} {}", link.title, link.artist));
                                }
                            }
                            ui.add_space(18.0);
                        }
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new(
                                "Credits come from Spotify and MusicBrainz, the open music encyclopedia anyone can edit.",
                            )
                            .size(12.0)
                            .color(pal().subtle),
                        );
                        if let Some(url) = &credits.source_url
                            && text_link(ui, "View on MusicBrainz").clicked()
                        {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                        }
                        ui.add_space(24.0);
                    }
                }
            });
        if retry && let Some(track_id) = track_id {
            self.credits.remove(&track_id);
        }
        if let Some(query) = search {
            self.search_for(&query);
        }
    }

    fn draw_lyrics(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        let painter = ui.painter_at(rect);
        let track = self.current_track().clone();
        let message = if self.lyrics_loading {
            Some("Loading lyrics…".to_owned())
        } else if let Some(error) = &self.lyrics_error {
            Some(error.clone())
        } else if self.lyrics.is_empty() {
            Some("No lyrics for this track".to_owned())
        } else {
            None
        };
        if let Some(message) = message {
            // No lyrics: show the track itself in the lyrics' typography.
            let title = paint_text(
                &painter,
                rect.left_top(),
                egui::Align2::LEFT_TOP,
                &track.title,
                font_light(30.0),
                pal().text,
                rect.width(),
            );
            let artist = paint_text(
                &painter,
                egui::pos2(rect.left(), title.bottom() + 8.0),
                egui::Align2::LEFT_TOP,
                &track.artist,
                font_light(21.0),
                pal().muted,
                rect.width(),
            );
            painter.text(
                egui::pos2(rect.left(), artist.bottom() + 28.0),
                egui::Align2::LEFT_TOP,
                message,
                egui::FontId::proportional(13.0),
                pal().subtle,
            );
            if self.lyrics_error.is_some() {
                let retry = egui::Rect::from_min_size(
                    egui::pos2(rect.left() - 12.0, artist.bottom() + 54.0),
                    egui::vec2(140.0, 36.0),
                );
                let mut retry_ui = ui.new_child(egui::UiBuilder::new().max_rect(retry));
                if pill_button(&mut retry_ui, "Retry lyrics", ButtonKind::Ghost).clicked() {
                    self.lyrics_track_id = None;
                    self.request_lyrics_for_current();
                }
            }
            return;
        }

        if !self.lyrics_synced {
            let mut lyrics_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(rect)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            egui::ScrollArea::vertical()
                .id_salt(("plain_lyrics", self.lyrics_track_id.as_deref()))
                .auto_shrink([false, false])
                .show(&mut lyrics_ui, |ui| {
                    for line in &self.lyrics {
                        let text = if line.text.is_empty() { " " } else { line.text.as_str() };
                        ui.label(egui::RichText::new(text).font(font_light(21.0)).color(pal().muted));
                        ui.add_space(10.0);
                    }
                });
            return;
        }

        // Synced lyrics: every line stays in a scrollable list. The active line
        // is highlighted and the view glides to it as the song moves on. When
        // the listener scrolls, following pauses so past lines can be read.
        let current = self
            .lyrics
            .iter()
            .rposition(|line| line.timestamp_ms <= self.position_ms)
            .unwrap_or(0);
        let now = Instant::now();
        if self.lyrics_manual_until.is_some_and(|until| now >= until) {
            // The pause is over: glide back to the current line.
            self.lyrics_manual_until = None;
            self.lyrics_follow = None;
        }
        let manual = self.lyrics_manual_until.is_some();
        let follow_key = (self.lyrics_track_id.clone(), current);
        let follow = !manual && self.lyrics_follow.as_ref() != Some(&follow_key);
        if follow {
            self.lyrics_follow = Some(follow_key);
        }

        let mut lyrics_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        let lyrics_id = ui.id().with(("lyrics", self.lyrics_track_id.as_deref()));
        let output = egui::ScrollArea::vertical()
            .id_salt(("synced_lyrics", self.lyrics_track_id.as_deref()))
            .auto_shrink([false, false])
            .show(&mut lyrics_ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let mut current_visible = false;
                let mut previous_top = None;
                let width = ui.available_width();
                for (index, line) in self.lyrics.iter().enumerate() {
                    let active = index == current;
                    let emphasis = animate(ui, lyrics_id.with(index), active, ANIM_SLOW);
                    let rest = if index < current {
                        pal().text.gamma_multiply(0.26)
                    } else {
                        pal().text.gamma_multiply(0.4)
                    };
                    let text = if line.text.is_empty() { "♪" } else { line.text.as_str() };
                    let color = mix(rest, pal().text, emphasis);
                    // Reserve the height the line needs at its largest size, so a
                    // line growing or shrinking never shifts the ones around it.
                    let reserved = ui
                        .painter()
                        .layout(text.to_owned(), font_light(28.0), color, width)
                        .size()
                        .y;
                    let (line_rect, _) = ui.allocate_exact_size(egui::vec2(width, reserved), egui::Sense::hover());
                    let galley = ui
                        .painter()
                        .layout(text.to_owned(), font_light(21.0 + 7.0 * emphasis), color, width);
                    ui.painter().galley(line_rect.min, galley, color);
                    if active {
                        current_visible = ui.clip_rect().intersects(line_rect);
                        if follow {
                            // Keep the previous line in view above the current one.
                            let mut target = line_rect;
                            target.min.y = previous_top.unwrap_or(line_rect.min.y) - 4.0;
                            ui.scroll_to_rect(target, Some(egui::Align::Min));
                        }
                    }
                    previous_top = Some(line_rect.min.y);
                    ui.add_space(14.0);
                }
                // Room below the last line so the final lines can still reach the top.
                ui.add_space(rect.height() * 0.6);
                current_visible
            });

        // Any scrolling by the listener pauses following for a few seconds.
        let scrolled = lyrics_ui.rect_contains_pointer(rect)
            && ui.input(|input| input.smooth_scroll_delta.y != 0.0);
        if scrolled {
            self.lyrics_manual_until = Some(now + LYRICS_MANUAL_SCROLL_PAUSE);
        }
        if self.lyrics_manual_until.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(250));
            if !output.inner {
                let button = egui::Rect::from_min_size(
                    egui::pos2(rect.left() - 12.0, rect.bottom() - 44.0),
                    egui::vec2(200.0, 36.0),
                );
                let mut button_ui = ui.new_child(egui::UiBuilder::new().max_rect(button));
                if pill_button(&mut button_ui, "Back to current line", ButtonKind::Secondary).clicked() {
                    self.lyrics_manual_until = None;
                    self.lyrics_follow = None;
                }
            }
        }
    }

    /// The vertical waveform: a live spectrum of what is playing, mirrored so
    /// the bass sits in the middle and the treble fans out towards both ends.
    fn paint_visualizer(&mut self, ui: &egui::Ui, rect: egui::Rect) {
        const SPACING: f32 = 11.5;
        let count = ((rect.height() / SPACING) as usize).max(3);
        let now = Instant::now();
        let dt = self
            .vis_last_frame
            .map_or(0.0, |last| now.duration_since(last).as_secs_f32())
            .min(0.1);
        self.vis_last_frame = Some(now);

        // Take the newest analysed frame whose audio is reaching the speakers now.
        let frame = match self.audio.bands.try_lock() {
            Ok(mut bands) => {
                self.vis_active = bands.is_active;
                bands.take_due(now)
            }
            Err(_) => None,
        };
        if self.vis_targets.len() != count {
            self.vis_targets = vec![0.0; count];
            self.vis_levels = vec![0.0; count];
        }
        if let Some(frame) = frame {
            self.vis_targets = spectrum_ticks(&frame.values, frame.peak_envelope, count);
        }
        if !self.vis_active {
            self.vis_targets.fill(0.0);
        }
        let ease = 1.0 - (-dt * 18.0).exp();
        for (level, target) in self.vis_levels.iter_mut().zip(&self.vis_targets) {
            *level += (target - *level) * ease;
        }
        if !self.vis_active && self.vis_levels.iter().any(|level| *level > 0.002) {
            // Let the ticks settle after playback stops.
            ui.ctx().request_repaint_after(DISC_FRAME_INTERVAL);
        }

        let max_length = rect.width().min(84.0);
        let painter = ui.painter();
        for (index, level) in self.vis_levels.iter().enumerate() {
            let length = 4.0 + (max_length - 4.0) * level;
            let y = rect.top() + index as f32 * SPACING;
            painter.hline(
                rect.left()..=rect.left() + length,
                y,
                egui::Stroke::new(1.2, pal().visualizer.gamma_multiply(0.18 + 0.72 * level)),
            );
        }
    }

    // ----------------------------------------------------------------------
    // Queue panel
    // ----------------------------------------------------------------------

    fn draw_queue_panel(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        ui.painter().vline(
            rect.left() + 0.5,
            rect.top()..=rect.bottom() - 16.0,
            egui::Stroke::new(1.0, pal().border),
        );
        let mut panel = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(egui::Rect::from_min_max(
                    rect.min + egui::vec2(22.0, 20.0),
                    rect.max - egui::vec2(14.0, 8.0),
                ))
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        let ui = &mut panel;

        let (header, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 36.0), egui::Sense::hover());
        ui.painter().text(
            header.left_center() + egui::vec2(6.0, 0.0),
            egui::Align2::LEFT_CENTER,
            match self.right_panel_tab {
                RightPanelTab::Queue => "Queue",
                RightPanelTab::Recent => "Recently played",
            },
            egui::FontId::proportional(17.0),
            pal().text,
        );
        let dots = egui::Rect::from_center_size(header.right_center() - egui::vec2(18.0, 0.0), egui::vec2(34.0, 34.0));
        let dots_response = icon_button_at(ui, ui.id().with("queue_menu"), dots, Icon::Dots, 18.0, false)
            .on_hover_text("Queue options");
        egui::Popup::from_toggle_button_response(&dots_response)
            .kind(egui::PopupKind::Menu)
            .align(egui::RectAlign::BOTTOM_END)
            .gap(6.0)
            .width(210.0)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClick)
            .frame(menu_frame())
            .show(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                if menu_item(ui, Icon::Queue, "Up next", self.right_panel_tab == RightPanelTab::Queue).clicked() {
                    self.right_panel_tab = RightPanelTab::Queue;
                }
                if menu_item(ui, Icon::Note, "Recently played", self.right_panel_tab == RightPanelTab::Recent).clicked() {
                    self.right_panel_tab = RightPanelTab::Recent;
                }
                if menu_item(ui, Icon::Close, "Hide queue", false).clicked() {
                    self.queue_panel_visible = false;
                }
            });
        ui.add_space(12.0);

        egui::ScrollArea::vertical()
            .id_salt("queue_panel")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                match self.right_panel_tab {
                    RightPanelTab::Queue => self.draw_up_next(ui),
                    RightPanelTab::Recent => self.draw_recently_played(ui),
                }
            });
    }

    /// Songs Spotify says this account played recently, newest first.
    fn draw_recently_played(&mut self, ui: &mut egui::Ui) {
        let ready = self.connection_state == ConnectionState::Ready;
        let stale = self
            .recent_loaded_at
            .is_none_or(|loaded| loaded.elapsed() >= Duration::from_secs(60));
        if ready && stale && !self.recent_loading {
            self.recent_loading = true;
            self.recent_loaded_at = Some(Instant::now());
            self.spotify.load_recently_played();
        }
        if !ready {
            Self::muted_note(ui, "Sign in to see what you played recently.");
            return;
        }
        if let Some(error) = self.recent_error.clone() {
            Self::muted_note(ui, &error);
            return;
        }
        if self.recent_tracks.is_empty() {
            Self::muted_note(
                ui,
                if self.recent_loading {
                    "Loading your recently played songs…"
                } else {
                    "Nothing played recently"
                },
            );
            return;
        }
        let current_id = self.current_track_id();
        for index in 0..self.recent_tracks.len() {
            let track = self.recent_tracks[index].clone();
            let track_id = self.recent_ids.get(index).cloned();
            let current = self.queue_active && track_id.is_some() && track_id == current_id;
            let response = self.draw_queue_row(ui, &track, current);
            if response.clicked() {
                self.play_list(
                    self.recent_tracks.clone(),
                    self.recent_ids.clone(),
                    index,
                    "Recently played",
                );
            }
            egui::Popup::context_menu(&response)
                .width(220.0)
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .frame(menu_frame())
                .show(|ui| self.track_menu(ui, &track, track_id.as_deref()));
        }
    }

    fn draw_up_next(&mut self, ui: &mut egui::Ui) {
        let (queue_len, current_index) = if self.queue_active && !self.queue_tracks.is_empty() {
            (self.queue_tracks.len(), self.queue_selected)
        } else {
            (self.tracks.len(), self.selected_track)
        };
        if queue_len == 0 {
            Self::muted_note(ui, "The queue is clear");
            return;
        }
        let limit = if self.queue_expanded { 60 } else { 8 };
        let end = (current_index + limit).min(queue_len);
        for index in current_index..end {
            let Some(track) = self.queue_track(index) else {
                continue;
            };
            let response = self.draw_queue_row(ui, &track, index == current_index);
            if response.clicked() {
                self.play_queue_track(index);
            }
            let track_id = if self.queue_active && !self.queue_tracks.is_empty() {
                self.queue_spotify_ids.get(&index).cloned()
            } else {
                self.spotify_id_for(index)
            };
            egui::Popup::context_menu(&response)
                .width(220.0)
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .frame(menu_frame())
                .show(|ui| self.track_menu(ui, &track, track_id.as_deref()));
        }
        if end < queue_len || self.queue_expanded {
            ui.add_space(4.0);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(52.0, 32.0), egui::Sense::hover());
            if icon_button_at(ui, ui.id().with("queue_more"), rect, Icon::Dots, 18.0, false)
                .on_hover_text(if self.queue_expanded { "Show less" } else { "Show more" })
                .clicked()
            {
                self.queue_expanded = !self.queue_expanded;
            }
        }
    }

    fn draw_queue_row(&mut self, ui: &mut egui::Ui, track: &Track, current: bool) -> egui::Response {
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), 64.0),
            egui::Sense::click(),
        );
        let hover = hover_t(ui, &response);
        let fill = if current { 1.0 } else { hover * 0.6 };
        if fill > 0.0 {
            ui.painter()
                .rect_filled(rect, RADIUS_MD, pal().surface_raised.gamma_multiply(fill));
        }
        let y = rect.center().y;
        if current {
            ui.painter().rect_filled(
                egui::Rect::from_center_size(egui::pos2(rect.left() + 1.0, y), egui::vec2(2.0, 28.0)),
                1,
                pal().text.gamma_multiply(0.7),
            );
        }
        let art = egui::Rect::from_min_size(egui::pos2(rect.left() + 12.0, y - 24.0), egui::vec2(48.0, 48.0));
        self.paint_artwork(
            ui,
            art,
            &track.artwork,
            track.image_url.as_deref(),
            egui::CornerRadius::same(RADIUS_XS),
        );
        if current || hover > 0.0 {
            let t = if current { 1.0 } else { hover };
            ui.painter().rect_filled(
                art,
                RADIUS_XS,
                egui::Color32::from_black_alpha((120.0 * t) as u8),
            );
            paint_icon(ui.painter(), Icon::Play, art.center() + egui::vec2(1.0, 0.0), 14.0, pal().text.gamma_multiply(t));
        }
        let painter = ui.painter();
        let duration = painter.text(
            egui::pos2(rect.right() - 8.0, y),
            egui::Align2::RIGHT_CENTER,
            &track.duration,
            egui::FontId::proportional(12.5),
            pal().muted,
        );
        let text_x = art.right() + 20.0;
        let max_width = duration.left() - text_x - 10.0;
        paint_text(
            painter,
            egui::pos2(text_x, y - 1.0),
            egui::Align2::LEFT_BOTTOM,
            &track.title,
            egui::FontId::proportional(14.0),
            pal().text,
            max_width,
        );
        paint_text(
            painter,
            egui::pos2(text_x, y + 3.0),
            egui::Align2::LEFT_TOP,
            &track.artist,
            egui::FontId::proportional(12.5),
            pal().muted,
            max_width,
        );
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    }

    // ----------------------------------------------------------------------
    // Player: track info, progress, transport, and the bottom strip
    // ----------------------------------------------------------------------

    fn current_track_id(&self) -> Option<String> {
        if self.queue_active && !self.queue_tracks.is_empty() {
            self.queue_spotify_ids.get(&self.queue_selected).cloned()
        } else {
            self.spotify_id_for(self.selected_track)
        }
    }

    fn toggle_like_current(&mut self) {
        let Some(track_id) = self.current_track_id() else {
            return;
        };
        let saved = !self.liked_song_ids.contains(&track_id);
        self.set_liked_state(&track_id, saved);
        if self.connection_state == ConnectionState::Ready {
            self.spotify.set_track_saved(track_id, saved);
        }
    }

    fn draw_player(&mut self, ui: &mut egui::Ui, rect: egui::Rect, wide_layout: bool) {
        let track = self.current_track().clone();
        let id = ui.id().with("player");
        // The controls keep their natural spacing; extra height from resizing is
        // shared evenly above and below them.
        let top = rect.top() + ((rect.height() - PLAYER_HEIGHT) * 0.5).max(0.0);

        // Track info: the title and artist slide up into place on track change.
        let now = ui.input(|input| input.time);
        let track_key = egui::Id::new((&track.title, &track.artist));
        let (last_key, changed_at) = ui
            .data(|data| data.get_temp::<(egui::Id, f64)>(id.with("track_change")))
            .unwrap_or((track_key, f64::NEG_INFINITY));
        let changed_at = if last_key != track_key { now } else { changed_at };
        ui.data_mut(|data| data.insert_temp(id.with("track_change"), (track_key, changed_at)));
        let enter = egui::emath::easing::cubic_out(((now - changed_at) as f32 / ANIM_SLOW).clamp(0.0, 1.0));
        if enter < 1.0 {
            ui.ctx().request_repaint();
        }
        let lift = 8.0 * (1.0 - enter);
        let text_x = rect.left() + 26.0;
        let text_width = (rect.width() * 0.34).clamp(120.0, 260.0);
        let title = paint_text(
            ui.painter(),
            egui::pos2(text_x, top + 22.0 + lift),
            egui::Align2::LEFT_CENTER,
            &track.title,
            egui::FontId::proportional(16.0),
            pal().text.gamma_multiply(enter),
            text_width,
        );
        let artist = paint_text(
            ui.painter(),
            egui::pos2(text_x, top + 44.0 + lift * 1.5),
            egui::Align2::LEFT_CENTER,
            &track.artist,
            egui::FontId::proportional(13.0),
            pal().muted.gamma_multiply(enter),
            text_width,
        );
        let heart_x = title.right().max(artist.right()) + 56.0;
        let liked = self
            .current_track_id()
            .is_some_and(|track_id| self.liked_song_ids.contains(&track_id));
        let heart = egui::Rect::from_center_size(egui::pos2(heart_x, top + 32.0), egui::vec2(32.0, 32.0));
        if icon_button_at(
            ui,
            id.with("like"),
            heart,
            if liked { Icon::HeartFilled } else { Icon::Heart },
            18.0,
            false,
        )
        .on_hover_text(if liked { "Remove from Liked" } else { "Save to Liked" })
        .clicked()
        {
            self.toggle_like_current();
        }
        let dots = heart.translate(egui::vec2(50.0, 0.0));
        let dots_response = icon_button_at(ui, id.with("more"), dots, Icon::Dots, 18.0, false)
            .on_hover_text("More");
        let track_id = self.current_track_id();
        egui::Popup::from_toggle_button_response(&dots_response)
            .kind(egui::PopupKind::Menu)
            .align(egui::RectAlign::TOP_START)
            .gap(6.0)
            .width(220.0)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .frame(menu_frame())
            .show(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                if menu_item(ui, Icon::NowPlaying, "Now playing", self.section == Section::Home).clicked() {
                    self.now_playing_tab = NowPlayingTab::Lyrics;
                    self.open_section(Section::Home);
                    ui.close();
                }
                if menu_item(ui, Icon::Waveform, "Song credits", false).clicked() {
                    self.now_playing_tab = NowPlayingTab::Credits;
                    self.open_section(Section::Home);
                    ui.close();
                }
                if menu_item(ui, Icon::Queue, "Show queue", false).clicked() {
                    self.right_panel_tab = RightPanelTab::Queue;
                    if wide_layout {
                        self.queue_panel_visible = true;
                    } else {
                        self.toggle_queue_view(false);
                    }
                    ui.close();
                }
                self.track_menu_items(ui, &track, track_id.as_deref(), true);
                let label = match self.sleep_timer {
                    None => "Sleep timer".to_owned(),
                    Some(SleepTimer::EndOfTrack) => "Sleep timer · end of song".to_owned(),
                    Some(SleepTimer::At(at)) => {
                        let minutes = at.saturating_duration_since(Instant::now()).as_secs().div_ceil(60);
                        format!("Sleep timer · {minutes} min")
                    }
                };
                submenu(ui, Icon::Clock, &label, |ui| {
                    ui.set_min_width(180.0);
                    ui.spacing_mut().item_spacing.y = 2.0;
                    let mut choice = None;
                    for minutes in [5u64, 15, 30, 45, 60, 90] {
                        if menu_item(ui, Icon::Clock, &format!("{minutes} minutes"), false).clicked() {
                            choice = Some(Some(SleepTimer::At(
                                Instant::now() + Duration::from_secs(minutes * 60),
                            )));
                        }
                    }
                    if menu_item(
                        ui,
                        Icon::Clock,
                        "End of this song",
                        self.sleep_timer == Some(SleepTimer::EndOfTrack),
                    )
                    .clicked()
                    {
                        choice = Some(Some(SleepTimer::EndOfTrack));
                    }
                    if self.sleep_timer.is_some()
                        && menu_item(ui, Icon::Close, "Turn off", false).clicked()
                    {
                        choice = Some(None);
                    }
                    if let Some(timer) = choice {
                        self.sleep_timer = timer;
                        self.show_notice(match timer {
                            Some(SleepTimer::At(_)) => "Sleep timer set",
                            Some(SleepTimer::EndOfTrack) => "Playback will pause after this song",
                            None => "Sleep timer off",
                        });
                        ui.close();
                    }
                });
            });

        // Progress
        let bar_left = rect.left() + (rect.width() * 0.26).clamp(26.0, 204.0);
        let bar_right = rect.right() - 38.0;
        let bar_y = top + 76.0;
        let hit = egui::Rect::from_min_max(egui::pos2(bar_left, bar_y - 8.0), egui::pos2(bar_right, bar_y + 8.0));
        let bar_hover = animate(ui, id.with("progress_hover"), ui.rect_contains_pointer(hit), ANIM_FAST);
        // Glide on large jumps (new track, resume); regular ticks snap.
        let target = self.progress.clamp(0.0, 1.0);
        let state_id = id.with("progress_target");
        let (last_target, last_jump) = ui
            .data(|data| data.get_temp::<(f32, f64)>(state_id))
            .unwrap_or((target, f64::NEG_INFINITY));
        let last_jump = if (target - last_target).abs() > 0.02 { now } else { last_jump };
        ui.data_mut(|data| data.insert_temp(state_id, (target, last_jump)));
        let glide = if now - last_jump < 0.3 { 0.25 } else { 0.0 };
        let progress = ui.ctx().animate_value_with_time(id.with("progress"), target, glide);
        let rail = egui::Stroke::new(1.5, pal().text.gamma_multiply(0.2));
        ui.painter().hline(bar_left..=bar_right, bar_y, rail);
        let knob_x = bar_left + (bar_right - bar_left) * progress;
        ui.painter()
            .hline(bar_left..=knob_x, bar_y, egui::Stroke::new(1.5, pal().accent));
        ui.painter()
            .circle_filled(egui::pos2(knob_x, bar_y), 4.5 + 1.5 * bar_hover, pal().accent);
        let time_font = egui::FontId::proportional(12.0);
        ui.painter().text(
            egui::pos2(bar_left, bar_y + 15.0),
            egui::Align2::LEFT_CENTER,
            format_duration(self.position_ms.min(track.duration_ms())),
            time_font.clone(),
            pal().muted,
        );
        ui.painter().text(
            egui::pos2(bar_right, bar_y + 15.0),
            egui::Align2::RIGHT_CENTER,
            &track.duration,
            time_font,
            pal().muted,
        );

        // Transport
        let center_x = (bar_left + bar_right) * 0.5;
        let controls_y = top + 118.0;
        let spread = ((bar_right - bar_left) / 620.0).clamp(0.62, 1.0);
        let small = egui::vec2(34.0, 34.0);
        let repeat_icon = if self.repeat == RepeatMode::One {
            Icon::RepeatOne
        } else {
            Icon::Repeat
        };
        let at = |offset: f32| egui::Rect::from_center_size(egui::pos2(center_x + offset * spread, controls_y), small);
        if icon_button_at(ui, id.with("shuffle"), at(-172.0), Icon::Shuffle, 17.0, self.shuffle)
            .on_hover_text(if self.shuffle { "Disable shuffle" } else { "Enable shuffle" })
            .clicked()
        {
            self.toggle_shuffle();
        }
        if icon_button_at(ui, id.with("previous"), at(-92.0), Icon::Previous, 16.0, false)
            .on_hover_text("Previous")
            .clicked()
        {
            self.previous_track();
        }
        let play_rect = egui::Rect::from_center_size(egui::pos2(center_x, controls_y), egui::vec2(46.0, 46.0));
        let play = ui
            .interact(play_rect, id.with("play"), egui::Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let play_hover = hover_t(ui, &play);
        let play_press = press_t(ui, &play);
        let play_radius = 22.0 + play_hover - 1.5 * play_press;
        if play_hover > 0.0 {
            ui.painter().circle_filled(
                play_rect.center(),
                play_radius,
                pal().surface_raised.gamma_multiply(play_hover),
            );
        }
        ui.painter().circle_stroke(
            play_rect.center(),
            play_radius,
            egui::Stroke::new(1.3, pal().accent.gamma_multiply(0.9)),
        );
        // Morph between play and pause: one icon shrinks away as the other grows in.
        let pausing = animate(ui, id.with("pausing"), self.playing, ANIM_MEDIUM);
        if pausing < 1.0 {
            paint_icon(
                ui.painter(),
                Icon::Play,
                play_rect.center() + egui::vec2(2.0, 0.0),
                18.0 * (1.0 - 0.4 * pausing),
                pal().text.gamma_multiply(1.0 - pausing),
            );
        }
        if pausing > 0.0 {
            paint_icon(
                ui.painter(),
                Icon::Pause,
                play_rect.center(),
                18.0 * (0.6 + 0.4 * pausing),
                pal().text.gamma_multiply(pausing),
            );
        }
        if play
            .on_hover_text(if self.playing { "Pause" } else { "Play" })
            .clicked()
        {
            self.toggle_playback();
        }
        if icon_button_at(ui, id.with("next"), at(88.0), Icon::Next, 16.0, false)
            .on_hover_text("Next")
            .clicked()
        {
            self.next_track();
        }
        let repeat_label = match self.repeat {
            RepeatMode::Off => "Enable repeat",
            RepeatMode::All => "Repeat one",
            RepeatMode::One => "Disable repeat",
        };
        if icon_button_at(ui, id.with("repeat"), at(170.0), repeat_icon, 17.0, self.repeat != RepeatMode::Off)
            .on_hover_text(repeat_label)
            .clicked()
        {
            self.cycle_repeat();
        }
    }

    fn draw_equalizer(&mut self, ui: &mut egui::Ui) {
        let mut settings = self.eq_settings;
        let enabled_t = animate(ui, ui.id().with("eq_enabled"), settings.enabled, ANIM_MEDIUM);

        // Header: title and on/off switch.
        let (header, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 30.0), egui::Sense::hover());
        ui.painter().text(
            header.left_center(),
            egui::Align2::LEFT_CENTER,
            "Equaliser",
            egui::FontId::proportional(17.0),
            pal().text,
        );
        let switch = egui::Rect::from_center_size(header.right_center() - egui::vec2(22.0, 0.0), egui::vec2(44.0, 24.0));
        let switch_response = ui
            .interact(switch, ui.id().with("eq_switch"), egui::Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(if settings.enabled { "Turn equaliser off" } else { "Turn equaliser on" });
        if switch_response.clicked() {
            settings.enabled = !settings.enabled;
        }
        ui.painter().rect_filled(switch, 12, mix(pal().surface_hover, pal().text, enabled_t));
        ui.painter().circle_filled(
            egui::pos2(switch.left() + 12.0 + 20.0 * enabled_t, switch.center().y),
            8.5,
            mix(pal().muted, pal().background, enabled_t),
        );
        ui.add_space(14.0);

        // Presets; an edited curve shows as "Custom".
        let current = EqualizerPreset::from_gains(&settings.gains_db);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
            for preset in EqualizerPreset::ALL {
                if chip(ui, preset.short_label(), settings.enabled && current == Some(preset)).clicked() {
                    settings.gains_db = preset.gains_db();
                    settings.enabled = true;
                }
            }
            if current.is_none() {
                chip(ui, "Custom", settings.enabled);
            }
        });
        ui.add_space(16.0);

        // Graph: dB grid, response curve, and one draggable handle per band.
        let (graph, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 230.0), egui::Sense::hover());
        let dim = 0.35 + 0.65 * enabled_t;
        let plot = egui::Rect::from_min_max(
            egui::pos2(graph.left() + 34.0, graph.top() + 18.0),
            egui::pos2(graph.right() - 6.0, graph.bottom() - 30.0),
        );
        let range = f32::from(MAX_EQ_GAIN_DB);
        let y_for = |db: f32| plot.center().y - (db / range).clamp(-1.0, 1.0) * plot.height() * 0.5;
        let column_width = plot.width() / NUM_EQ_BANDS as f32;
        let x_for_band = |band: usize| plot.left() + (band as f32 + 0.5) * column_width;
        let painter = ui.painter();
        for db in [12.0, 6.0, 0.0, -6.0, -12.0_f32] {
            let y = y_for(db);
            painter.hline(
                plot.x_range(),
                y,
                egui::Stroke::new(1.0, if db == 0.0 { pal().border.gamma_multiply(1.6) } else { pal().border }),
            );
            painter.text(
                egui::pos2(graph.left(), y),
                egui::Align2::LEFT_CENTER,
                if db == 0.0 { "0".to_owned() } else { format!("{db:+}") },
                egui::FontId::proportional(10.5),
                pal().subtle,
            );
        }
        // The bands are one octave apart, so frequency is linear in x.
        let first = x_for_band(0);
        let last = x_for_band(NUM_EQ_BANDS - 1);
        let octaves = (EQ_FREQUENCIES_HZ[NUM_EQ_BANDS - 1] / EQ_FREQUENCIES_HZ[0]).log2();
        let curve = (0..=160)
            .map(|step| {
                let t = step as f32 / 160.0;
                let frequency = EQ_FREQUENCIES_HZ[0] * 2.0_f64.powf(f64::from(t) * octaves);
                egui::pos2(first + (last - first) * t, y_for(settings.response_db(frequency) as f32))
            })
            .collect::<Vec<_>>();
        let zero = y_for(0.0);
        let mut fill = egui::Mesh::default();
        for (index, point) in curve.iter().enumerate() {
            let color = pal().text.gamma_multiply(0.07 * dim);
            fill.colored_vertex(*point, color);
            fill.colored_vertex(egui::pos2(point.x, zero), color);
            if index > 0 {
                let base = (index as u32 - 1) * 2;
                fill.add_triangle(base, base + 1, base + 2);
                fill.add_triangle(base + 1, base + 2, base + 3);
            }
        }
        painter.add(fill);
        painter.add(egui::Shape::line(curve, egui::Stroke::new(1.8, pal().text.gamma_multiply(0.85 * dim))));

        for band in 0..NUM_EQ_BANDS {
            let x = x_for_band(band);
            let column = egui::Rect::from_min_max(
                egui::pos2(x - column_width * 0.5, plot.top() - 10.0),
                egui::pos2(x + column_width * 0.5, plot.bottom() + 10.0),
            );
            let response = ui
                .interact(column, ui.id().with(("eq_band", band)), egui::Sense::click_and_drag())
                .on_hover_cursor(egui::CursorIcon::ResizeVertical);
            if response.double_clicked() {
                settings.set_band(band, 0);
                settings.enabled = true;
            } else if (response.dragged() || response.clicked())
                && let Some(pointer) = response.interact_pointer_pos()
            {
                let db = ((plot.center().y - pointer.y) / (plot.height() * 0.5) * range).round();
                settings.set_band(band, db.clamp(f32::from(MIN_EQ_GAIN_DB), range) as i8);
                settings.enabled = true;
            }
            let gain = f32::from(settings.gains_db[band]);
            let knob = egui::pos2(x, y_for(gain));
            let focus = animate(ui, response.id.with("focus"), response.hovered() || response.dragged(), ANIM_FAST);
            let painter = ui.painter();
            painter.vline(x, plot.y_range(), egui::Stroke::new(1.0, pal().text.gamma_multiply(0.08 + 0.1 * focus)));
            painter.circle_filled(knob, 6.0 + 1.5 * focus, pal().text.gamma_multiply(dim));
            painter.circle_stroke(knob, 6.0 + 1.5 * focus, egui::Stroke::new(2.0, pal().surface_raised));
            if focus > 0.0 {
                painter.text(
                    knob - egui::vec2(0.0, 16.0),
                    egui::Align2::CENTER_BOTTOM,
                    format!("{:+} dB", settings.gains_db[band]),
                    egui::FontId::proportional(11.0),
                    pal().text.gamma_multiply(focus),
                );
            }
            let frequency = EQ_FREQUENCIES_HZ[band];
            painter.text(
                egui::pos2(x, graph.bottom() - 8.0),
                egui::Align2::CENTER_CENTER,
                if frequency >= 1_000.0 {
                    format!("{}k", frequency / 1_000.0)
                } else {
                    format!("{frequency}")
                },
                egui::FontId::proportional(11.0),
                pal().muted,
            );
        }
        ui.add_space(10.0);

        // Footer: reset and the automatic headroom applied to avoid clipping.
        ui.horizontal(|ui| {
            if pill_button(ui, "Reset", ButtonKind::Ghost).clicked() {
                settings.gains_db = [0; NUM_EQ_BANDS];
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let note = if !settings.enabled {
                    "Bypassed".to_owned()
                } else if settings.is_flat() {
                    "Flat · drag a band to shape the sound".to_owned()
                } else {
                    format!("Preamp {:+.1} dB · double-click a band to reset it", settings.auto_preamp_db())
                };
                ui.label(egui::RichText::new(note).size(11.5).color(pal().subtle));
            });
        });

        if settings != self.eq_settings {
            self.eq_settings = settings;
            if let Ok(mut shared) = self.audio.equalizer.lock() {
                *shared = settings;
            }
            self.eq_unsaved = true;
        }
    }

    fn draw_bottom_strip(&mut self, ui: &mut egui::Ui, wide_layout: bool) {
        let rect = ui.max_rect();
        let id = ui.id().with("strip");
        let cy = rect.center().y;
        let button = |x: f32| egui::Rect::from_center_size(egui::pos2(x, cy), egui::vec2(38.0, 38.0));

        let eq_popup = id.with("equalizer_popup");
        let eq_open = egui::Popup::is_id_open(ui.ctx(), eq_popup);
        let eq_button = icon_button_at(ui, id.with("equalizer"), button(rect.left() + 42.0), Icon::Waveform, 20.0, eq_open)
            .on_hover_text("Equaliser");
        egui::Popup::from_toggle_button_response(&eq_button)
            .id(eq_popup)
            .kind(egui::PopupKind::Menu)
            .align(egui::RectAlign::TOP_START)
            .gap(10.0)
            .width(560.0)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .frame(menu_frame().inner_margin(egui::Margin::same(20)))
            .show(|ui| self.draw_equalizer(ui));
        // Persist the curve once a drag or click has finished.
        if self.eq_unsaved && !ui.input(|input| input.pointer.any_down()) {
            if let Err(error) = self.eq_settings.save(&equalizer_path()) {
                log::warn!("Could not save the equalizer settings: {error}");
            }
            self.eq_unsaved = false;
        }

        if icon_button_at(ui, id.with("queue"), button(rect.right() - 34.0), Icon::Queue, 18.0, false)
            .on_hover_text("Queue")
            .clicked()
        {
            self.toggle_queue_view(wide_layout);
        }
        let liked = self
            .current_track_id()
            .is_some_and(|track_id| self.liked_song_ids.contains(&track_id));
        if icon_button_at(
            ui,
            id.with("like"),
            button(rect.right() - 84.0),
            if liked { Icon::HeartFilled } else { Icon::Heart },
            18.0,
            false,
        )
        .on_hover_text(if liked { "Remove from Liked" } else { "Save to Liked" })
        .clicked()
        {
            self.toggle_like_current();
        }
        if icon_button_at(
            ui,
            id.with("now_playing"),
            button(rect.right() - 134.0),
            Icon::NowPlaying,
            18.0,
            false,
        )
        .on_hover_text("Now playing")
        .clicked()
        {
            self.open_section(Section::Home);
        }

        // Volume
        let slider = egui::Rect::from_min_max(
            egui::pos2(rect.right() - 390.0, cy - 9.0),
            egui::pos2(rect.right() - 196.0, cy + 9.0),
        );
        let volume_response = ui
            .interact(slider, id.with("volume"), egui::Sense::click_and_drag())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if (volume_response.dragged() || volume_response.clicked())
            && let Some(pointer) = volume_response.interact_pointer_pos()
        {
            let volume = ((pointer.x - slider.left()) / slider.width()).clamp(0.0, 1.0);
            if (volume - self.volume).abs() > f32::EPSILON {
                self.set_volume(volume);
            }
        }
        let active = animate(
            ui,
            id.with("volume_active"),
            volume_response.hovered() || volume_response.dragged(),
            ANIM_FAST,
        );
        ui.painter().hline(
            slider.x_range(),
            cy,
            egui::Stroke::new(1.5, pal().text.gamma_multiply(0.2)),
        );
        let level = slider.left() + slider.width() * self.volume.clamp(0.0, 1.0);
        ui.painter()
            .hline(slider.left()..=level, cy, egui::Stroke::new(1.5, pal().accent));
        ui.painter()
            .circle_filled(egui::pos2(level, cy), 4.5 + 1.5 * active, pal().accent);
        volume_response.on_hover_text(format!("Volume {}%", (self.volume * 100.0).round() as u32));
        let volume_icon = Icon::Volume(if self.volume <= 0.001 {
            0
        } else if self.volume < 0.5 {
            1
        } else {
            2
        });
        paint_icon(
            ui.painter(),
            volume_icon,
            egui::pos2(slider.left() - 26.0, cy),
            17.0,
            pal().muted,
        );
    }

    fn draw_root(&mut self, ui: &mut egui::Ui) {
        if ui.input(|input| input.viewport().close_requested())
            && !self.quitting
            && self.prefs.close_to_tray
            && self.tray.is_some()
        {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.hide_to_tray(ui.ctx());
            return;
        }
        // eframe may still run the UI pass for a hidden window on Windows;
        // skip building it so nothing is laid out or requested while in the tray.
        if self.window_hidden {
            return;
        }
        self.poll_theme_dialog();
        self.apply_theme(ui.ctx());
        self.update_background_image(ui.ctx());

        self.fit_window_to_monitor(ui.ctx());
        let viewport = ui.ctx().viewport_rect();
        self.paint_background(ui, viewport);
        let layout = Layout::fit(&self.prefs, viewport.size());
        let wide_layout = layout.queue_fits;
        self.advance_disc();
        // Panels stay unfilled so the one background (colour, image and window
        // transparency) shows through all of them.
        let plain = egui::Frame::new();

        egui::Panel::top("title_bar")
            .exact_size(TITLE_BAR_HEIGHT)
            .show_separator_line(false)
            .frame(plain)
            .show(ui, |ui| self.draw_title_bar(ui));

        egui::Panel::bottom("bottom_strip")
            .exact_size(BOTTOM_STRIP_HEIGHT)
            .show_separator_line(false)
            .frame(plain)
            .show(ui, |ui| self.draw_bottom_strip(ui, wide_layout));

        egui::Panel::left("sidebar")
            .exact_size(layout.sidebar_width)
            .resizable(false)
            .show_separator_line(false)
            .frame(plain)
            .show(ui, |ui| self.draw_sidebar(ui));

        // The queue slides in and out rather than popping.
        let mut panel_open = self.queue_panel_visible && wide_layout;
        let queue_panel = egui::Panel::right("queue_panel")
            .exact_size(layout.queue_width)
            .resizable(false)
            .show_separator_line(false)
            .frame(plain)
            .show_collapsible(ui, &mut panel_open, |ui| self.draw_queue_panel(ui))
            .map(|shown| shown.response.rect);

        // Fade and lift the page in whenever the user navigates somewhere new.
        let page_key = (
            self.section,
            self.detail
                .as_ref()
                .map(DetailPage::key)
                .or_else(|| self.playlist_name.clone()),
        );
        if page_key != self.page_key {
            self.page_key = page_key;
            self.page_entered = Instant::now();
        }
        let page_elapsed = self.page_entered.elapsed().as_secs_f32();
        let page_t = egui::emath::easing::cubic_out((page_elapsed / PAGE_TRANSITION).min(1.0));
        if page_t < 1.0 {
            ui.ctx().request_repaint();
        }

        let central = egui::CentralPanel::default()
            .frame(plain)
            .show(ui, |ui| {
                let rect = ui.max_rect();
                let player_rect = egui::Rect::from_min_max(
                    egui::pos2(rect.left(), rect.bottom() - layout.player_height),
                    rect.max,
                );
                // Leave room on the right so floating scroll bars sit beside the content.
                let scroll_gutter = 12;
                let content_rect = egui::Rect::from_min_max(
                    rect.min + egui::vec2(CONTENT_INSET, 30.0),
                    egui::pos2(
                        rect.right() - 36.0 + scroll_gutter as f32,
                        player_rect.top() - 8.0,
                    ),
                )
                .translate(egui::vec2(0.0, 14.0 * (1.0 - page_t)));
                let mut page = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(content_rect)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                page.set_clip_rect(content_rect.expand2(egui::vec2(8.0, 0.0)));
                page.multiply_opacity(page_t);
                let gutter = egui::Margin {
                    right: scroll_gutter,
                    ..Default::default()
                };
                if self.page_scrolls_itself() {
                    egui::Frame::new()
                        .inner_margin(gutter)
                        .show(&mut page, |ui| self.draw_page(ui));
                } else {
                    egui::ScrollArea::vertical()
                        .id_salt(("page", self.section as u8))
                        .auto_shrink([false, false])
                        .show(&mut page, |ui| {
                            egui::Frame::new()
                                .inner_margin(gutter)
                                .show(ui, |ui| self.draw_page(ui));
                            ui.add_space(24.0);
                        });
                }
                self.draw_player(ui, player_rect, wide_layout);
            })
            .response
            .rect;

        self.draw_splitters(ui, &layout, central, queue_panel);
        Self::handle_resize_edges(ui);
    }

    /// Drag handles between the sections. Dragging resizes and saves the
    /// preference; double-clicking restores the default size.
    fn draw_splitters(
        &mut self,
        ui: &egui::Ui,
        layout: &Layout,
        central: egui::Rect,
        queue_panel: Option<egui::Rect>,
    ) {
        let top = central.top();
        let bottom = central.bottom();
        let player_top = central.bottom() - layout.player_height;

        let sidebar_x = central.left();
        if let Some(pointer) = splitter(ui, "sidebar", egui::Rect::from_min_max(
            egui::pos2(sidebar_x - 4.0, top),
            egui::pos2(sidebar_x + 4.0, bottom),
        ), true, &mut self.prefs.sidebar_width, SIDEBAR_WIDTH) {
            self.prefs.sidebar_width = (pointer.x - ui.ctx().viewport_rect().left())
                .clamp(*SIDEBAR_WIDTH_RANGE.start(), *SIDEBAR_WIDTH_RANGE.end());
        }

        if let Some(queue) = queue_panel.filter(|rect| (rect.width() - layout.queue_width).abs() < 1.0) {
            let queue_x = queue.left();
            if let Some(pointer) = splitter(ui, "queue", egui::Rect::from_min_max(
                egui::pos2(queue_x - 4.0, top),
                egui::pos2(queue_x + 4.0, bottom),
            ), true, &mut self.prefs.queue_width, QUEUE_PANEL_WIDTH) {
                self.prefs.queue_width = (ui.ctx().viewport_rect().right() - pointer.x)
                    .clamp(*QUEUE_WIDTH_RANGE.start(), *QUEUE_WIDTH_RANGE.end());
            }
        }

        if let Some(pointer) = splitter(ui, "player", egui::Rect::from_min_max(
            egui::pos2(central.left(), player_top - 4.0),
            egui::pos2(central.right(), player_top + 4.0),
        ), false, &mut self.prefs.player_height, PLAYER_HEIGHT) {
            self.prefs.player_height = (central.bottom() - pointer.y)
                .clamp(*PLAYER_HEIGHT_RANGE.start(), *PLAYER_HEIGHT_RANGE.end());
        }
    }

    /// Keeps the window usable after it moves to another monitor: if it no
    /// longer fits (a smaller screen, or a higher display scale), shrink it.
    fn fit_window_to_monitor(&mut self, ctx: &egui::Context) {
        let (monitor, inner, maximized, fullscreen) = ctx.input(|input| {
            let viewport = input.viewport();
            (
                viewport.monitor_size,
                viewport.inner_rect,
                viewport.maximized.unwrap_or(false),
                viewport.fullscreen.unwrap_or(false),
            )
        });
        let (Some(monitor), Some(inner)) = (monitor, inner) else {
            return;
        };
        if self.last_monitor_size == Some(monitor) {
            return;
        }
        self.last_monitor_size = Some(monitor);
        if maximized || fullscreen {
            return;
        }
        // Leave room for the taskbar.
        let limit = (monitor - egui::vec2(24.0, 64.0)).max(egui::vec2(760.0, 540.0));
        let size = inner.size();
        if size.x > limit.x || size.y > limit.y {
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size.min(limit)));
        }
    }

    /// Non-drawing work: runs from `App::logic`, so it keeps going while the
    /// window is hidden in the tray (playback auto-advance depends on it).
    fn tick(&mut self, ctx: &egui::Context) {
        let events_changed = self.poll_spotify(ctx);
        if let Some(SleepTimer::At(at)) = self.sleep_timer {
            let now = Instant::now();
            if now >= at {
                self.sleep_timer = None;
                if self.playing {
                    self.toggle_playback();
                }
                self.save_session();
                self.show_notice("Sleep timer paused playback");
            } else {
                ctx.request_repaint_after(at - now);
            }
        }
        if self.queue_active && self.last_session_save.elapsed() >= Duration::from_secs(5) {
            self.save_session();
            self.last_session_save = Instant::now();
        }
        self.handle_tray(ctx);
        if self.prefs.update_mode == UpdateMode::Automatic
            && self.started_at.elapsed() > Duration::from_secs(15)
            && self
                .updater
                .last_checked()
                .is_none_or(|checked| checked.elapsed() >= UPDATE_CHECK_INTERVAL)
        {
            self.updater.check(ctx, true);
        }
        if self.window_hidden {
            // No painting happens while hidden; just keep draining Spotify events.
            ctx.request_repaint_after(Duration::from_millis(500));
            return;
        }
        let repaint_delay = match self.connection_state {
            ConnectionState::Ready
                if self.playing || events_changed || !self.artwork_queue.is_empty() =>
            {
                Duration::from_millis(250)
            }
            ConnectionState::Ready => Duration::from_millis(1_000),
            ConnectionState::Authenticating | ConnectionState::Connecting => {
                Duration::from_millis(250)
            }
            ConnectionState::Disconnected => Duration::from_millis(1000),
        };
        let minimized = ctx.input(|input| input.viewport().minimized.unwrap_or(false));
        let repaint_delay = if self.playing && !minimized {
            // Keep the spinning record smooth; only while it is on screen.
            repaint_delay.min(DISC_FRAME_INTERVAL)
        } else {
            repaint_delay
        };
        if self.playing
            || events_changed
            || !self.artwork_queue.is_empty()
            || matches!(
                self.connection_state,
                ConnectionState::Authenticating | ConnectionState::Connecting
            )
        {
            ctx.request_repaint_after(repaint_delay);
        } else {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
    }

    fn handle_tray(&mut self, ctx: &egui::Context) {
        let Some(tray) = self.tray.as_ref() else {
            return;
        };
        let mut actions = Vec::new();
        while let Some(action) = tray.next_action() {
            actions.push(action);
        }
        for action in actions {
            match action {
                TrayAction::Show => self.show_window(ctx),
                TrayAction::PlayPause => self.toggle_playback(),
                TrayAction::Next => self.next_track(),
                TrayAction::Previous => self.previous_track(),
                TrayAction::Quit => {
                    self.quitting = true;
                    self.save_session();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }

        let tooltip = if self.connection_state == ConnectionState::Ready && self.queue_active {
            let track = self.current_track();
            format!("Oynx — {} · {}", track.title, track.artist)
        } else {
            "Oynx".to_owned()
        };
        let playing = self.playing;
        if let Some(tray) = self.tray.as_mut() {
            tray.update(&tooltip, playing);
        }
    }

    /// Hides the window but keeps the process (and playback) running in the tray.
    fn hide_to_tray(&mut self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        self.window_hidden = true;
        self.save_session();
        // Album art is cached on disk, so drop the textures to free memory;
        // they are reloaded on demand once the window is shown again.
        self.artwork_textures.clear();
        self.artwork_queue.clear();
        self.pending_artwork.clear();
    }

    fn show_window(&mut self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        self.window_hidden = false;
        ctx.request_repaint();
    }

}

#[derive(Clone, Copy)]
struct TrackColumns {
    width: f32,
    album: f32,
}

impl TrackColumns {
    fn album_left(self, rect: egui::Rect) -> f32 {
        rect.right() - 104.0 - self.album
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ButtonKind {
    Primary,
    Secondary,
    Ghost,
}

fn font_light(size: f32) -> egui::FontId {
    egui::FontId::new(size, egui::FontFamily::Name(FAMILY_LIGHT.into()))
}

fn font_medium(size: f32) -> egui::FontId {
    egui::FontId::new(size, egui::FontFamily::Name(FAMILY_MEDIUM.into()))
}

fn font_bold(size: f32) -> egui::FontId {
    egui::FontId::new(size, egui::FontFamily::Name(FAMILY_BOLD.into()))
}

/// Paints single-line text, truncating it with an ellipsis to fit `max_width`.
fn paint_text(
    painter: &egui::Painter,
    pos: egui::Pos2,
    align: egui::Align2,
    text: &str,
    font: egui::FontId,
    color: egui::Color32,
    max_width: f32,
) -> egui::Rect {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_width.max(1.0));
    let galley = painter.layout_job(job);
    let rect = align.anchor_size(pos, galley.size());
    painter.galley(rect.min, galley, color);
    rect
}

/// Paints single-line text with extra space between letters (wordmark, overlines).
fn paint_text_spaced(
    painter: &egui::Painter,
    pos: egui::Pos2,
    align: egui::Align2,
    text: &str,
    font: egui::FontId,
    color: egui::Color32,
    letter_spacing: f32,
) -> egui::Rect {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        text,
        0.0,
        egui::TextFormat {
            font_id: font,
            color,
            extra_letter_spacing: letter_spacing,
            ..Default::default()
        },
    );
    let galley = painter.layout_job(job);
    let rect = align.anchor_size(pos, galley.size());
    painter.galley(rect.min, galley, color);
    rect
}

/// Maps analyser bands onto `count` waveform ticks (0..1), mirrored around the
/// middle tick: bass in the centre, treble towards both ends. Uses the same
/// perceptual curve and spatial smoothing as MYX's spectrum renderer.
fn spectrum_ticks(values: &[f32; NUM_BANDS], peak_envelope: f32, count: usize) -> Vec<f32> {
    let peak = peak_envelope.max(1e-6);
    // The top quarter of the log bands is mostly air; leave it out.
    let usable = NUM_BANDS * 3 / 4;
    let centre = (count - 1) as f32 / 2.0;
    let side = centre.ceil().max(1.0) as usize + 1;
    let side_levels = (0..side)
        .map(|step| {
            let lo = step * usable / side;
            let hi = ((step + 1) * usable / side).max(lo + 1).min(usable);
            let average = values[lo..hi].iter().sum::<f32>() / (hi - lo) as f32;
            // A little extra gain: averaged bands sit well below the single-band peak.
            ((average / peak).sqrt() * 1.3).clamp(0.0, 1.0)
        })
        .collect::<Vec<_>>();
    let mut ticks = (0..count)
        .map(|index| side_levels[((index as f32 - centre).abs().round() as usize).min(side - 1)])
        .collect::<Vec<_>>();
    for _ in 0..2 {
        let source = ticks.clone();
        for index in 0..count {
            let left = source[index.saturating_sub(1)];
            let right = source[(index + 1).min(count - 1)];
            ticks[index] = left * 0.25 + source[index] * 0.5 + right * 0.25;
        }
    }
    ticks
}

/// One section divider. Returns the pointer position while it is dragged.
/// Hovering highlights the divider; double-clicking resets `value` to `default`.
fn splitter(
    ui: &egui::Ui,
    name: &str,
    zone: egui::Rect,
    vertical: bool,
    value: &mut f32,
    default: f32,
) -> Option<egui::Pos2> {
    let response = ui
        .interact(zone, egui::Id::new(("splitter", name)), egui::Sense::click_and_drag())
        .on_hover_cursor(if vertical {
            egui::CursorIcon::ResizeHorizontal
        } else {
            egui::CursorIcon::ResizeVertical
        });
    let active = response.hovered() || response.dragged();
    let t = animate(ui, response.id.with("active"), active, ANIM_FAST);
    if t > 0.0 {
        let stroke = egui::Stroke::new(1.0 + t, pal().text.gamma_multiply(0.35 * t));
        if vertical {
            ui.painter().vline(zone.center().x, zone.y_range(), stroke);
        } else {
            ui.painter().hline(zone.x_range(), zone.center().y, stroke);
        }
    }
    if response.double_clicked() {
        *value = default;
        return None;
    }
    if response.dragged() {
        return response.interact_pointer_pos();
    }
    None
}

/// The frame shared by popup menus.
fn menu_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(pal().surface_raised)
        .stroke(egui::Stroke::new(1.0, pal().border))
        .corner_radius(RADIUS_LG)
        .inner_margin(egui::Margin::same(6))
        .shadow(egui::Shadow {
            offset: [0, 8],
            blur: 24,
            spread: 0,
            color: egui::Color32::from_black_alpha(150),
        })
}

// --------------------------------------------------------------------------
// Animation helpers
// --------------------------------------------------------------------------

/// Eased 0→1 progress towards `on`, animated over `time` seconds. egui keeps
/// repainting while the value is in flight, so idle frames stay cheap.
fn animate(ui: &egui::Ui, id: egui::Id, on: bool, time: f32) -> f32 {
    ui.ctx()
        .animate_bool_with_time_and_easing(id, on, time, egui::emath::easing::cubic_out)
}

/// Hover progress for an interactive response.
fn hover_t(ui: &egui::Ui, response: &egui::Response) -> f32 {
    animate(ui, response.id.with("hover"), response.hovered(), ANIM_FAST)
}

/// Press progress, used for a subtle "push in" when a control is held down.
fn press_t(ui: &egui::Ui, response: &egui::Response) -> f32 {
    animate(
        ui,
        response.id.with("press"),
        response.is_pointer_button_down_on(),
        0.08,
    )
}

fn mix(from: egui::Color32, to: egui::Color32, t: f32) -> egui::Color32 {
    from.lerp_to_gamma(to, t.clamp(0.0, 1.0))
}

/// Scale factor that briefly overshoots and settles whenever `on` flips,
/// e.g. the heart "pop" when a track is liked.
fn pop_scale(ui: &egui::Ui, id: egui::Id, on: bool, amount: f32) -> f32 {
    let t = ui.ctx().animate_bool_with_time(id, on, ANIM_SLOW);
    1.0 + amount * (t * std::f32::consts::PI).sin()
}

fn pill_button(ui: &mut egui::Ui, text: &str, kind: ButtonKind) -> egui::Response {
    let font = font_medium(13.0);
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font.clone(), pal().text);
    let size = egui::vec2(galley.size().x + 32.0, 36.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let hover = hover_t(ui, &response);
    let press = press_t(ui, &response);
    let (fill, color) = match kind {
        ButtonKind::Primary => (mix(pal().accent, pal().accent_hover, hover), pal().on_accent),
        ButtonKind::Secondary => (mix(pal().surface_raised, pal().surface_hover, hover), pal().text),
        ButtonKind::Ghost => (pal().surface_raised.gamma_multiply(hover), mix(pal().muted, pal().text, hover)),
    };
    let rect = rect.shrink2(rect.size() * 0.03 * press);
    ui.painter().rect_filled(rect, 18, fill);
    ui.painter()
        .text(rect.center(), egui::Align2::CENTER_CENTER, text, font, color);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A settings row: title and detail on the left, controls on the right.
fn theme_row(ui: &mut egui::Ui, title: &str, detail: &str, add_controls: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.set_min_height(44.0);
        ui.vertical(|ui| {
            ui.add_space(4.0);
            ui.spacing_mut().item_spacing.y = 1.0;
            ui.label(egui::RichText::new(title).font(font_medium(14.0)).color(pal().text));
            ui.label(egui::RichText::new(detail).size(12.0).color(pal().muted));
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            add_controls(ui);
        });
    });
}

/// A labelled slider with its value shown on the right.
fn theme_slider(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    format: impl Fn(f32) -> String,
) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).font(font_medium(13.0)).color(pal().text));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new(format(*value)).size(12.0).color(pal().muted));
        });
    });
    ui.scope(|ui| {
        ui.spacing_mut().slider_width = ui.available_width();
        ui.add(egui::Slider::new(value, range).show_value(false));
    });
}

/// An on/off switch, the same as the equaliser's.
fn toggle_switch(ui: &mut egui::Ui, on: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(44.0, 24.0), egui::Sense::click());
    let on_t = animate(ui, response.id.with("on"), on, ANIM_MEDIUM);
    ui.painter().rect_filled(rect, 12, mix(pal().surface_hover, pal().text, on_t));
    ui.painter().circle_filled(
        egui::pos2(rect.left() + 12.0 + 20.0 * on_t, rect.center().y),
        8.5,
        mix(pal().muted, pal().background, on_t),
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Small underlined-on-hover text that acts as a button.
fn text_link(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let font = font_medium(12.0);
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font.clone(), pal().muted);
    let (rect, response) = ui.allocate_exact_size(galley.size() + egui::vec2(4.0, 8.0), egui::Sense::click());
    let hover = hover_t(ui, &response);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        font,
        mix(pal().muted, pal().text, hover),
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn credits_heading(ui: &mut egui::Ui, text: &str) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 18.0), egui::Sense::hover());
    paint_text_spaced(
        ui.painter(),
        rect.left_center(),
        egui::Align2::LEFT_CENTER,
        &text.to_uppercase(),
        egui::FontId::proportional(10.5),
        pal().subtle,
        2.0,
    );
    ui.add_space(6.0);
}

/// A name in the song credits that runs a search when clicked.
fn credit_link(ui: &mut egui::Ui, text: &str, size: f32, color: egui::Color32) -> egui::Response {
    let font = font_light(size);
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font.clone(), color);
    let (rect, response) = ui.allocate_exact_size(galley.size(), egui::Sense::click());
    let hover = hover_t(ui, &response);
    ui.painter()
        .text(rect.left_center(), egui::Align2::LEFT_CENTER, text, font, mix(color, pal().accent, hover));
    if hover > 0.0 {
        ui.painter().hline(
            rect.x_range(),
            rect.bottom(),
            egui::Stroke::new(1.0, pal().accent.gamma_multiply(hover)),
        );
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn song_link_row(ui: &mut egui::Ui, link: &SongLink) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 40.0), egui::Sense::click());
    let hover = hover_t(ui, &response);
    if hover > 0.0 {
        ui.painter()
            .rect_filled(rect, RADIUS_SM, pal().surface_raised.gamma_multiply(hover));
    }
    paint_icon(ui.painter(), Icon::Note, rect.left_center() + egui::vec2(16.0, 0.0), 15.0, pal().muted);
    let title = paint_text(
        ui.painter(),
        rect.left_center() + egui::vec2(36.0, 0.0),
        egui::Align2::LEFT_CENTER,
        &link.title,
        font_medium(14.0),
        mix(pal().text, pal().accent, hover),
        (rect.width() - 48.0) * 0.6,
    );
    if !link.artist.is_empty() {
        paint_text(
            ui.painter(),
            egui::pos2(title.right() + 10.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            &link.artist,
            egui::FontId::proportional(13.0),
            pal().muted,
            rect.right() - title.right() - 20.0,
        );
    }
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text("Search Spotify")
}

fn chip(ui: &mut egui::Ui, text: &str, selected: bool) -> egui::Response {
    let font = font_medium(12.0);
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font.clone(), pal().text);
    let size = egui::vec2(galley.size().x + 26.0, 30.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let hover = hover_t(ui, &response);
    let select = animate(ui, response.id.with("selected"), selected, ANIM_MEDIUM);
    let fill = mix(mix(pal().surface_raised, pal().surface_hover, hover), pal().text, select);
    let color = mix(pal().text, pal().background, select);
    ui.painter().rect_filled(rect, 15, fill);
    ui.painter()
        .text(rect.center(), egui::Align2::CENTER_CENTER, text, font, color);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn text_icon_button(ui: &mut egui::Ui, icon: Icon, text: &str) -> egui::Response {
    let font = font_medium(13.0);
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font.clone(), pal().text);
    let size = egui::vec2(galley.size().x + 44.0, 32.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let hover = hover_t(ui, &response);
    if hover > 0.0 {
        ui.painter()
            .rect_filled(rect, 16, pal().surface_raised.gamma_multiply(hover));
    }
    let color = mix(pal().muted, pal().text, hover);
    paint_icon(ui.painter(), icon, rect.left_center() + egui::vec2(18.0, 0.0), 14.0, color);
    ui.painter().text(
        rect.left_center() + egui::vec2(32.0, 0.0),
        egui::Align2::LEFT_CENTER,
        text,
        font,
        color,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A selectable row with a radio marker, a title, and a short description.
fn option_row(ui: &mut egui::Ui, title: &str, detail: &str, selected: bool) -> egui::Response {
    let width = ui.available_width();
    let detail_galley = ui.painter().layout(
        detail.to_owned(),
        egui::FontId::proportional(12.0),
        pal().muted,
        width - 64.0,
    );
    let height = 30.0 + detail_galley.size().y + 12.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    let hover = hover_t(ui, &response);
    if hover > 0.0 {
        ui.painter().rect_filled(rect, RADIUS_MD, pal().surface_hover.gamma_multiply(0.6 * hover));
    }
    let marker = egui::pos2(rect.left() + 22.0, rect.top() + 21.0);
    ui.painter().circle_stroke(marker, 8.0, egui::Stroke::new(1.3, if selected { pal().text } else { pal().muted }));
    if selected {
        ui.painter().circle_filled(marker, 4.0, pal().text);
    }
    ui.painter().text(
        egui::pos2(rect.left() + 44.0, marker.y),
        egui::Align2::LEFT_CENTER,
        title,
        font_medium(14.0),
        pal().text,
    );
    ui.painter().galley(egui::pos2(rect.left() + 44.0, rect.top() + 33.0), detail_galley, pal().muted);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A full-width row in a popup menu.
fn menu_item(ui: &mut egui::Ui, icon: Icon, text: &str, selected: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 38.0),
        egui::Sense::click(),
    );
    let hovered = response.hovered();
    if hovered || selected {
        ui.painter().rect_filled(rect, RADIUS_SM, pal().surface_hover);
    }
    let color = if hovered || selected { pal().text } else { pal().muted };
    paint_icon(ui.painter(), icon, rect.left_center() + egui::vec2(20.0, 0.0), 16.0, color);
    ui.painter().text(
        rect.left_center() + egui::vec2(42.0, 0.0),
        egui::Align2::LEFT_CENTER,
        text,
        font_medium(13.0),
        if hovered || selected { pal().text } else { pal().muted },
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A menu row that opens a submenu to the side when hovered.
fn submenu(ui: &mut egui::Ui, icon: Icon, text: &str, add_contents: impl FnOnce(&mut egui::Ui)) {
    let response = menu_item(ui, icon, text, false);
    paint_icon(
        ui.painter(),
        Icon::ChevronRight,
        response.rect.right_center() - egui::vec2(16.0, 0.0),
        12.0,
        pal().muted,
    );
    egui::containers::menu::SubMenu::new().show(ui, &response, add_contents);
}

/// Paints a texture clipped to a circle and rotated clockwise by `angle` radians.
fn paint_textured_circle(
    painter: &egui::Painter,
    texture_id: egui::TextureId,
    center: egui::Pos2,
    radius: f32,
    angle: f32,
) {
    const SEGMENTS: u32 = 64;
    let mut mesh = egui::Mesh::with_texture(texture_id);
    let vertex = |pos: egui::Pos2, uv: egui::Pos2| egui::epaint::Vertex {
        pos,
        uv,
        color: egui::Color32::WHITE,
    };
    mesh.vertices.push(vertex(center, egui::pos2(0.5, 0.5)));
    for step in 0..=SEGMENTS {
        let a = step as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
        let (sin, cos) = a.sin_cos();
        let (uv_sin, uv_cos) = (a - angle).sin_cos();
        mesh.vertices.push(vertex(
            center + egui::vec2(cos, sin) * radius,
            egui::pos2(0.5 + 0.5 * uv_cos, 0.5 + 0.5 * uv_sin),
        ));
    }
    for step in 1..=SEGMENTS {
        mesh.add_triangle(0, step, step + 1);
    }
    painter.add(mesh);
}

fn play_circle_button(ui: &mut egui::Ui, size: f32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click());
    let hover = hover_t(ui, &response);
    let press = press_t(ui, &response);
    let radius = size * 0.5 * (0.96 + 0.04 * hover - 0.05 * press);
    ui.painter().circle_filled(rect.center(), radius, mix(pal().accent, pal().accent_hover, hover));
    paint_icon(
        ui.painter(),
        Icon::Play,
        rect.center() + egui::vec2(1.5, 0.0),
        size * 0.36,
        pal().on_accent,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A round, borderless icon button painted at an explicit position.
/// Active buttons are tinted with the accent colour and get an indicator dot.
fn icon_button_at(
    ui: &egui::Ui,
    id: egui::Id,
    rect: egui::Rect,
    icon: Icon,
    icon_size: f32,
    active: bool,
) -> egui::Response {
    let response = ui
        .interact(rect, id, egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let hover = hover_t(ui, &response);
    let press = press_t(ui, &response);
    let active_t = animate(ui, id.with("active"), active, ANIM_MEDIUM);
    let painter = ui.painter();
    if hover > 0.0 {
        painter.circle_filled(
            rect.center(),
            rect.width() * 0.5 * (0.8 + 0.2 * hover),
            pal().surface_hover.gamma_multiply(hover),
        );
    }
    let color = mix(mix(pal().muted, pal().accent, active_t), mix(pal().text, pal().accent_hover, active_t), hover);
    let offset = egui::vec2(0.0, -2.0 * active_t);
    paint_icon(painter, icon, rect.center() + offset, icon_size * (1.0 - 0.08 * press), color);
    if active_t > 0.0 {
        painter.circle_filled(
            rect.center() + egui::vec2(0.0, icon_size * 0.5 + 3.0),
            2.0 * active_t,
            color,
        );
    }
    response
}

/// The "now playing" equalizer. While `time` is set the bars bounce, and the
/// caller is responsible for scheduling the next frame.
fn paint_equalizer(
    painter: &egui::Painter,
    center: egui::Pos2,
    size: f32,
    color: egui::Color32,
    time: Option<f64>,
) {
    let s = size / 16.0;
    let base = center.y + 6.5 * s;
    for (i, (x, rest)) in [(-4.5_f32, 8.0_f32), (0.0, 13.0), (4.5, 6.0)].into_iter().enumerate() {
        let height = match time {
            Some(time) => {
                let phase = time * (5.5 + i as f64 * 1.7) + i as f64 * 1.9;
                let wave = 0.5 + 0.5 * phase.sin() * (phase * 0.37).cos();
                3.0 + 10.0 * wave as f32
            }
            None => rest,
        };
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(center.x + (x - 1.4) * s, base - height * s),
                egui::pos2(center.x + (x + 1.4) * s, base),
            ),
            1,
            color,
        );
    }
}

/// Draws a vector icon centred on `center`, designed on a 16×16 grid and scaled to `size`.
fn paint_icon(painter: &egui::Painter, icon: Icon, center: egui::Pos2, size: f32, color: egui::Color32) {
    let s = size / 16.0;
    let p = |x: f32, y: f32| center + egui::vec2(x * s, y * s);
    let stroke = egui::Stroke::new((1.3 * s).max(1.1), color);
    let line = |points: &[(f32, f32)]| {
        painter.add(egui::Shape::line(
            points.iter().map(|&(x, y)| p(x, y)).collect(),
            stroke,
        ));
    };
    let fill = |points: &[(f32, f32)]| {
        painter.add(egui::Shape::convex_polygon(
            points.iter().map(|&(x, y)| p(x, y)).collect(),
            color,
            egui::Stroke::NONE,
        ));
    };
    let arc = |cx: f32, radius: f32, from: f32, to: f32| {
        let points = (0..=12)
            .map(|step| {
                let angle = (from + (to - from) * step as f32 / 12.0).to_radians();
                p(cx + radius * angle.cos(), radius * angle.sin())
            })
            .collect();
        painter.add(egui::Shape::line(points, stroke));
    };
    // Heart outline: bottom tip, then the left lobe and the right lobe (two arcs
    // meeting at the top notch). Starting at the tip keeps the fill correct,
    // since every point is visible from it.
    let heart = || {
        let lobe = |cx: f32, from: f32, to: f32| {
            (0..=14).map(move |step| {
                let a = (from + (to - from) * step as f32 / 14.0).to_radians();
                (cx + 3.8 * a.cos(), -2.0 + 3.8 * a.sin())
            })
        };
        std::iter::once((0.0, 7.0))
            .chain(lobe(-3.5, 135.0, 337.0))
            .chain(lobe(3.5, 203.0, 405.0))
            .map(|(x, y)| p(x, y))
            .collect::<Vec<_>>()
    };

    match icon {
        Icon::Home => {
            line(&[(-7.0, -0.5), (0.0, -7.0), (7.0, -0.5)]);
            line(&[(-5.0, -2.0), (-5.0, 6.5), (5.0, 6.5), (5.0, -2.0)]);
        }
        Icon::Search => {
            painter.circle_stroke(p(-1.5, -1.5), 5.2 * s, stroke);
            line(&[(2.4, 2.4), (7.0, 7.0)]);
        }
        Icon::Settings => {
            painter.circle_stroke(center, 2.4 * s, stroke);
            painter.circle_stroke(center, 5.2 * s, stroke);
            for tooth in 0..8 {
                let angle = tooth as f32 * std::f32::consts::FRAC_PI_4;
                let direction = egui::vec2(angle.cos(), angle.sin());
                painter.line_segment(
                    [center + direction * 5.2 * s, center + direction * 7.4 * s],
                    egui::Stroke::new(2.4 * s, color),
                );
            }
        }
        Icon::Queue => {
            line(&[(-7.0, -5.0), (7.0, -5.0)]);
            line(&[(-7.0, 0.0), (1.0, 0.0)]);
            line(&[(-7.0, 5.0), (1.0, 5.0)]);
            fill(&[(3.5, -1.0), (8.0, 2.5), (3.5, 6.0)]);
        }
        Icon::Shuffle => {
            line(&[(-7.0, -4.5), (-3.5, -4.5), (2.5, 4.5), (7.0, 4.5)]);
            line(&[(-7.0, 4.5), (-3.5, 4.5), (2.5, -4.5), (7.0, -4.5)]);
            line(&[(4.5, -7.0), (7.0, -4.5), (4.5, -2.0)]);
            line(&[(4.5, 2.0), (7.0, 4.5), (4.5, 7.0)]);
        }
        Icon::Previous => {
            fill(&[(6.0, -6.5), (6.0, 6.5), (-3.0, 0.0)]);
            painter.rect_filled(egui::Rect::from_min_max(p(-6.5, -6.5), p(-4.0, 6.5)), 1, color);
        }
        Icon::Next => {
            fill(&[(-6.0, -6.5), (3.0, 0.0), (-6.0, 6.5)]);
            painter.rect_filled(egui::Rect::from_min_max(p(4.0, -6.5), p(6.5, 6.5)), 1, color);
        }
        Icon::Play => fill(&[(-5.0, -7.5), (7.0, 0.0), (-5.0, 7.5)]),
        Icon::Pause => {
            for x in [-3.3, 3.3] {
                painter.rect_filled(
                    egui::Rect::from_center_size(p(x, 0.0), egui::vec2(3.6 * s, 13.0 * s)),
                    1,
                    color,
                );
            }
        }
        Icon::Repeat | Icon::RepeatOne => {
            line(&[(-7.0, 2.0), (-7.0, -2.5), (-5.0, -4.5), (6.0, -4.5)]);
            line(&[(4.0, -6.8), (6.3, -4.5), (4.0, -2.2)]);
            line(&[(7.0, -2.0), (7.0, 2.5), (5.0, 4.5), (-6.0, 4.5)]);
            line(&[(-4.0, 2.2), (-6.3, 4.5), (-4.0, 6.8)]);
            if icon == Icon::RepeatOne {
                painter.text(center, egui::Align2::CENTER_CENTER, "1", font_bold(7.5 * s), color);
            }
        }
        Icon::Heart => {
            painter.add(egui::Shape::closed_line(heart(), stroke));
        }
        Icon::HeartFilled => {
            painter.add(egui::Shape::convex_polygon(heart(), color, egui::Stroke::NONE));
        }
        Icon::Close => {
            line(&[(-5.0, -5.0), (5.0, 5.0)]);
            line(&[(-5.0, 5.0), (5.0, -5.0)]);
        }
        Icon::Volume(level) => {
            fill(&[(-7.0, -2.5), (-3.5, -2.5), (-3.5, 2.5), (-7.0, 2.5)]);
            fill(&[(-4.0, -2.5), (1.0, -6.5), (1.0, 6.5), (-4.0, 2.5)]);
            if level == 0 {
                line(&[(3.5, -2.5), (8.0, 2.5)]);
                line(&[(3.5, 2.5), (8.0, -2.5)]);
            } else {
                arc(1.0, 4.0, -50.0, 50.0);
                if level >= 2 {
                    arc(1.0, 7.0, -50.0, 50.0);
                }
            }
        }
        Icon::ChevronLeft => line(&[(2.5, -6.0), (-3.5, 0.0), (2.5, 6.0)]),
        Icon::ChevronRight => line(&[(-2.5, -6.0), (3.5, 0.0), (-2.5, 6.0)]),
        Icon::Dots => {
            for x in [-5.5, 0.0, 5.5] {
                painter.circle_filled(p(x, 0.0), 1.25 * s, color);
            }
        }
        Icon::Waveform => {
            for (x, height) in [(-7.0, 4.0), (-3.5, 11.0), (0.0, 16.0), (3.5, 9.0), (7.0, 5.0)] {
                line(&[(x, -height * 0.5), (x, height * 0.5)]);
            }
        }
        Icon::NowPlaying => {
            painter.rect_stroke(
                egui::Rect::from_min_max(p(-7.0, -7.0), p(7.0, 7.0)),
                (2.5 * s) as u8,
                stroke,
                egui::StrokeKind::Middle,
            );
            line(&[(-4.0, -3.5), (1.5, -3.5)]);
            line(&[(-4.0, -0.5), (-0.5, -0.5)]);
            painter.circle_stroke(p(2.5, 3.0), 2.0 * s, stroke);
        }
        Icon::Share => {
            line(&[(-1.5, -5.5), (-6.0, -5.5), (-6.0, 6.0), (5.5, 6.0), (5.5, 1.5)]);
            line(&[(1.5, -6.5), (6.5, -6.5), (6.5, -1.5)]);
            line(&[(6.5, -6.5), (-0.5, 0.5)]);
        }
        Icon::Sparkle => {
            // A four-pointed star, built from four convex arms.
            for (dx, dy) in [(0.0_f32, -1.0_f32), (1.0, 0.0), (0.0, 1.0), (-1.0, 0.0)] {
                let (nx, ny) = (-dy, dx);
                fill(&[
                    (dx * 7.5, dy * 7.5),
                    ((dx + nx) * 1.3, (dy + ny) * 1.3),
                    (0.0, 0.0),
                    ((dx - nx) * 1.3, (dy - ny) * 1.3),
                ]);
            }
        }
        Icon::Minimize => line(&[(-5.5, 0.0), (5.5, 0.0)]),
        Icon::Maximize => {
            painter.rect_stroke(
                egui::Rect::from_min_max(p(-5.0, -5.0), p(5.0, 5.0)),
                1,
                stroke,
                egui::StrokeKind::Middle,
            );
        }
        Icon::Restore => {
            painter.rect_stroke(
                egui::Rect::from_min_max(p(-5.5, -3.0), p(3.0, 5.5)),
                1,
                stroke,
                egui::StrokeKind::Middle,
            );
            line(&[(-3.0, -3.0), (-3.0, -5.5), (5.5, -5.5), (5.5, 3.0), (3.0, 3.0)]);
        }
        Icon::Note => {
            painter.circle_filled(p(-3.0, 4.5), 2.8 * s, color);
            line(&[(-0.4, 4.5), (-0.4, -6.5), (5.5, -4.5), (5.5, -1.5)]);
        }
        Icon::Clock => {
            painter.circle_stroke(center, 6.5 * s, stroke);
            line(&[(0.0, -3.5), (0.0, 0.0), (2.5, 2.0)]);
        }
    }
}

impl Track {
    fn duration_ms(&self) -> u32 {
        let mut parts = self.duration.split(':');
        let minutes = parts
            .next()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(0);
        let seconds = parts
            .next()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(0);
        minutes * 60_000 + seconds * 1_000
    }
}

/// Where the equalizer curve is saved, next to the Spotify settings.
fn theme_path() -> std::path::PathBuf {
    SpotifyConfig::path().with_file_name("theme.json")
}

fn equalizer_path() -> std::path::PathBuf {
    SpotifyConfig::path().with_file_name("equalizer.json")
}

fn format_duration(duration_ms: u32) -> String {
    let total_seconds = duration_ms / 1_000;
    format!("{}:{:02}", total_seconds / 60, total_seconds % 60)
}

impl Drop for OynxApp {
    fn drop(&mut self) {
        self.save_session();
        self.save_theme();
        self.updater.install_on_exit();
    }
}

impl eframe::App for OynxApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, UI_PREFS_KEY, &self.prefs);
        self.save_theme();
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        if self.transparent_window {
            [0.0; 4]
        } else {
            pal().background.to_normalized_gamma_f32()
        }
    }

    /// Popups and scroll positions should start fresh on each launch.
    fn persist_egui_memory(&self) -> bool {
        false
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.tick(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.apply_window_blur(frame);
        self.draw_root(ui);
    }
}

fn native_options(transparent: bool) -> eframe::NativeOptions {
    let icon = tray::app_icon(true);
    eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1360.0, 900.0])
            .with_min_inner_size([760.0, 540.0])
            .with_decorations(false)
            .with_transparent(transparent)
            .with_icon(egui::IconData {
                rgba: icon.rgba,
                width: icon.size,
                height: icon.size,
            }),
        persist_window: true,
        renderer: if transparent { eframe::Renderer::Glow } else { eframe::Renderer::default() },
        ..Default::default()
    }
}

/// Runs Oynx in a window created with or without a transparent surface.
fn run(transparent: bool, transparency_failed: bool) -> eframe::Result {
    eframe::run_native(
        "Oynx",
        native_options(transparent),
        Box::new(move |cc| {
            let mut app = OynxApp::new();
            app.transparent_window = transparent;
            app.transparency_failed = transparency_failed;
            // Fonts registered here are available from the first frame.
            app.apply_theme(&cc.egui_ctx);
            if let Some(prefs) = cc.storage.and_then(|storage| eframe::get_value::<UiPrefs>(storage, UI_PREFS_KEY)) {
                app.prefs = prefs;
            }
            // Without a tray icon, closing the window quits as usual.
            match tray::Tray::new(&cc.egui_ctx) {
                Ok(tray) => app.tray = Some(tray),
                Err(error) => log::warn!("System tray unavailable: {error}"),
            }
            Ok(Box::new(app))
        }),
    )
}

fn main() -> eframe::Result {
    env_logger::init();
    // A see-through window needs a transparent surface from the start, and the
    // OpenGL renderer is the one that composites it on Windows. Everyone else
    // keeps the default renderer and an opaque window.
    let transparent = ThemeSettings::load(&theme_path()).wants_transparent_window();
    let result = run(transparent, false);
    if transparent && let Err(error) = &result {
        // Without a usable OpenGL driver the see-through window can't open;
        // never let a theme setting keep Oynx from starting.
        log::warn!("Could not open a see-through window ({error}); opening a normal one instead");
        return run(false, true);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_ui_prefs_keep_closing_to_tray() {
        let prefs: UiPrefs = serde_json::from_str(
            r#"{"sidebar_width":240.0,"queue_width":340.0,"player_height":180.0,"update_mode":"Manual"}"#,
        )
        .expect("prefs saved by an older version should deserialize");
        assert!(prefs.close_to_tray);
        assert_eq!(prefs.sidebar_width, 240.0);
    }

    #[test]
    fn saved_session_round_trips_resume_state() {
        let session = SavedSession {
            account: "listener".to_owned(),
            display_name: "Listener".to_owned(),
            avatar_url: None,
            volume: 0.64,
            shuffle: true,
            repeat: RepeatMode::One,
            position_ms: 42_000,
            queue_source: "Liked Songs".to_owned(),
            queue_selected: 0,
            queue: vec![SavedTrack {
                id: "track-id".to_owned(),
                title: "A song".to_owned(),
                artist: "An artist".to_owned(),
                album: "An album".to_owned(),
                duration_ms: 180_000,
                image_url: None,
                artist_id: Some("artist-id".to_owned()),
                album_id: None,
            }],
        };
        let encoded = serde_json::to_string(&session).expect("session should serialize");
        let decoded: SavedSession =
            serde_json::from_str(&encoded).expect("session should deserialize");
        assert_eq!(decoded.account, session.account);
        assert_eq!(decoded.position_ms, session.position_ms);
        assert_eq!(decoded.repeat, RepeatMode::One);
        assert_eq!(decoded.queue[0].id, "track-id");
    }
}

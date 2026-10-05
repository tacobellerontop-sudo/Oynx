//! Custom themes: light and dark mode, colour overrides, a background image,
//! a custom font, and window transparency.
//!
//! [`ThemeSettings`] is what the listener chooses and what is saved to
//! `theme.json`. [`Palette`] is the full set of colours the UI paints with,
//! derived from those settings; the active palette lives in a global so every
//! painting helper can read it with [`pal`].

use eframe::egui::{self, Color32};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{LazyLock, RwLock},
};

pub type Rgb = [u8; 3];

/// Lowest window opacity offered, so the window never disappears entirely.
pub const MIN_WINDOW_OPACITY: f32 = 0.35;
/// Strongest background-image blur offered, in pixels.
pub const MAX_IMAGE_BLUR: f32 = 40.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemeMode {
    #[default]
    Dark,
    Light,
    /// Follow the Windows app mode.
    System,
}

/// Colours the listener picked. `None` keeps the mode's built-in colour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ColorOverrides {
    pub text: Option<Rgb>,
    pub secondary_text: Option<Rgb>,
    pub background: Option<Rgb>,
    pub panels: Option<Rgb>,
    pub accent: Option<Rgb>,
    pub visualizer: Option<Rgb>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorRole {
    Text,
    SecondaryText,
    Background,
    Panels,
    Accent,
    Visualizer,
}

impl ColorRole {
    pub const ALL: [Self; 6] = [
        Self::Text,
        Self::SecondaryText,
        Self::Background,
        Self::Panels,
        Self::Accent,
        Self::Visualizer,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Text => "Text",
            Self::SecondaryText => "Secondary text",
            Self::Background => "Background",
            Self::Panels => "Panels and cards",
            Self::Accent => "Accent",
            Self::Visualizer => "Visualiser",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            Self::Text => "Titles, track names and lyrics",
            Self::SecondaryText => "Artists, hints and timestamps",
            Self::Background => "Behind everything",
            Self::Panels => "Cards, menus and buttons",
            Self::Accent => "Play buttons, the playing track and highlights",
            Self::Visualizer => "The waveform beside the record",
        }
    }
}

impl ColorOverrides {
    pub fn get(&self, role: ColorRole) -> Option<Rgb> {
        match role {
            ColorRole::Text => self.text,
            ColorRole::SecondaryText => self.secondary_text,
            ColorRole::Background => self.background,
            ColorRole::Panels => self.panels,
            ColorRole::Accent => self.accent,
            ColorRole::Visualizer => self.visualizer,
        }
    }

    pub fn set(&mut self, role: ColorRole, value: Option<Rgb>) {
        let slot = match role {
            ColorRole::Text => &mut self.text,
            ColorRole::SecondaryText => &mut self.secondary_text,
            ColorRole::Background => &mut self.background,
            ColorRole::Panels => &mut self.panels,
            ColorRole::Accent => &mut self.accent,
            ColorRole::Visualizer => &mut self.visualizer,
        };
        *slot = value;
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// A named set of colours for one mode.
pub struct Preset {
    pub name: &'static str,
    pub colors: ColorOverrides,
}

const fn preset(name: &'static str, background: Rgb, panels: Rgb, text: Rgb, secondary: Rgb, accent: Rgb) -> Preset {
    Preset {
        name,
        colors: ColorOverrides {
            text: Some(text),
            secondary_text: Some(secondary),
            background: Some(background),
            panels: Some(panels),
            accent: Some(accent),
            visualizer: Some(accent),
        },
    }
}

pub const DARK_PRESETS: &[Preset] = &[
    preset("Midnight", [11, 14, 24], [22, 27, 42], [226, 232, 245], [130, 140, 165], [110, 160, 255]),
    preset("Forest", [10, 16, 13], [20, 30, 25], [222, 236, 226], [126, 150, 135], [96, 200, 140]),
    preset("Rose", [20, 11, 15], [34, 21, 27], [245, 226, 233], [165, 130, 145], [240, 120, 160]),
    preset("Amber", [18, 14, 9], [31, 25, 17], [242, 232, 214], [160, 145, 120], [240, 170, 70]),
    preset("Violet", [15, 11, 22], [27, 21, 39], [236, 228, 248], [148, 135, 170], [170, 130, 255]),
];

pub const LIGHT_PRESETS: &[Preset] = &[
    preset("Paper", [247, 244, 237], [255, 253, 248], [40, 34, 28], [120, 110, 98], [176, 104, 48]),
    preset("Sky", [239, 244, 251], [255, 255, 255], [22, 32, 52], [100, 114, 140], [40, 110, 230]),
    preset("Mint", [238, 247, 242], [252, 255, 253], [20, 42, 32], [96, 124, 110], [20, 150, 100]),
    preset("Blush", [251, 240, 244], [255, 250, 252], [52, 24, 36], [140, 104, 118], [210, 70, 120]),
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeSettings {
    pub mode: ThemeMode,
    /// Colours used while the dark mode is active.
    pub dark: ColorOverrides,
    /// Colours used while the light mode is active.
    pub light: ColorOverrides,
    pub background_image: Option<PathBuf>,
    /// How strongly the image shows through the background colour, 0 to 1.
    pub image_strength: f32,
    /// Blur applied to the background image, in pixels.
    pub image_blur: f32,
    /// A TrueType or OpenType font file used for all text instead of Inter.
    pub font: Option<PathBuf>,
    /// Opacity of the window background, from [`MIN_WINDOW_OPACITY`] to 1.
    pub window_opacity: f32,
    /// Blur whatever is behind the window (Windows acrylic).
    pub window_blur: bool,
}

impl Default for ThemeSettings {
    fn default() -> Self {
        Self {
            mode: ThemeMode::Dark,
            dark: ColorOverrides::default(),
            light: ColorOverrides::default(),
            background_image: None,
            image_strength: 0.35,
            image_blur: 0.0,
            font: None,
            window_opacity: 1.0,
            window_blur: false,
        }
    }
}

impl ThemeSettings {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<Self>(&text).ok())
            .map(Self::sanitized)
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|error| error.to_string())?;
        std::fs::write(path, text).map_err(|error| format!("Could not save {}: {error}", path.display()))
    }

    /// Clamps values a hand-edited file could put out of range.
    fn sanitized(mut self) -> Self {
        let clamp = |value: f32, min: f32, max: f32, fallback: f32| {
            if value.is_finite() { value.clamp(min, max) } else { fallback }
        };
        self.image_strength = clamp(self.image_strength, 0.0, 1.0, 0.35);
        self.image_blur = clamp(self.image_blur, 0.0, MAX_IMAGE_BLUR, 0.0);
        self.window_opacity = clamp(self.window_opacity, MIN_WINDOW_OPACITY, 1.0, 1.0);
        self
    }

    pub fn is_dark(&self, system: Option<egui::Theme>) -> bool {
        match self.mode {
            ThemeMode::Dark => true,
            ThemeMode::Light => false,
            ThemeMode::System => system != Some(egui::Theme::Light),
        }
    }

    pub fn colors(&self, dark: bool) -> &ColorOverrides {
        if dark { &self.dark } else { &self.light }
    }

    pub fn colors_mut(&mut self, dark: bool) -> &mut ColorOverrides {
        if dark { &mut self.dark } else { &mut self.light }
    }

    /// Whether the window needs a transparent surface to show these settings.
    pub fn wants_transparent_window(&self) -> bool {
        self.window_opacity < 1.0 || self.window_blur
    }

    pub fn palette(&self, dark: bool) -> Palette {
        Palette::derive(&Base::for_mode(dark).with(self.colors(dark)), dark)
    }
}

/// The colours a listener can set; everything else in [`Palette`] follows from them.
#[derive(Clone, Copy)]
struct Base {
    background: Color32,
    panels: Color32,
    text: Color32,
    secondary_text: Color32,
    accent: Color32,
    visualizer: Color32,
}

impl Base {
    fn for_mode(dark: bool) -> Self {
        if dark {
            let text = Color32::from_rgb(236, 236, 236);
            Self {
                background: Color32::from_rgb(10, 10, 11),
                panels: Color32::from_rgb(25, 25, 27),
                text,
                secondary_text: Color32::from_rgb(140, 140, 144),
                accent: text,
                visualizer: text,
            }
        } else {
            let text = Color32::from_rgb(22, 22, 24);
            Self {
                background: Color32::from_rgb(244, 244, 246),
                panels: Color32::from_rgb(255, 255, 255),
                text,
                secondary_text: Color32::from_rgb(104, 104, 110),
                accent: text,
                visualizer: text,
            }
        }
    }

    fn with(mut self, overrides: &ColorOverrides) -> Self {
        let rgb = |value: Option<Rgb>, fallback: Color32| value.map_or(fallback, |[r, g, b]| Color32::from_rgb(r, g, b));
        // An unset accent or visualiser follows a custom text colour, as the
        // built-in monochrome look does.
        let text = rgb(overrides.text, self.text);
        let accent_default = if self.accent == self.text { text } else { self.accent };
        let visualizer_default = if self.visualizer == self.text { text } else { self.visualizer };
        self.background = rgb(overrides.background, self.background);
        self.panels = rgb(overrides.panels, self.panels);
        self.secondary_text = rgb(overrides.secondary_text, self.secondary_text);
        self.accent = rgb(overrides.accent, accent_default);
        self.visualizer = rgb(overrides.visualizer, visualizer_default);
        self.text = text;
        self
    }
}

/// Every colour the interface paints with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub background: Color32,
    pub surface: Color32,
    pub surface_raised: Color32,
    pub surface_hover: Color32,
    pub border: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub subtle: Color32,
    pub accent: Color32,
    pub accent_hover: Color32,
    pub accent_dark: Color32,
    /// Icons and labels drawn on top of the accent colour.
    pub on_accent: Color32,
    pub visualizer: Color32,
    pub placeholder_art: Color32,
    pub liked_tile: Color32,
    pub warning: Color32,
    pub danger: Color32,
    pub dark: bool,
}

impl Palette {
    fn derive(base: &Base, dark: bool) -> Self {
        let towards_edge = if dark { Color32::WHITE } else { Color32::BLACK };
        // Whichever of background and text is darker reads best on a light
        // accent, and the lighter one on a dark accent.
        let (darker, lighter) = if luminance(base.background) < luminance(base.text) {
            (base.background, base.text)
        } else {
            (base.text, base.background)
        };
        Self {
            background: base.background,
            surface: mix(base.background, base.panels, 0.4),
            surface_raised: base.panels,
            surface_hover: mix(base.panels, base.text, 0.05),
            border: mix(base.background, base.text, 0.1),
            text: base.text,
            muted: base.secondary_text,
            subtle: mix(base.background, base.secondary_text, 0.63),
            accent: base.accent,
            accent_hover: mix(base.accent, towards_edge, 0.35),
            accent_dark: mix(base.background, base.accent, 0.13),
            on_accent: if luminance(base.accent) > 0.45 { darker } else { lighter },
            visualizer: base.visualizer,
            placeholder_art: mix(base.background, base.text, 0.13),
            liked_tile: mix(base.background, base.text, 0.124),
            warning: if dark { Color32::from_rgb(214, 186, 140) } else { Color32::from_rgb(150, 100, 30) },
            danger: if dark { Color32::from_rgb(224, 132, 132) } else { Color32::from_rgb(190, 50, 50) },
            dark,
        }
    }
}

fn mix(from: Color32, to: Color32, t: f32) -> Color32 {
    from.lerp_to_gamma(to, t)
}

/// Relative luminance, 0 (black) to 1 (white).
pub fn luminance(color: Color32) -> f32 {
    let channel = |value: u8| {
        let value = value as f32 / 255.0;
        if value <= 0.04045 { value / 12.92 } else { ((value + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
}

static PALETTE: LazyLock<RwLock<Palette>> = LazyLock::new(|| RwLock::new(ThemeSettings::default().palette(true)));

/// The active palette.
pub fn pal() -> Palette {
    *PALETTE.read().unwrap_or_else(|error| error.into_inner())
}

pub fn set_palette(palette: Palette) {
    *PALETTE.write().unwrap_or_else(|error| error.into_inner()) = palette;
}

/// Reads a font file and checks that it can draw text, so a bad file can be
/// refused instead of crashing egui when the fonts are rebuilt.
pub fn load_font(path: &Path) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("Could not read {}: {error}", path.display()))?;
    let font = skrifa::FontRef::from_index(&bytes, 0)
        .map_err(|_| format!("{} is not a TrueType or OpenType font.", file_name(path)))?;
    use skrifa::MetadataProvider;
    if font.charmap().map('a').is_none() {
        return Err(format!("{} has no Latin letters, so it can't be used for the interface.", file_name(path)));
    }
    Ok(bytes)
}

/// Decodes, downsizes and optionally blurs a background image.
pub fn load_background_image(path: &Path, blur: f32) -> Result<egui::ColorImage, String> {
    const MAX_SIDE: u32 = 2560;
    let image = image::open(path).map_err(|error| format!("Could not open {}: {error}", file_name(path)))?;
    let mut image = if image.width() > MAX_SIDE || image.height() > MAX_SIDE {
        image.resize(MAX_SIDE, MAX_SIDE, image::imageops::FilterType::Triangle)
    } else {
        image
    };
    if blur > 0.5 {
        // Blurring a smaller copy is much faster and looks the same once scaled up.
        let scale = 4;
        let small = image.resize(
            (image.width() / scale).max(1),
            (image.height() / scale).max(1),
            image::imageops::FilterType::Triangle,
        );
        image = image::DynamicImage::ImageRgba8(image::imageops::fast_blur(&small.to_rgba8(), blur / scale as f32));
    }
    let rgba = image.to_rgba8();
    Ok(egui::ColorImage::from_rgba_unmultiplied(
        [rgba.width() as usize, rgba.height() as usize],
        rgba.as_raw(),
    ))
}

pub fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Fonts that ship with Windows, offered by name so nobody has to hunt for files.
pub const SYSTEM_FONTS: &[(&str, &str)] = &[
    ("Segoe UI", "segoeui.ttf"),
    ("Segoe UI Variable", "SegUIVar.ttf"),
    ("Bahnschrift", "bahnschrift.ttf"),
    ("Arial", "arial.ttf"),
    ("Calibri", "calibri.ttf"),
    ("Candara", "Candara.ttf"),
    ("Cascadia Code", "CascadiaCode.ttf"),
    ("Consolas", "consola.ttf"),
    ("Corbel", "corbel.ttf"),
    ("Georgia", "georgia.ttf"),
    ("Tahoma", "tahoma.ttf"),
    ("Trebuchet MS", "trebuc.ttf"),
    ("Verdana", "verdana.ttf"),
];

/// The installed fonts from [`SYSTEM_FONTS`], with their full paths.
pub fn installed_system_fonts() -> Vec<(&'static str, PathBuf)> {
    let windows = std::env::var_os("WINDIR").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
    let fonts = windows.join("Fonts");
    SYSTEM_FONTS
        .iter()
        .map(|(name, file)| (*name, fonts.join(file)))
        .filter(|(_, path)| path.is_file())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_dark_palette_matches_the_original_colours() {
        let palette = ThemeSettings::default().palette(true);
        assert_eq!(palette.background, Color32::from_rgb(10, 10, 11));
        assert_eq!(palette.surface_raised, Color32::from_rgb(25, 25, 27));
        assert_eq!(palette.text, Color32::from_rgb(236, 236, 236));
        assert_eq!(palette.muted, Color32::from_rgb(140, 140, 144));
        assert_eq!(palette.accent, palette.text);
        assert_eq!(palette.on_accent, palette.background);
    }

    #[test]
    fn overrides_only_apply_to_their_mode() {
        let mut settings = ThemeSettings::default();
        settings.dark.accent = Some([255, 0, 0]);
        assert_eq!(settings.palette(true).accent, Color32::from_rgb(255, 0, 0));
        assert_ne!(settings.palette(false).accent, Color32::from_rgb(255, 0, 0));
    }

    #[test]
    fn dark_accent_gets_light_icons() {
        let mut settings = ThemeSettings::default();
        settings.dark.accent = Some([20, 40, 120]);
        let palette = settings.palette(true);
        assert_eq!(palette.on_accent, palette.text);
    }

    #[test]
    fn custom_text_colour_carries_to_unset_accent() {
        let mut settings = ThemeSettings::default();
        settings.light.text = Some([10, 60, 30]);
        let palette = settings.palette(false);
        assert_eq!(palette.accent, Color32::from_rgb(10, 60, 30));
        assert_eq!(palette.visualizer, Color32::from_rgb(10, 60, 30));
    }

    #[test]
    fn out_of_range_values_are_clamped_on_load() {
        let settings: ThemeSettings =
            serde_json::from_str(r#"{"window_opacity":0.0,"image_blur":500.0,"mode":"Light"}"#).unwrap();
        let settings = settings.sanitized();
        assert_eq!(settings.window_opacity, MIN_WINDOW_OPACITY);
        assert_eq!(settings.image_blur, MAX_IMAGE_BLUR);
        assert_eq!(settings.mode, ThemeMode::Light);
        assert!(!settings.is_dark(None));
    }

    #[test]
    fn system_mode_follows_windows() {
        let settings = ThemeSettings { mode: ThemeMode::System, ..Default::default() };
        assert!(settings.is_dark(Some(egui::Theme::Dark)));
        assert!(!settings.is_dark(Some(egui::Theme::Light)));
        assert!(settings.is_dark(None));
    }
}

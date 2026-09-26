//! System tray icon that keeps Oynx reachable while its window is hidden.

use eframe::egui;
use std::sync::mpsc::{self, Receiver};
use tray_icon::{
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};

/// Windows rejects tooltips longer than 128 UTF-16 units.
const MAX_TOOLTIP_CHARS: usize = 120;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    Show,
    PlayPause,
    Next,
    Previous,
    Quit,
}

pub struct Tray {
    icon: TrayIcon,
    play_pause: MenuItem,
    actions: Receiver<TrayAction>,
    tooltip: String,
    playing: Option<bool>,
}

impl Tray {
    /// Creates the tray icon. Must be called on the thread that runs the event loop.
    ///
    /// Tray and menu events can arrive while the window is hidden, so each one
    /// wakes egui with `request_repaint`, which makes eframe call `App::logic`.
    pub fn new(ctx: &egui::Context) -> Result<Self, String> {
        let show = MenuItem::new("Show Oynx", true, None);
        let play_pause = MenuItem::new("Play", true, None);
        let next = MenuItem::new("Next", true, None);
        let previous = MenuItem::new("Previous", true, None);
        let quit = MenuItem::new("Quit Oynx", true, None);
        let menu = Menu::with_items(&[
            &show,
            &PredefinedMenuItem::separator(),
            &play_pause,
            &next,
            &previous,
            &PredefinedMenuItem::separator(),
            &quit,
        ])
        .map_err(|error| error.to_string())?;

        let (sender, actions) = mpsc::channel();
        let menu_actions = [
            (show.id().clone(), TrayAction::Show),
            (play_pause.id().clone(), TrayAction::PlayPause),
            (next.id().clone(), TrayAction::Next),
            (previous.id().clone(), TrayAction::Previous),
            (quit.id().clone(), TrayAction::Quit),
        ];
        {
            let sender = sender.clone();
            let ctx = ctx.clone();
            MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                if let Some((_, action)) = menu_actions.iter().find(|(id, _)| *id == event.id) {
                    let _ = sender.send(*action);
                    ctx.request_repaint();
                }
            }));
        }
        {
            let ctx = ctx.clone();
            TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
                let open = matches!(
                    event,
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } | TrayIconEvent::DoubleClick {
                        button: MouseButton::Left,
                        ..
                    }
                );
                if open {
                    let _ = sender.send(TrayAction::Show);
                    ctx.request_repaint();
                }
            }));
        }

        let size = 32;
        let icon = Icon::from_rgba(app_icon_rgba(size), size, size)
            .map_err(|error| error.to_string())?;
        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_tooltip("Oynx")
            .with_icon(icon)
            .build()
            .map_err(|error| error.to_string())?;

        Ok(Self {
            icon,
            play_pause,
            actions,
            tooltip: "Oynx".to_owned(),
            playing: None,
        })
    }

    pub fn next_action(&self) -> Option<TrayAction> {
        self.actions.try_recv().ok()
    }

    /// Keeps the tooltip and the Play/Pause entry in sync with playback.
    pub fn update(&mut self, tooltip: &str, playing: bool) {
        let tooltip = tooltip.chars().take(MAX_TOOLTIP_CHARS).collect::<String>();
        if tooltip != self.tooltip {
            let _ = self.icon.set_tooltip(Some(&tooltip));
            self.tooltip = tooltip;
        }
        if self.playing != Some(playing) {
            self.play_pause.set_text(if playing { "Pause" } else { "Play" });
            self.playing = Some(playing);
        }
    }
}

/// The Oynx mark (a green disc with a dark ring) as `size`×`size` RGBA pixels.
pub fn app_icon_rgba(size: u32) -> Vec<u8> {
    let center = size as f32 / 2.0;
    let outer = center - 0.5;
    let ring_radius = outer * 0.4;
    let ring_half_width = outer * 0.11;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 + 0.5 - center;
            let dy = y as f32 + 0.5 - center;
            let distance = (dx * dx + dy * dy).sqrt();
            // Anti-aliased coverage of the disc and of the ring cut into it.
            let disc = (outer - distance + 0.5).clamp(0.0, 1.0);
            let ring = (ring_half_width - (distance - ring_radius).abs() + 0.5).clamp(0.0, 1.0);
            let mix = |accent: f32, dark: f32| (accent * (1.0 - ring) + dark * ring).round() as u8;
            rgba.extend_from_slice(&[
                mix(30.0, 9.0),
                mix(215.0, 10.0),
                mix(96.0, 12.0),
                (disc * 255.0).round() as u8,
            ]);
        }
    }
    rgba
}

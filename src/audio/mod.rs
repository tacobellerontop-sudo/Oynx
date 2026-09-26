//! Audio processing between librespot's decoder and the speakers:
//! equalizer → spectrum analyser → rodio output.

pub mod equalizer;
pub mod visualizer;

use std::sync::{Arc, Mutex};

pub use equalizer::{
    EQ_FREQUENCIES_HZ, EqualizerPreset, EqualizerSettings, MAX_EQ_GAIN_DB, MIN_EQ_GAIN_DB,
    NUM_EQ_BANDS,
};
pub use visualizer::{NUM_BANDS, VisBands};

use librespot::playback::audio_backend::Sink;

/// Handles shared between the audio thread and the UI.
#[derive(Clone)]
pub struct AudioTaps {
    pub bands: Arc<Mutex<VisBands>>,
    pub equalizer: Arc<Mutex<EqualizerSettings>>,
}

impl AudioTaps {
    pub fn new(settings: EqualizerSettings) -> Self {
        Self {
            bands: VisBands::shared(),
            equalizer: Arc::new(Mutex::new(settings.normalized())),
        }
    }

    /// Wraps the real output sink: the equalizer runs first, so the analyser
    /// describes what the listener actually hears.
    pub fn wrap_sink(&self, output: Box<dyn Sink>, sample_rate: f32) -> Box<dyn Sink> {
        let analyser = Box::new(visualizer::VisualizationSink::new(
            output,
            Arc::clone(&self.bands),
            sample_rate,
        ));
        Box::new(equalizer::EqualizerSink::new(
            analyser,
            Arc::clone(&self.equalizer),
        ))
    }
}

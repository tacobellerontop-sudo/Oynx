//! Real-time FFT frequency-band visualizer.
//!
//! Vendored from MYX (https://github.com/HaseebKhalid1507/Myx, `src/audio/visualizer.rs`,
//! MIT, (c) 2026 Haseeb Khalid), which adapted it from aome510/spotify-player
//! (`ui/streaming.rs`, MIT, (c) 2021 Thang Pham).
//!
//! The design is a **tee'd audio sink**: it forwards every packet it receives
//! unchanged to the real backend while computing a windowed FFT on a copy. The
//! equalizer sits immediately before this sink, so these bands describe the
//! sound after EQ.
//!
//! Oynx change: the rodio backend queues roughly half a second of audio ahead
//! of the speakers, so analysing packets as they are written would run ahead
//! of what the listener hears. Each analysed frame is therefore stamped with
//! the moment its audio will actually play, and the UI only shows frames whose
//! time has come. The hot path stays allocation-free after warm-up, and the UI
//! reads through `try_lock`, so the audio thread never waits on a render.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use librespot::playback::audio_backend::{Sink, SinkResult};
use librespot::playback::convert::Converter;
use librespot::playback::decoder::AudioPacket;
use rustfft::{FftPlanner, num_complex::Complex};

const FFT_SIZE: usize = 1024;
/// New samples consumed per FFT frame (overlap = FFT_SIZE - HOP_SIZE).
const HOP_SIZE: usize = 128;
pub const NUM_BANDS: usize = 128;

/// Per-frame decay for individual bands — snappy but not jittery.
const DECAY_FACTOR: f32 = 0.985;
/// Slower decay for the normalization envelope so quiet passages read quiet.
const DECAY_FACTOR_PEAK: f32 = 0.9985;
/// Upper bound on frames waiting for their play time (~1.5 s at 44.1 kHz).
const MAX_PENDING_FRAMES: usize = 512;

/// One analysed slice of audio, released to the UI at `due`.
#[derive(Clone, Copy)]
pub struct VisFrame {
    pub due: Instant,
    pub values: [f32; NUM_BANDS],
    pub peak_envelope: f32,
}

/// Shared frequency-band state written by the audio sink, read by the renderer.
pub struct VisBands {
    frames: VecDeque<VisFrame>,
    pub is_active: bool,
}

impl VisBands {
    pub fn shared() -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self {
            frames: VecDeque::with_capacity(MAX_PENDING_FRAMES),
            is_active: false,
        }))
    }

    /// The newest frame whose audio is playing by `now`; older ones are dropped.
    pub fn take_due(&mut self, now: Instant) -> Option<VisFrame> {
        let mut latest = None;
        while self.frames.front().is_some_and(|frame| frame.due <= now) {
            latest = self.frames.pop_front();
        }
        latest
    }

    fn push(&mut self, frame: VisFrame) {
        if self.frames.len() == MAX_PENDING_FRAMES {
            self.frames.pop_front();
        }
        self.frames.push_back(frame);
    }
}

/// A tee'd sink: forwards audio to `inner` and computes FFT bands on the side.
pub struct VisualizationSink {
    inner: Box<dyn Sink>,
    sample_buf: VecDeque<f32>,
    bands: Arc<Mutex<VisBands>>,
    fft: Arc<dyn rustfft::Fft<f32>>,
    hann_window: Vec<f32>,
    fft_buf: Vec<Complex<f32>>,
    magnitudes: Vec<f32>,
    sample_rate: f32,
    band_ranges: Vec<(usize, usize)>,
    new_bands: [f32; NUM_BANDS],
    smooth_scratch: [f32; NUM_BANDS],
    /// Decayed band values and normalisation envelope (sink-local state).
    values: [f32; NUM_BANDS],
    peak_envelope: f32,
    /// When the first sample of the current stream reaches the speakers.
    stream_start: Option<Instant>,
    /// Frames (stereo sample pairs) written since the stream started.
    frames_written: u64,
    /// Stream frame index of the first sample in `sample_buf`.
    buffer_start_frame: u64,
}

impl VisualizationSink {
    pub fn new(inner: Box<dyn Sink>, bands: Arc<Mutex<VisBands>>, sample_rate: f32) -> Self {
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        let hann_window: Vec<f32> = (0..FFT_SIZE)
            .map(|i| {
                0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / (FFT_SIZE - 1) as f32).cos())
            })
            .collect();
        let band_ranges = precompute_band_ranges(FFT_SIZE / 2, NUM_BANDS);
        Self {
            inner,
            sample_buf: VecDeque::with_capacity(FFT_SIZE * 2),
            bands,
            fft,
            hann_window,
            fft_buf: vec![Complex::new(0.0, 0.0); FFT_SIZE],
            magnitudes: vec![0.0; FFT_SIZE / 2],
            sample_rate,
            band_ranges,
            new_bands: [0.0; NUM_BANDS],
            smooth_scratch: [0.0; NUM_BANDS],
            values: [0.0; NUM_BANDS],
            peak_envelope: 1e-6,
            stream_start: None,
            frames_written: 0,
            buffer_start_frame: 0,
        }
    }

    fn reset(&mut self) {
        self.sample_buf.clear();
        self.values = [0.0; NUM_BANDS];
        self.peak_envelope = 1e-6;
        self.stream_start = None;
        self.frames_written = 0;
        self.buffer_start_frame = 0;
    }

    fn frame_time(&self, frame: u64) -> Duration {
        Duration::from_secs_f64(frame as f64 / f64::from(self.sample_rate))
    }
}

impl Sink for VisualizationSink {
    fn start(&mut self) -> SinkResult<()> {
        self.reset();
        if let Ok(mut bands) = self.bands.lock() {
            bands.frames.clear();
            bands.is_active = true;
        }
        self.inner.start()
    }

    fn stop(&mut self) -> SinkResult<()> {
        // The rodio backend drains its queue while stopping, so once this
        // returns the speakers are silent.
        let result = self.inner.stop();
        if let Ok(mut bands) = self.bands.lock() {
            bands.frames.clear();
            bands.is_active = false;
        }
        self.reset();
        result
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        if let AudioPacket::Samples(ref samples) = packet {
            // Map stream time to wall-clock play time. If audio arrives late
            // (e.g. a network stall emptied the queue), this packet plays now.
            let now = Instant::now();
            let packet_time = self.frame_time(self.frames_written);
            let start = self.stream_start.get_or_insert(now);
            if *start + packet_time < now {
                *start = now - packet_time;
            }
            let stream_start = *start;

            // Interleaved stereo -> mono.
            let before = self.sample_buf.len();
            self.sample_buf.extend(samples.chunks(2).map(|c| {
                if c.len() == 2 {
                    f64::midpoint(c[0], c[1]) as f32
                } else {
                    c[0] as f32
                }
            }));
            self.frames_written += (self.sample_buf.len() - before) as u64;

            while self.sample_buf.len() >= FFT_SIZE {
                {
                    let (front, back) = self.sample_buf.as_slices();
                    if front.len() >= FFT_SIZE {
                        for (dst, (&s, &w)) in self
                            .fft_buf
                            .iter_mut()
                            .zip(front.iter().zip(self.hann_window.iter()))
                        {
                            *dst = Complex::new(s * w, 0.0);
                        }
                    } else {
                        let split = front.len();
                        for (dst, (&s, &w)) in self.fft_buf[..split]
                            .iter_mut()
                            .zip(front.iter().zip(self.hann_window[..split].iter()))
                        {
                            *dst = Complex::new(s * w, 0.0);
                        }
                        let remaining = FFT_SIZE - split;
                        for (dst, (&s, &w)) in self.fft_buf[split..].iter_mut().zip(
                            back[..remaining]
                                .iter()
                                .zip(self.hann_window[split..].iter()),
                        ) {
                            *dst = Complex::new(s * w, 0.0);
                        }
                    }
                }

                self.fft.process(&mut self.fft_buf);

                for (mag, c) in self.magnitudes.iter_mut().zip(self.fft_buf.iter()) {
                    *mag = c.norm();
                }

                fill_log_bands(&self.magnitudes, &self.band_ranges, &mut self.new_bands);
                smooth_bands(&mut self.new_bands, &mut self.smooth_scratch);

                let frame_peak = self.new_bands.iter().copied().fold(0.0_f32, f32::max);
                for (stored, fresh) in self.values.iter_mut().zip(self.new_bands.iter()) {
                    *stored = (*stored * DECAY_FACTOR).max(*fresh);
                }
                self.peak_envelope = (self.peak_envelope * DECAY_FACTOR_PEAK).max(frame_peak);

                // The frame describes the centre of the analysis window.
                let centre = self.buffer_start_frame + (FFT_SIZE / 2) as u64;
                let frame = VisFrame {
                    due: stream_start + self.frame_time(centre),
                    values: self.values,
                    peak_envelope: self.peak_envelope,
                };
                if let Ok(mut bands) = self.bands.lock() {
                    bands.push(frame);
                }

                self.sample_buf.drain(..HOP_SIZE);
                self.buffer_start_frame += HOP_SIZE as u64;
            }
        }

        self.inner.write(packet, converter)
    }
}

fn precompute_band_ranges(num_bins: usize, num_bands: usize) -> Vec<(usize, usize)> {
    let log_min = 1.0_f64;
    let log_max = num_bins as f64;
    let mut used_up_to: usize = 1;
    let mut ranges = Vec::with_capacity(num_bands);
    for band in 0..num_bands {
        if used_up_to >= num_bins {
            ranges.push((num_bins - 1, num_bins));
            continue;
        }
        let t_start = band as f64 / num_bands as f64;
        let t_end = (band + 1) as f64 / num_bands as f64;
        let natural_start = (log_min * (log_max / log_min).powf(t_start)) as usize;
        let natural_end = (log_min * (log_max / log_min).powf(t_end)) as usize;
        let start = natural_start.max(used_up_to).min(num_bins - 1);
        let end = natural_end.max(start + 1).min(num_bins);
        used_up_to = end;
        ranges.push((start, end));
    }
    ranges
}

fn fill_log_bands(magnitudes: &[f32], band_ranges: &[(usize, usize)], out: &mut [f32]) {
    for (band_val, &(start, end)) in out.iter_mut().zip(band_ranges.iter()) {
        let len = (end - start) as f32;
        let sum_sq: f32 = magnitudes[start..end].iter().map(|&v| v * v).sum();
        *band_val = (sum_sq / len).sqrt();
    }
}

fn smooth_bands(bands: &mut [f32], scratch: &mut [f32]) {
    let n = bands.len();
    if n < 3 {
        return;
    }
    scratch[..n].copy_from_slice(&bands[..n]);
    for i in 0..n {
        let prev = scratch[if i > 0 { i - 1 } else { 0 }];
        let next = scratch[if i + 1 < n { i + 1 } else { n - 1 }];
        bands[i] = prev * 0.25 + scratch[i] * 0.5 + next * 0.25;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_released_in_order_once_due() {
        let shared = VisBands::shared();
        let now = Instant::now();
        let mut bands = shared.lock().unwrap();
        for offset_ms in [0_u64, 10, 500] {
            bands.push(VisFrame {
                due: now + Duration::from_millis(offset_ms),
                values: [offset_ms as f32; NUM_BANDS],
                peak_envelope: 1.0,
            });
        }
        let released = bands.take_due(now + Duration::from_millis(20)).unwrap();
        assert_eq!(released.values[0], 10.0);
        assert!(bands.take_due(now + Duration::from_millis(20)).is_none());
        assert_eq!(bands.take_due(now + Duration::from_secs(1)).unwrap().values[0], 500.0);
    }

    #[test]
    fn pending_frames_are_bounded() {
        let shared = VisBands::shared();
        let mut bands = shared.lock().unwrap();
        let later = Instant::now() + Duration::from_secs(60);
        for _ in 0..MAX_PENDING_FRAMES * 2 {
            bands.push(VisFrame {
                due: later,
                values: [0.0; NUM_BANDS],
                peak_envelope: 1.0,
            });
        }
        assert_eq!(bands.frames.len(), MAX_PENDING_FRAMES);
    }
}

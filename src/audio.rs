//! Built-in audio preview player.
//!
//! The player is intentionally lightweight: it decodes local files with
//! `rodio`/`symphonia`, keeps a small queue for album playback and extracts a
//! waveform in a background thread so the UI never blocks on decoding.
//! When no output device is available every method degrades to a no-op and the
//! caller gets a readable error instead of a panic.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use rodio::source::SeekError;
use rodio::{OutputStream, OutputStreamHandle, Sink, Source};

use crate::models::RepeatMode;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::{FormatOptions, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// File extensions the built-in player tries to open. Symphonia is built with
/// all codecs/formats, but a short allow-list keeps folder scans predictable.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "flac", "mp3", "m4a", "mp4", "aac", "ogg", "oga", "opus", "wav", "wave", "aiff", "aif", "alac",
    "ape", "wv", "tta", "dsf", "dff",
];

pub fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            let extension = extension.to_ascii_lowercase();
            AUDIO_EXTENSIONS.iter().any(|known| *known == extension)
        })
        .unwrap_or(false)
}

/// Collect every playable file below `folder`, sorted by path so albums keep
/// their natural track order.
pub fn collect_audio_files(folder: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_into(folder, &mut files, 0);
    files.sort_by(|a, b| natural_key(a).cmp(&natural_key(b)).then_with(|| a.cmp(b)));
    files
}

fn collect_into(folder: &Path, files: &mut Vec<PathBuf>, depth: usize) {
    if depth > 8 || files.len() >= 5000 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_into(&path, files, depth + 1);
        } else if is_audio_file(&path) {
            files.push(path);
        }
    }
}

/// Sort key that compares numeric track prefixes naturally ("2" < "10").
fn natural_key(path: &Path) -> (u32, String) {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_lowercase();
    let digits: String = name
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    (digits.parse().unwrap_or(u32::MAX), name)
}

#[derive(Clone, Debug, Default)]
pub struct Waveform {
    /// Peak amplitude per bucket, normalised to 0..=1.
    pub peaks: Vec<f32>,
    pub duration: f32,
}

#[derive(Clone, Debug)]
pub struct Track {
    pub path: PathBuf,
    pub title: String,
    pub duration: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackState {
    Stopped,
    Playing,
    Paused,
}

struct AnalyzeResult {
    path: PathBuf,
    waveform: Option<Waveform>,
}

/// A `rodio::Source` backed directly by Symphonia with a real `File`.
///
/// rodio's own `Decoder` wraps the file in a `ReadSeekSource` whose
/// `byte_len()` is hard-coded to `None`. Symphonia's FLAC demuxer needs the
/// byte length to binarily search for a frame, so seeking failed with
/// `SeekError::Unseekable` for FLAC/OGG. Reading through a `File`-backed
/// `MediaSourceStream` keeps the byte length intact, and implementing
/// `try_seek` on top of `FormatReader::seek` makes timeline seeking work for
/// every container Symphonia can demux.
struct SymphoniaSource {
    format: Box<dyn symphonia::core::formats::FormatReader>,
    decoder: Box<dyn symphonia::core::codecs::Decoder>,
    track_id: u32,
    sample_rate: u32,
    channels: u16,
    total_duration: Option<Duration>,
    buffer: VecDeque<f32>,
    finished: bool,
}

impl SymphoniaSource {
    fn new(path: &Path) -> Option<Self> {
        let file = File::open(path).ok()?;
        let stream = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(extension) = path.extension().and_then(|extension| extension.to_str()) {
            hint.with_extension(extension);
        }
        let probed = symphonia::default::get_probe()
            .format(
                &hint,
                stream,
                &FormatOptions {
                    enable_gapless: true,
                    ..Default::default()
                },
                &MetadataOptions::default(),
            )
            .ok()?;
        let format = probed.format;
        let track = format.default_track()?.clone();
        let decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .ok()?;
        let sample_rate = track.codec_params.sample_rate.unwrap_or(44_100).max(1);
        let channels = track
            .codec_params
            .channels
            .map(|channels| channels.count() as u16)
            .unwrap_or(2)
            .max(1);
        let total_duration = track
            .codec_params
            .n_frames
            .map(|frames| frames as f64 / sample_rate as f64)
            .map(Duration::from_secs_f64);
        Some(Self {
            format,
            decoder,
            track_id: track.id,
            sample_rate,
            channels,
            total_duration,
            buffer: VecDeque::new(),
            finished: false,
        })
    }

    /// Decode packets until at least one interleaved sample is buffered.
    fn refill(&mut self) {
        while self.buffer.is_empty() && !self.finished {
            let packet = match self.format.next_packet() {
                Ok(packet) => packet,
                Err(_) => {
                    self.finished = true;
                    return;
                }
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(decoded) => {
                    let spec = *decoded.spec();
                    self.sample_rate = spec.rate.max(1);
                    self.channels = (spec.channels.count() as u16).max(1);
                    let mut samples = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
                    samples.copy_interleaved_ref(decoded);
                    self.buffer.extend(samples.samples().iter().copied());
                }
                Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
                Err(_) => {
                    self.finished = true;
                    return;
                }
            }
        }
    }
}

impl Iterator for SymphoniaSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.buffer.is_empty() {
            self.refill();
        }
        self.buffer.pop_front()
    }
}

impl Source for SymphoniaSource {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> u16 {
        self.channels
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn total_duration(&self) -> Option<Duration> {
        self.total_duration
    }

    fn try_seek(&mut self, pos: Duration) -> Result<(), SeekError> {
        let time = symphonia::core::units::Time::new(
            pos.as_secs(),
            f64::from(pos.subsec_nanos()) / 1_000_000_000.0,
        );
        self.format
            .seek(
                SeekMode::Accurate,
                SeekTo::Time {
                    time,
                    track_id: Some(self.track_id),
                },
            )
            .map_err(|_| SeekError::NotSupported {
                underlying_source: std::any::type_name::<Self>(),
            })?;
        self.decoder.reset();
        self.buffer.clear();
        self.finished = false;
        Ok(())
    }
}

pub struct AudioPlayer {
    stream: Option<OutputStream>,
    handle: Option<OutputStreamHandle>,
    sink: Option<Sink>,
    queue: Vec<Track>,
    current: Option<usize>,
    state: PlaybackState,
    /// True when the next track was already appended to the sink.
    prefetched: bool,
    prefetched_index: Option<usize>,
    /// Folder the current queue was loaded from (used to find the album cover).
    queue_folder: Option<PathBuf>,
    shuffle: bool,
    repeat: RepeatMode,
    /// Position to restore when playback next starts.
    resume_at: f32,
    rng: u64,
    volume: f32,
    enabled: bool,
    waveform_cache: HashMap<PathBuf, Arc<Waveform>>,
    pending: HashSet<PathBuf>,
    analyze_tx: Sender<AnalyzeResult>,
    analyze_rx: Receiver<AnalyzeResult>,
    error: Option<String>,
}

impl AudioPlayer {
    pub fn new(enabled: bool, volume: f32) -> Self {
        let (analyze_tx, analyze_rx) = channel();
        Self {
            stream: None,
            handle: None,
            sink: None,
            queue: Vec::new(),
            current: None,
            state: PlaybackState::Stopped,
            prefetched: false,
            prefetched_index: None,
            queue_folder: None,
            shuffle: false,
            repeat: RepeatMode::Off,
            resume_at: 0.0,
            rng: seed_rng(),
            volume: volume.clamp(0.0, 1.5),
            enabled,
            waveform_cache: HashMap::new(),
            pending: HashSet::new(),
            analyze_tx,
            analyze_rx,
            error: None,
        }
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.stop();
        }
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.5);
        if let Some(sink) = &self.sink {
            sink.set_volume(self.volume);
        }
    }

    pub fn set_shuffle(&mut self, shuffle: bool) {
        self.shuffle = shuffle;
    }

    pub fn shuffle(&self) -> bool {
        self.shuffle
    }

    pub fn set_repeat(&mut self, repeat: RepeatMode) {
        self.repeat = repeat;
    }

    pub fn repeat(&self) -> RepeatMode {
        self.repeat
    }

    pub fn queue_folder(&self) -> Option<&Path> {
        self.queue_folder.as_deref()
    }

    pub fn current_path(&self) -> Option<&Path> {
        self.track().map(|track| track.path.as_path())
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn state(&self) -> PlaybackState {
        self.state
    }

    pub fn is_playing(&self) -> bool {
        self.state == PlaybackState::Playing
    }

    pub fn has_track(&self) -> bool {
        self.current.is_some()
    }

    pub fn track(&self) -> Option<&Track> {
        self.current.and_then(|index| self.queue.get(index))
    }

    pub fn track_title(&self) -> Option<&str> {
        self.track().map(|track| track.title.as_str())
    }

    pub fn queue_len(&self) -> usize {
        self.queue.len()
    }

    pub fn queue_index(&self) -> Option<usize> {
        self.current
    }

    pub fn duration(&self) -> f32 {
        self.track()
            .map(|track| track.duration)
            .filter(|duration| *duration > 0.0)
            .or_else(|| {
                self.track()
                    .and_then(|track| self.waveform_cache.get(&track.path))
                    .map(|waveform| waveform.duration)
            })
            .unwrap_or(0.0)
    }

    pub fn position(&self) -> f32 {
        // rodio reports the position of the *current* appended source, so it is
        // already relative to the track that is playing. Do not subtract a
        // cumulative offset here: after a gapless transition the new source
        // restarts at zero.
        self.sink
            .as_ref()
            .map(|sink| sink.get_pos().as_secs_f32())
            .unwrap_or(self.resume_at)
    }

    pub fn progress(&self) -> f32 {
        let duration = self.duration();
        if duration > 0.0 {
            (self.position() / duration).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    pub fn waveform(&self) -> Option<Arc<Waveform>> {
        self.track()
            .and_then(|track| self.waveform_cache.get(&track.path))
            .cloned()
    }

    /// Replace the queue with every audio file in `folder` and start playing.
    pub fn play_folder(&mut self, folder: &Path) -> Result<usize, String> {
        if !self.enabled {
            return Err("Built-in player is disabled in Settings.".to_string());
        }
        let files = collect_audio_files(folder);
        if files.is_empty() {
            return Err("No playable audio files in this folder.".to_string());
        }
        self.queue = files
            .into_iter()
            .map(|path| Track {
                title: track_title(&path),
                duration: probe_duration(&path).unwrap_or_default(),
                path,
            })
            .collect();
        let count = self.queue.len();
        self.queue_folder = Some(folder.to_path_buf());
        self.current = Some(0);
        self.resume_at = 0.0;
        self.start_current()?;
        Ok(count)
    }

    /// Load a folder without starting playback, restoring a previous track.
    pub fn prepare_folder(
        &mut self,
        folder: &Path,
        file: &Path,
        position: f32,
    ) -> Result<(), String> {
        let files = collect_audio_files(folder);
        if files.is_empty() {
            return Err("No playable audio files in this folder.".to_string());
        }
        self.queue = files
            .into_iter()
            .map(|path| Track {
                title: track_title(&path),
                duration: probe_duration(&path).unwrap_or_default(),
                path,
            })
            .collect();
        self.current = Some(
            self.queue
                .iter()
                .position(|track| track.path == file)
                .unwrap_or(0),
        );
        self.queue_folder = Some(folder.to_path_buf());
        self.resume_at = position.max(0.0);
        self.prefetched = false;
        self.prefetched_index = None;
        if let Some(sink) = self.sink.take() {
            sink.stop();
        }
        self.state = PlaybackState::Stopped;
        Ok(())
    }

    /// Play a single file, replacing the queue.
    pub fn play_file(&mut self, path: &Path) -> Result<(), String> {
        if !self.enabled {
            return Err("Built-in player is disabled in Settings.".to_string());
        }
        self.queue = vec![Track {
            title: track_title(path),
            duration: probe_duration(path).unwrap_or_default(),
            path: path.to_path_buf(),
        }];
        self.queue_folder = path.parent().map(Path::to_path_buf);
        self.current = Some(0);
        self.resume_at = 0.0;
        self.start_current()
    }

    pub fn toggle(&mut self) {
        match self.state {
            PlaybackState::Playing => self.pause(),
            PlaybackState::Paused => self.resume(),
            PlaybackState::Stopped => {
                if self.current.is_some() {
                    let _ = self.start_current();
                } else if let Some(index) = self.queue.first().map(|_| 0) {
                    self.current = Some(index);
                    let _ = self.start_current();
                }
            }
        }
    }

    pub fn pause(&mut self) {
        if let Some(sink) = &self.sink {
            sink.pause();
            self.state = PlaybackState::Paused;
        }
    }

    pub fn resume(&mut self) {
        if let Some(sink) = &self.sink {
            sink.play();
            self.state = PlaybackState::Playing;
        }
    }

    pub fn stop(&mut self) {
        if let Some(sink) = self.sink.take() {
            sink.stop();
        }
        self.state = PlaybackState::Stopped;
        self.current = None;
        self.queue.clear();
        self.prefetched = false;
        self.prefetched_index = None;
        self.resume_at = 0.0;
    }

    fn random_index(&mut self, avoid: usize) -> Option<usize> {
        if self.queue.is_empty() {
            return None;
        }
        if self.queue.len() == 1 {
            return Some(0);
        }
        let mut index;
        for _ in 0..16 {
            self.rng ^= self.rng << 13;
            self.rng ^= self.rng >> 7;
            self.rng ^= self.rng << 17;
            index = (self.rng as usize) % self.queue.len();
            if index != avoid {
                return Some(index);
            }
        }
        Some((avoid + 1) % self.queue.len())
    }

    fn next_index(&mut self, current: usize, manual: bool) -> Option<usize> {
        if self.shuffle {
            return self.random_index(current);
        }
        if current + 1 < self.queue.len() {
            return Some(current + 1);
        }
        if self.repeat == RepeatMode::All || (!manual && self.repeat == RepeatMode::One) {
            return Some(0);
        }
        None
    }

    pub fn next(&mut self) {
        let Some(current) = self.current else {
            return;
        };
        if let Some(next) = self.next_index(current, true) {
            self.current = Some(next);
            let _ = self.start_current();
        } else {
            self.stop();
        }
    }

    pub fn previous(&mut self) {
        let Some(current) = self.current else {
            return;
        };
        // Restart the current track when we are already a few seconds in.
        if self.position() > 3.0 {
            let _ = self.seek(0.0);
            return;
        }
        let index = current.saturating_sub(1);
        self.current = Some(index);
        let _ = self.start_current();
    }

    pub fn seek(&mut self, seconds: f32) -> Result<(), String> {
        let mut target = seconds.max(0.0);
        // Seeking to the exact end has no packet to land on, so keep the
        // target just inside the track. This also stops Ctrl+Right from
        // pushing the playhead past the last frame.
        let duration = self.duration();
        if duration > 0.0 {
            target = target.min((duration - 0.05).max(0.0));
        }
        if self.sink.is_none() {
            self.resume_at = target;
            return Ok(());
        }
        // Seeking inside a pre-fetched queue is unreliable across all backends;
        // rebuild the current track at the requested offset instead.
        if self.prefetched {
            return self.start_current_at(target);
        }
        let Some(sink) = &self.sink else {
            return Ok(());
        };
        // rodio's `try_seek` seeks the source that is currently being played,
        // not the whole sink timeline, so the target is already track-relative.
        sink.try_seek(Duration::from_secs_f32(target))
            .map_err(|error| format!("Seek failed: {error}"))
    }

    pub fn seek_relative(&mut self, delta: f32) {
        let target = (self.position() + delta).max(0.0);
        let _ = self.seek(target);
    }

    pub fn set_position_fraction(&mut self, fraction: f32) {
        let duration = self.duration();
        if duration > 0.0 {
            let _ = self.seek(fraction.clamp(0.0, 1.0) * duration);
        }
    }

    /// Called once per frame: advances the queue when a track ends and picks
    /// up finished waveform analyses.
    pub fn tick(&mut self) {
        self.poll_analyses();
        if self.state != PlaybackState::Playing {
            return;
        }
        let (elapsed, duration, empty) = {
            let Some(sink) = &self.sink else {
                return;
            };
            (sink.get_pos().as_secs_f32(), self.duration(), sink.empty())
        };
        let Some(current) = self.current else {
            return;
        };

        // Append the next track slightly before the current one ends. The sink
        // plays both decoders back-to-back without a gap.
        if !self.prefetched
            && self.repeat != RepeatMode::One
            && duration > 0.0
            && elapsed > duration - 1.5
        {
            if let Some(next) = self.next_index(current, false) {
                if next != current {
                    if let Some(track) = self.queue.get(next) {
                        if let Some(decoder) = open_decoder(&track.path) {
                            if let Some(sink) = &self.sink {
                                sink.append(decoder);
                            }
                            self.prefetched = true;
                            self.prefetched_index = Some(next);
                        }
                    }
                }
            }
        }

        let finished = if duration > 0.0 {
            elapsed + 0.05 >= duration
        } else {
            empty
        };
        if finished {
            if self.repeat == RepeatMode::One {
                let _ = self.start_current();
                return;
            }
            let next = self
                .prefetched_index
                .take()
                .or_else(|| self.next_index(current, false));
            if let Some(next) = next {
                if next == current {
                    let _ = self.start_current();
                } else if self.prefetched {
                    // The sink is already playing the prefetched track; its
                    // position source restarted at zero, so there is no offset
                    // to carry over.
                    self.current = Some(next);
                    self.prefetched = false;
                    if let Some(track) = self.queue.get(next) {
                        self.request_waveform(track.path.clone());
                    }
                } else {
                    self.current = Some(next);
                    let _ = self.start_current();
                }
            } else if empty {
                self.state = PlaybackState::Stopped;
            }
        }
    }

    fn poll_analyses(&mut self) {
        while let Ok(result) = self.analyze_rx.try_recv() {
            self.pending.remove(&result.path);
            if let Some(waveform) = result.waveform {
                if waveform.duration > 0.0 {
                    for track in &mut self.queue {
                        if track.path == result.path && track.duration <= 0.0 {
                            track.duration = waveform.duration;
                        }
                    }
                }
                self.waveform_cache.insert(result.path, Arc::new(waveform));
            }
        }
    }

    fn request_waveform(&mut self, path: PathBuf) {
        if self.waveform_cache.contains_key(&path) || self.pending.contains(&path) {
            return;
        }
        self.pending.insert(path.clone());
        let sender = self.analyze_tx.clone();
        thread::spawn(move || {
            let waveform = analyze(&path);
            let _ = sender.send(AnalyzeResult { path, waveform });
        });
    }

    fn ensure_device(&mut self) -> bool {
        if self.handle.is_some() {
            return true;
        }
        match OutputStream::try_default() {
            Ok((stream, handle)) => {
                self.stream = Some(stream);
                self.handle = Some(handle);
                true
            }
            Err(error) => {
                self.error = Some(format!("Audio output unavailable: {error}"));
                false
            }
        }
    }

    fn start_current(&mut self) -> Result<(), String> {
        self.start_current_at(0.0)
    }

    fn start_current_at(&mut self, offset: f32) -> Result<(), String> {
        if !self.enabled {
            return Err("Built-in player is disabled in Settings.".to_string());
        }
        let Some(index) = self.current else {
            return Ok(());
        };
        let Some(track) = self.queue.get(index).cloned() else {
            return Ok(());
        };
        if !self.ensure_device() {
            return Err(self
                .error
                .clone()
                .unwrap_or_else(|| "Audio output unavailable.".to_string()));
        }
        let Some(handle) = &self.handle else {
            return Err("Audio output unavailable.".to_string());
        };
        let Some(decoder) = open_decoder(&track.path) else {
            self.error = Some(format!("Cannot decode {}", track.path.display()));
            return Err(self.error.clone().unwrap_or_default());
        };
        let sink = match Sink::try_new(handle) {
            Ok(sink) => sink,
            Err(error) => {
                self.error = Some(format!("Cannot create audio sink: {error}"));
                return Err(self.error.clone().unwrap_or_default());
            }
        };
        let offset = offset.max(self.resume_at);
        self.resume_at = 0.0;
        sink.set_volume(self.volume);
        sink.append(decoder);
        if let Some(previous) = self.sink.take() {
            previous.stop();
        }
        self.sink = Some(sink);
        self.state = PlaybackState::Playing;
        self.prefetched = false;
        self.prefetched_index = None;
        self.error = None;
        if offset > 0.0 {
            let _ = self
                .sink
                .as_ref()
                .expect("sink assigned")
                .try_seek(Duration::from_secs_f32(offset));
        }
        self.request_waveform(track.path);
        Ok(())
    }
}

fn seed_rng() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15);
    nanos | 1
}

fn open_decoder(path: &Path) -> Option<SymphoniaSource> {
    SymphoniaSource::new(path)
}

fn track_title(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("Unknown track")
        .to_string()
}

fn open_source(path: &Path) -> Option<Box<dyn symphonia::core::formats::FormatReader>> {
    let file = File::open(path).ok()?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|extension| extension.to_str()) {
        hint.with_extension(extension);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            stream,
            &FormatOptions {
                enable_gapless: true,
                ..Default::default()
            },
            &MetadataOptions::default(),
        )
        .ok()?;
    Some(probed.format)
}

fn probe_duration(path: &Path) -> Option<f32> {
    let format = open_source(path)?;
    let track = format.default_track()?;
    let sample_rate = track.codec_params.sample_rate? as f32;
    let frames = track.codec_params.n_frames? as f32;
    if sample_rate > 0.0 && frames > 0.0 {
        Some(frames / sample_rate)
    } else {
        None
    }
}

/// Decode the whole file once and keep one peak per ~20 ms, then reduce that
/// to a fixed 512-bucket waveform for drawing.
fn analyze(path: &Path) -> Option<Waveform> {
    let mut format = open_source(path)?;
    let track = format.default_track()?.clone();
    let track_id = track.id;
    let sample_rate = track.codec_params.sample_rate.unwrap_or(44_100).max(1) as usize;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .ok()?;

    let block_size = (sample_rate / 50).max(1);
    let mut blocks: Vec<f32> = Vec::new();
    let mut block_peak = 0.0f32;
    let mut block_frames = 0usize;
    let mut total_frames = 0usize;

    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(_) => break,
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
            Err(_) => break,
        };
        let spec = *decoded.spec();
        let channels = spec.channels.count().max(1);
        let mut buffer = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buffer.copy_interleaved_ref(decoded);
        for frame in buffer.samples().chunks(channels) {
            let peak = frame
                .iter()
                .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
            block_peak = block_peak.max(peak);
            block_frames += 1;
            total_frames += 1;
            if block_frames >= block_size {
                blocks.push(block_peak);
                block_peak = 0.0;
                block_frames = 0;
            }
        }
    }
    if block_frames > 0 {
        blocks.push(block_peak);
    }
    if blocks.is_empty() {
        return None;
    }
    let maximum = blocks.iter().copied().fold(0.0f32, f32::max).max(0.001);
    let buckets = 512usize.min(blocks.len().max(64));
    let mut peaks = Vec::with_capacity(buckets);
    for bucket in 0..buckets {
        let start = bucket * blocks.len() / buckets;
        let end = ((bucket + 1) * blocks.len() / buckets).max(start + 1);
        let peak = blocks[start.min(blocks.len() - 1)..end.min(blocks.len())]
            .iter()
            .copied()
            .fold(0.0f32, f32::max);
        peaks.push((peak / maximum).clamp(0.0, 1.0));
    }
    let duration = if sample_rate > 0 {
        total_frames as f32 / sample_rate as f32
    } else {
        0.0
    };
    Some(Waveform { peaks, duration })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_extensions_are_case_insensitive() {
        assert!(is_audio_file(Path::new("track.FLAC")));
        assert!(is_audio_file(Path::new("track.mp3")));
        assert!(!is_audio_file(Path::new("cover.jpg")));
    }

    #[test]
    fn natural_sort_orders_track_numbers() {
        let mut paths = vec![
            PathBuf::from("10.flac"),
            PathBuf::from("2.flac"),
            PathBuf::from("1.flac"),
        ];
        paths.sort_by(|a, b| natural_key(a).cmp(&natural_key(b)));
        assert_eq!(paths[0], PathBuf::from("1.flac"));
        assert_eq!(paths[1], PathBuf::from("2.flac"));
        assert_eq!(paths[2], PathBuf::from("10.flac"));
    }

    #[test]
    fn waveform_is_extracted_from_wav() {
        let path = std::env::temp_dir().join("dst-explorer-audio-test.wav");
        write_test_wav(&path);
        let waveform = analyze(&path).expect("wav should decode");
        assert!(
            (waveform.duration - 2.0).abs() < 0.1,
            "duration {}",
            waveform.duration
        );
        assert!(!waveform.peaks.is_empty());
        assert!(waveform.peaks.iter().all(|peak| (0.0..=1.0).contains(peak)));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn sink_position_restarts_for_prefetched_track() {
        // rodio's Sink reports the position of the source that is currently
        // playing. A prefetched track is a separate source, so its position
        // starts at zero again; the player must not subtract a cumulative
        // offset (that used to freeze the timeline after a gapless switch).
        let first = std::env::temp_dir().join("dst-explorer-audio-pos-1.wav");
        let second = std::env::temp_dir().join("dst-explorer-audio-pos-2.wav");
        write_test_wav(&first);
        write_test_wav(&second);

        let (sink, mut output) = rodio::Sink::new_idle();
        sink.append(open_decoder(&first).expect("first wav should decode"));
        for _ in 0..8_000 {
            let _ = output.next();
        }
        let position = sink.get_pos().as_secs_f32();
        assert!(
            (position - 1.0).abs() < 0.15,
            "first track position {position}"
        );

        sink.append(open_decoder(&second).expect("second wav should decode"));
        // Finish the remaining second of the first track, then play 0.25 s of
        // the second one.
        for _ in 0..10_000 {
            let _ = output.next();
        }
        let position = sink.get_pos().as_secs_f32();
        assert!(
            position < 0.6,
            "gapless position should restart at zero, got {position}"
        );

        drop(sink);
        let _ = std::fs::remove_file(first);
        let _ = std::fs::remove_file(second);
    }

    #[test]
    fn decoder_supports_seeking() {
        // The player seeks through `Sink::try_seek`, so the custom source must
        // forward `try_seek` to Symphonia's format reader.
        use rodio::Source;

        let path = std::env::temp_dir().join("dst-explorer-audio-seek.wav");
        write_test_wav(&path);

        let mut decoder = open_decoder(&path).expect("wav should decode");
        for _ in 0..4_000 {
            let _ = decoder.next();
        }
        decoder
            .try_seek(Duration::from_secs_f32(1.5))
            .expect("decoder should support seeking");

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn sink_seek_moves_the_playhead() {
        // Exercise the real playback path: `AudioPlayer::seek` goes through
        // `Sink::try_seek`, which only completes while samples are being
        // consumed, so drain the idle output on a background thread.
        let path = std::env::temp_dir().join("dst-explorer-audio-sink-seek.wav");
        write_test_wav(&path);

        let (sink, mut output) = rodio::Sink::new_idle();
        sink.append(open_decoder(&path).expect("wav should decode"));
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let consumer = thread::spawn(move || {
            let mut samples = 0u32;
            loop {
                if stop_rx.try_recv().is_ok() {
                    break;
                }
                if output.next().is_none() {
                    break;
                }
                samples += 1;
                // 8 kHz test file: pace at roughly real time.
                if samples % 8 == 0 {
                    thread::sleep(Duration::from_millis(1));
                }
            }
        });

        thread::sleep(Duration::from_millis(150));
        let before = sink.get_pos().as_secs_f32();
        assert!(before < 1.0, "unexpected initial position {before}");

        sink.try_seek(Duration::from_secs_f32(1.5))
            .expect("sink seek should succeed");
        thread::sleep(Duration::from_millis(150));
        let after = sink.get_pos().as_secs_f32();
        assert!(
            (1.4..1.9).contains(&after),
            "seek should move the playhead near 1.5s, got {after}"
        );

        let _ = stop_tx.send(());
        let _ = consumer.join();
        drop(sink);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn flac_decoder_supports_seeking() {
        // Regression test for rodio's `ReadSeekSource::byte_len() == None`,
        // which made Symphonia's FLAC demuxer report `Unseekable`. The player
        // now decodes through its own `File`-backed source.
        use rodio::Source;

        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/test/seek.flac");
        let mut decoder = open_decoder(&path).expect("flac should decode");
        for _ in 0..4_000 {
            let _ = decoder.next();
        }
        decoder
            .try_seek(Duration::from_secs_f32(1.5))
            .expect("flac decoder should support seeking");
    }

    fn write_test_wav(path: &Path) {
        let sample_rate = 8_000u32;
        let frames = sample_rate * 2;
        let data_size = frames * 2;
        let mut bytes = Vec::with_capacity(44 + data_size as usize);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_size).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&(sample_rate * 2).to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_size.to_le_bytes());
        for frame in 0..frames {
            let t = frame as f32 / sample_rate as f32;
            let sample = (t * 440.0 * std::f32::consts::TAU).sin();
            bytes.extend_from_slice(&((sample * 12_000.0) as i16).to_le_bytes());
        }
        std::fs::write(path, bytes).expect("write test wav");
    }
}

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use std::time::Instant;

use crossbeam_channel::{unbounded, Receiver, Sender};
use eframe::egui;

use crate::api::USER_AGENT;

/// Hard cap on the approximate decoded-texture memory kept alive.
pub const TEXTURE_BYTE_BUDGET: usize = 20 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct ImageSpec {
    pub key: String,
    pub url: String,
    pub cache_path: PathBuf,
    pub max_dim: u32,
}

#[derive(Debug)]
pub struct DecodedImage {
    pub key: String,
    pub size: [usize; 2],
    pub rgba: Vec<u8>,
    /// Saturated dominant colour sampled from the cover, used for ambient glow.
    pub tint: Option<egui::Color32>,
}

struct ImageRequest {
    spec: ImageSpec,
}

pub struct ImageLoader {
    tx: Sender<ImageRequest>,
    rx: Receiver<Result<DecodedImage, String>>,
    requested: HashSet<String>,
    failed: HashSet<String>,
}

impl ImageLoader {
    pub fn new(workers: usize) -> Self {
        let (req_tx, req_rx) = unbounded::<ImageRequest>();
        let (res_tx, res_rx) = unbounded::<Result<DecodedImage, String>>();
        let worker_count = workers.max(2);
        // One shared client (cheap Arc clone) instead of one per worker: each
        // separate client keeps its own connection pool and TLS state.
        let client = match reqwest::blocking::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(25))
            .connect_timeout(Duration::from_secs(10))
            .build()
        {
            Ok(client) => Some(client),
            Err(err) => {
                eprintln!("Could not build image client: {err}");
                None
            }
        };
        for index in 0..worker_count {
            let rx = req_rx.clone();
            let tx = res_tx.clone();
            let client = client.clone();
            let _ = thread::Builder::new()
                .name(format!("image-worker-{index}"))
                .spawn(move || image_worker(rx, tx, client));
        }
        Self {
            tx: req_tx,
            rx: res_rx,
            requested: HashSet::new(),
            failed: HashSet::new(),
        }
    }

    pub fn request(&mut self, spec: ImageSpec) {
        if self.requested.contains(&spec.key) || self.failed.contains(&spec.key) {
            return;
        }
        self.requested.insert(spec.key.clone());
        if self.tx.send(ImageRequest { spec }).is_err() {
            self.requested.clear();
        }
    }

    /// Whether a new request for this key would actually be dispatched.
    pub fn wants(&self, key: &str) -> bool {
        !self.requested.contains(key) && !self.failed.contains(key)
    }

    pub fn clear_failed(&mut self) {
        self.failed.clear();
    }

    pub fn reset_requests(&mut self) {
        self.requested.clear();
        self.failed.clear();
    }

    pub fn pump(&mut self, ctx: &egui::Context, textures: &mut Textures, budget: usize) -> usize {
        let mut loaded = 0;
        for _ in 0..budget {
            let result = match self.rx.try_recv() {
                Ok(result) => result,
                Err(_) => break,
            };
            match result {
                Ok(image) => {
                    self.requested.remove(&image.key);
                    let pixels = egui::ColorImage::from_rgba_unmultiplied(image.size, &image.rgba);
                    let handle = ctx.load_texture(
                        format!("img:{}", image.key),
                        pixels,
                        egui::TextureOptions::LINEAR,
                    );
                    textures.insert_with_tint(image.key, handle, image.tint);
                    loaded += 1;
                }
                Err(message) => {
                    if let Some(key) = message.strip_prefix("key:") {
                        let key = key.split('|').next().unwrap_or_default().to_string();
                        self.requested.remove(&key);
                        self.failed.insert(key.clone());
                        textures.mark_failed(key);
                    }
                    eprintln!("Image load failed: {message}");
                }
            }
        }
        if !self.requested.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(40));
        }
        loaded
    }
}

fn image_worker(
    rx: Receiver<ImageRequest>,
    tx: Sender<Result<DecodedImage, String>>,
    client: Option<reqwest::blocking::Client>,
) {
    let Some(client) = client else {
        return;
    };

    while let Ok(request) = rx.recv() {
        let spec = request.spec;
        let key = spec.key.clone();
        match load_image(&client, &spec) {
            Ok(image) => {
                let _ = tx.send(Ok(image));
            }
            Err(message) => {
                let _ = tx.send(Err(format!("key:{key}|{message}")));
            }
        }
    }
}

fn load_image(
    client: &reqwest::blocking::Client,
    spec: &ImageSpec,
) -> Result<DecodedImage, String> {
    let bytes = match fs::read(&spec.cache_path) {
        Ok(bytes) if !bytes.is_empty() => bytes,
        _ => {
            let response = client
                .get(&spec.url)
                .header(reqwest::header::REFERER, "https://doujinstyle.com/")
                .header(
                    reqwest::header::ACCEPT,
                    "image/avif,image/webp,image/jpeg,image/png,*/*",
                )
                .send()
                .map_err(|e| format!("network: {e}"))?;
            if !response.status().is_success() {
                return Err(format!("HTTP {} for {}", response.status(), spec.url));
            }
            let bytes = response.bytes().map_err(|e| e.to_string())?.to_vec();
            if bytes.is_empty() {
                return Err("empty response".into());
            }
            if let Some(parent) = spec.cache_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::write(&spec.cache_path, &bytes);
            bytes
        }
    };

    let decoded = image::load_from_memory(&bytes).map_err(|e| format!("decode: {e}"))?;
    let (width, height) = (decoded.width(), decoded.height());
    if width == 0 || height == 0 {
        return Err("zero-sized image".into());
    }
    let decoded = if width > spec.max_dim || height > spec.max_dim {
        decoded.thumbnail(spec.max_dim, spec.max_dim)
    } else {
        decoded
    };
    let rgba = decoded.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    let tint = dominant_tint(rgba.as_raw(), rgba.width(), rgba.height());
    Ok(DecodedImage {
        key: spec.key.clone(),
        size,
        rgba: rgba.into_raw(),
        tint,
    })
}

/// Pick a vivid colour from the image without allocating another image.
///
/// Samples at most ~64x64 pixels and averages the most saturated ones. The
/// result is only used to tint soft glow shapes, so an approximate hue is fine.
fn dominant_tint(rgba: &[u8], width: u32, height: u32) -> Option<egui::Color32> {
    if width == 0 || height == 0 {
        return None;
    }
    let step_x = (width / 64).max(1);
    let step_y = (height / 64).max(1);
    let mut sum = [0f32; 3];
    let mut weight_sum = 0f32;
    let mut y = 0;
    while y < height {
        let mut x = 0;
        while x < width {
            let index = ((y * width + x) * 4) as usize;
            let r = rgba[index] as f32;
            let g = rgba[index + 1] as f32;
            let b = rgba[index + 2] as f32;
            let a = rgba[index + 3] as f32;
            let max = r.max(g).max(b);
            let min = r.min(g).min(b);
            let luma = (r + g + b) / 3.0;
            // Ignore transparent, near-black and blown-out pixels.
            if a < 160.0 || luma < 14.0 || luma > 242.0 {
                x += step_x;
                continue;
            }
            let saturation = (max - min) / max.max(1.0);
            // Favour colourful pixels but keep some weight for muted covers.
            let weight =
                (saturation * saturation * 3.0 + 0.08) * (1.0 - (luma - 128.0).abs() / 200.0);
            if weight > 0.0 {
                sum[0] += r * weight;
                sum[1] += g * weight;
                sum[2] += b * weight;
                weight_sum += weight;
            }
            x += step_x;
        }
        y += step_y;
    }
    if weight_sum <= 0.0 {
        return None;
    }
    let mut r = (sum[0] / weight_sum).clamp(0.0, 255.0) as u8;
    let mut g = (sum[1] / weight_sum).clamp(0.0, 255.0) as u8;
    let mut b = (sum[2] / weight_sum).clamp(0.0, 255.0) as u8;
    // Keep the glow readable on top of dark panels.
    let max = r.max(g).max(b);
    if max < 60 {
        let lift = 60.0 / max.max(1) as f32;
        r = (r as f32 * lift).min(255.0) as u8;
        g = (g as f32 * lift).min(255.0) as u8;
        b = (b as f32 * lift).min(255.0) as u8;
    }
    Some(egui::Color32::from_rgb(r, g, b))
}

struct TexEntry {
    handle: egui::TextureHandle,
    last_used: u64,
    inserted: Instant,
    /// Approximate VRAM/RAM footprint of the decoded RGBA texture.
    bytes: usize,
    /// Dominant colour of the source image, used for ambient glow.
    tint: Option<egui::Color32>,
}

pub struct Textures {
    entries: HashMap<String, TexEntry>,
    /// Keys whose download/decode failed. Used to stop the animated placeholder
    /// from requesting repaints forever.
    failed: HashSet<String>,
    total_bytes: usize,
    frame: u64,
    reported: (usize, usize),
}

impl Default for Textures {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            failed: HashSet::new(),
            total_bytes: 0,
            frame: 0,
            reported: (0, 0),
        }
    }
}

impl Textures {
    pub fn begin_frame(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }

    pub fn insert_with_tint(
        &mut self,
        key: String,
        handle: egui::TextureHandle,
        tint: Option<egui::Color32>,
    ) {
        let [width, height] = handle.size();
        let bytes = width.saturating_mul(height).saturating_mul(4);
        self.failed.remove(&key);
        if let Some(previous) = self.entries.insert(
            key,
            TexEntry {
                handle,
                last_used: self.frame,
                inserted: Instant::now(),
                bytes,
                tint,
            },
        ) {
            self.total_bytes = self.total_bytes.saturating_sub(previous.bytes);
        }
        self.total_bytes = self.total_bytes.saturating_add(bytes);
    }

    pub fn mark_failed(&mut self, key: String) {
        self.failed.insert(key);
    }

    pub fn is_failed(&self, key: &str) -> bool {
        self.failed.contains(key)
    }

    /// Dominant colour sampled from a cover, if it has finished loading.
    pub fn tint(&self, key: &str) -> Option<egui::Color32> {
        self.entries.get(key).and_then(|entry| entry.tint)
    }

    pub fn get(&mut self, key: &str) -> Option<&egui::TextureHandle> {
        let frame = self.frame;
        self.entries.get_mut(key).map(|entry| {
            entry.last_used = frame;
            &entry.handle
        })
    }

    /// Texture plus a 0..=1 fade-in factor based on how long it has existed.
    pub fn get_with_fade(&mut self, key: &str) -> Option<(&egui::TextureHandle, f32)> {
        let frame = self.frame;
        self.entries.get_mut(key).map(|entry| {
            entry.last_used = frame;
            let alpha = (entry.inserted.elapsed().as_secs_f32() / 0.24).clamp(0.0, 1.0);
            (&entry.handle, alpha)
        })
    }

    /// Drop least-recently-used textures so scrolling many pages can't grow memory without bound.
    ///
    /// Two independent limits are enforced: a maximum entry count and a maximum
    /// approximate byte size (decoded RGBA). The byte cap is what really keeps
    /// RAM in check, since a single 900px cover costs ~3 MB.
    pub fn trim(&mut self, budget: usize, max_bytes: usize) {
        self.report(self.total_bytes);

        if self.entries.len() <= budget && self.total_bytes <= max_bytes {
            return;
        }

        let target = budget.saturating_sub(budget / 6).max(50);
        let byte_target = max_bytes.saturating_sub(max_bytes / 6);
        let mut ages: Vec<(String, u64)> = self
            .entries
            .iter()
            .map(|(key, entry)| (key.clone(), entry.last_used))
            .collect();
        ages.sort_by_key(|(_, age)| *age);
        for (key, _) in ages {
            if self.entries.len() <= target && self.total_bytes <= byte_target {
                break;
            }
            if let Some(entry) = self.entries.remove(&key) {
                self.total_bytes = self.total_bytes.saturating_sub(entry.bytes);
            }
        }
        self.report(self.total_bytes);
    }

    fn report(&mut self, total_bytes: usize) {
        use std::sync::OnceLock;
        static ENABLED: OnceLock<bool> = OnceLock::new();
        if !*ENABLED.get_or_init(|| std::env::var_os("DST_TEX_STATS").is_some()) {
            return;
        }
        let stats = (self.entries.len(), total_bytes);
        if stats != self.reported {
            self.reported = stats;
            eprintln!(
                "textures: {} entries, {:.2} MB",
                stats.0,
                stats.1 as f64 / (1024.0 * 1024.0)
            );
        }
    }
}

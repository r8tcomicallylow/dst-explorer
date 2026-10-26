use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use crossbeam_channel::{unbounded, Receiver, Sender};
use eframe::egui;

use crate::api::USER_AGENT;

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
        for index in 0..worker_count {
            let rx = req_rx.clone();
            let tx = res_tx.clone();
            let _ = thread::Builder::new()
                .name(format!("image-worker-{index}"))
                .spawn(move || image_worker(rx, tx));
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
                    textures.insert(image.key, handle);
                    loaded += 1;
                }
                Err(message) => {
                    if let Some(key) = message.strip_prefix("key:") {
                        let key = key.split('|').next().unwrap_or_default().to_string();
                        self.requested.remove(&key);
                        self.failed.insert(key);
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

fn image_worker(rx: Receiver<ImageRequest>, tx: Sender<Result<DecodedImage, String>>) {
    let client = reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(25))
        .connect_timeout(Duration::from_secs(10))
        .build();
    let client = match client {
        Ok(client) => client,
        Err(err) => {
            eprintln!("Could not build image client: {err}");
            return;
        }
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
    Ok(DecodedImage {
        key: spec.key.clone(),
        size,
        rgba: rgba.into_raw(),
    })
}

struct TexEntry {
    handle: egui::TextureHandle,
    last_used: u64,
}

pub struct Textures {
    entries: HashMap<String, TexEntry>,
    frame: u64,
}

impl Default for Textures {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            frame: 0,
        }
    }
}

impl Textures {
    pub fn begin_frame(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }

    pub fn insert(&mut self, key: String, handle: egui::TextureHandle) {
        self.entries.insert(
            key,
            TexEntry {
                handle,
                last_used: self.frame,
            },
        );
    }

    pub fn get(&mut self, key: &str) -> Option<&egui::TextureHandle> {
        let frame = self.frame;
        self.entries.get_mut(key).map(|entry| {
            entry.last_used = frame;
            &entry.handle
        })
    }

    /// Drop least-recently-used textures so scrolling many pages can't grow memory without bound.
    pub fn trim(&mut self, budget: usize) {
        if self.entries.len() <= budget {
            return;
        }
        let target = budget.saturating_sub(budget / 6).max(50);
        let mut ages: Vec<(String, u64)> = self
            .entries
            .iter()
            .map(|(key, entry)| (key.clone(), entry.last_used))
            .collect();
        ages.sort_by_key(|(_, age)| *age);
        let remove_count = self.entries.len().saturating_sub(target);
        for (key, _) in ages.into_iter().take(remove_count) {
            self.entries.remove(&key);
        }
    }
}

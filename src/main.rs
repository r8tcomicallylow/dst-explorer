#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod app;
mod downloads;
mod images;
mod models;
mod persist;
mod store;
mod taskbar;
mod theme;
mod tray;

use app::App;
use eframe::egui;
use std::io::Write;

fn main() -> eframe::Result<()> {
    // Initialize logging if DST_EXPLORER_LOG is set
    if std::env::var("DST_EXPLORER_LOG").is_ok() {
        let log_dir = crate::store::data_dir();
        let _ = std::fs::create_dir_all(&log_dir);
        let log_file = log_dir.join("dst-explorer.log");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_file)
            .ok();
        let log_file_clone = log_file.clone();
        if let Some(mut file) = file {
            let _ = writeln!(file, "\n=== DST-Explorer started at {} ===", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
        }
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
            .format_timestamp_millis()
            .target(env_logger::Target::Pipe(Box::new(std::fs::OpenOptions::new().create(true).append(true).open(&log_file_clone).unwrap())))
            .init();
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("DST-Explorer")
            .with_inner_size([1200.0, 760.0])
            .with_min_inner_size([720.0, 520.0])
            .with_icon(std::sync::Arc::new(make_icon())),
        renderer: eframe::Renderer::Glow,
        vsync: true,
        multisampling: 0,
        ..Default::default()
    };

    eframe::run_native(
        "DST-Explorer",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}

pub(crate) fn make_icon() -> egui::IconData {
    let size = 64u32;
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    let accent = (183u8, 165u8, 255u8);
    let radius = 15.0f32;

    let inside_rounded = |x: f32, y: f32, left: f32, top: f32, right: f32, bottom: f32, r: f32| {
        if x < left || x > right || y < top || y > bottom {
            return false;
        }
        let cx = x.clamp(left + r, right - r);
        let cy = y.clamp(top + r, bottom - r);
        (x - cx).powi(2) + (y - cy).powi(2) <= r * r
    };

    for y in 0..size {
        for x in 0..size {
            let fx = x as f32 + 0.5;
            let fy = y as f32 + 0.5;
            let index = ((y * size + x) * 4) as usize;
            if !inside_rounded(fx, fy, 2.0, 2.0, 62.0, 62.0, radius) {
                continue;
            }
            // Soft vertical gradient in the accent color.
            let t = (fy / 64.0).clamp(0.0, 1.0);
            let r = (accent.0 as f32 * (1.05 - t * 0.18)).min(255.0) as u8;
            let g = (accent.1 as f32 * (1.02 - t * 0.16)).min(255.0) as u8;
            let b = accent.2 as u8;
            rgba[index] = r;
            rgba[index + 1] = g;
            rgba[index + 2] = b;
            rgba[index + 3] = 255;

            // Four dark equalizer bars.
            let bars = [(20.0f32, 30.0f32), (30.0, 20.0), (40.0, 27.0), (50.0, 17.0)];
            for (bx, half) in bars {
                if (fx - bx).abs() <= 2.0 && (fy - 32.0).abs() <= half {
                    rgba[index] = 18;
                    rgba[index + 1] = 19;
                    rgba[index + 2] = 27;
                }
            }
        }
    }

    egui::IconData {
        rgba,
        width: size,
        height: size,
    }
}

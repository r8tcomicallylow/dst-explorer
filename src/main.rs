#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod app;
mod downloads;
mod images;
mod i18n;
mod models;
mod persist;
mod store;
mod taskbar;
mod theme;
mod tray;
mod tui;

use app::App;
use eframe::egui;
use std::io::Write;

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        #[cfg(windows)]
        attach_parent_console();
        std::process::exit(run_cli(&args));
    }

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

/// Reattach stdout/stderr to the parent console in release builds, where the
/// binary is linked as a Windows GUI application and has no console of its own.
#[cfg(windows)]
fn attach_parent_console() {
    use std::ptr::null_mut;

    #[link(name = "kernel32")]
    extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
        fn GetStdHandle(which: u32) -> *mut std::ffi::c_void;
        fn SetStdHandle(which: u32, handle: *mut std::ffi::c_void) -> i32;
        fn CreateFileW(
            name: *const u16,
            access: u32,
            share: u32,
            security: *mut std::ffi::c_void,
            disposition: u32,
            flags: u32,
            template: *mut std::ffi::c_void,
        ) -> *mut std::ffi::c_void;
    }

    const ATTACH_PARENT_PROCESS: u32 = 0xFFFF_FFFF;
    const STD_INPUT_HANDLE: u32 = 0xFFFF_FFF6;
    const STD_OUTPUT_HANDLE: u32 = 0xFFFF_FFF5;
    const STD_ERROR_HANDLE: u32 = 0xFFFF_FFF4;
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;
    const OPEN_EXISTING: u32 = 3;

    let wide = |text: &str| -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    };
    let out = wide("CONOUT$");
    let inp = wide("CONIN$");

    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return;
        }
        // Only replace handles that are not already valid.
        if GetStdHandle(STD_OUTPUT_HANDLE).is_null() {
            let handle = CreateFileW(
                out.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null_mut(),
                OPEN_EXISTING,
                0,
                null_mut(),
            );
            if handle as isize != -1 {
                SetStdHandle(STD_OUTPUT_HANDLE, handle);
                SetStdHandle(STD_ERROR_HANDLE, handle);
            }
        }
        if GetStdHandle(STD_INPUT_HANDLE).is_null() {
            let handle = CreateFileW(
                inp.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null_mut(),
                OPEN_EXISTING,
                0,
                null_mut(),
            );
            if handle as isize != -1 {
                SetStdHandle(STD_INPUT_HANDLE, handle);
            }
        }
    }
}

fn cli_help() {
    println!(
        "DST-Explorer {version}\n\
\n\
USAGE:\n\
    dst-explorer                     Launch the graphical app\n\
    dst-explorer --tui               Browse releases in the terminal\n\
    dst-explorer --latest [--page N] [--json]\n\
    dst-explorer --search <text> [--kind title|artist|tags] [--page N] [--json]\n\
    dst-explorer --download <album-id> [--dir <path>]\n\
    dst-explorer --help | --version\n\
\n\
OPTIONS:\n\
    --page N       Page number, starting at 0\n\
    --json         Print machine-readable JSON\n\
    --dir PATH     Folder for downloaded files (default: current directory)\n\
    --kind KIND    Search field: blanket (default), artist, tags or credit\n",
        version = env!("CARGO_PKG_VERSION")
    );
}

fn parse_flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn print_albums(albums: &[models::Album], json: bool) -> Result<(), String> {
    if json {
        let text = serde_json::to_string_pretty(albums).map_err(|e| e.to_string())?;
        println!("{text}");
        return Ok(());
    }
    for album in albums {
        let artist = if album.artist.trim().is_empty() {
            "Unknown artist"
        } else {
            album.artist.trim()
        };
        let format = if album.format.trim().is_empty() {
            "-"
        } else {
            album.format.trim()
        };
        println!(
            "{:>8}  {:<6}  {} — {}{}",
            album.id,
            format,
            artist,
            album.title,
            if album.year > 0 {
                format!(" ({})", album.year)
            } else {
                String::new()
            }
        );
    }
    Ok(())
}

fn run_cli(args: &[String]) -> i32 {
    let json = args.iter().any(|arg| arg == "--json");
    let page = parse_flag_value(args, "--page")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);

    let result = match args[0].as_str() {
        "--help" | "-h" | "help" => {
            cli_help();
            return 0;
        }
        "--version" | "-V" | "version" => {
            println!("dst-explorer {}", env!("CARGO_PKG_VERSION"));
            return 0;
        }
        "--tui" | "tui" => return match tui::run() {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("TUI error: {error}");
                1
            }
        },
        "--latest" | "latest" => {
            let client = api::page_client();
            api::fetch_latest(&client, page).and_then(|feed| print_albums(&feed.albums, json))
        }
        "--search" | "search" => {
            let text = match args.get(1) {
                Some(value) if !value.starts_with("--") => value.clone(),
                _ => {
                    eprintln!("Missing search text. Try: dst-explorer --search \"artist name\"");
                    return 2;
                }
            };
            let kind = match parse_flag_value(args, "--kind").as_deref() {
                Some("artist") => models::SearchKind::Artist,
                Some("tags") | Some("tag") => models::SearchKind::Tags,
                Some("credit") => models::SearchKind::Credit,
                _ => models::SearchKind::Blanket,
            };
            let client = api::page_client();
            let params = api::SearchParams {
                text,
                kind,
                formats: models::FormatFilter::default(),
                page,
            };
            api::fetch_search(&client, &params).and_then(|feed| {
                if !json {
                    if let Some(total) = feed.total_results {
                        eprintln!("{} result(s)", total);
                    }
                }
                print_albums(&feed.albums, json)
            })
        }
        "--download" | "download" => {
            let id = match args.get(1).and_then(|value| value.parse::<u64>().ok()) {
                Some(id) => id,
                None => {
                    eprintln!("Missing album id. Try: dst-explorer --download 12345");
                    return 2;
                }
            };
            let dir = parse_flag_value(args, "--dir")
                .map(std::path::PathBuf::from)
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            eprintln!("Resolving album {id}…");
            downloads::cli_download(id, &dir).map(|path| {
                println!("{}", path.display());
            })
        }
        other => {
            eprintln!("Unknown option: {other}");
            eprintln!("Try: dst-explorer --help");
            return 2;
        }
    };

    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("Error: {error}");
            1
        }
    }
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

use std::sync::mpsc::{channel, Receiver, TryRecvError};

use eframe::egui;
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayAction {
    Show,
    PauseAll,
    ResumeAll,
    Quit,
}

/// Native tray icon with a small menu. Events are forwarded through a channel
/// and wake the egui event loop, so actions also work while the window is hidden.
pub struct Tray {
    _icon: TrayIcon,
    rx: Receiver<TrayAction>,
}

impl Tray {
    pub fn new(ctx: &egui::Context) -> Option<Self> {
        let menu = Menu::new();
        let open = MenuItem::new("Open Doujin Explorer", true, None);
        let pause = MenuItem::new("Pause all downloads", true, None);
        let resume = MenuItem::new("Resume all downloads", true, None);
        let quit = MenuItem::new("Quit", true, None);
        menu.append_items(&[&open, &pause, &resume, &quit]).ok()?;

        let icon_data = crate::make_icon();
        let icon = Icon::from_rgba(icon_data.rgba, icon_data.width, icon_data.height).ok()?;

        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("Doujin Explorer")
            .with_icon(icon)
            .build()
            .ok()?;

        let (tx, rx) = channel();
        let menu_ids = (
            open.id().clone(),
            pause.id().clone(),
            resume.id().clone(),
            quit.id().clone(),
        );
        let menu_tx = tx.clone();
        let repaint = ctx.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let action = if event.id == menu_ids.0 {
                Some(TrayAction::Show)
            } else if event.id == menu_ids.1 {
                Some(TrayAction::PauseAll)
            } else if event.id == menu_ids.2 {
                Some(TrayAction::ResumeAll)
            } else if event.id == menu_ids.3 {
                Some(TrayAction::Quit)
            } else {
                None
            };
            if let Some(action) = action {
                let _ = menu_tx.send(action);
                repaint.request_repaint();
            }
        }));

        let click_tx = tx;
        let repaint = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            let show = matches!(
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
            if show {
                let _ = click_tx.send(TrayAction::Show);
                repaint.request_repaint();
            }
        }));

        Some(Self { _icon: tray, rx })
    }

    pub fn poll(&self) -> Vec<TrayAction> {
        let mut actions = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(action) => actions.push(action),
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
        actions
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        TrayIconEvent::set_event_handler::<fn(TrayIconEvent)>(None);
        MenuEvent::set_event_handler::<fn(MenuEvent)>(None);
    }
}

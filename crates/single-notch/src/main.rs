//! Phase 1 spike (E30): prove `single-notch` builds and opens a window
//! before any product UI. Deliberately minimal -- no custom container
//! styling, no close-key handling -- both need iced 0.14's real widget
//! API confirmed live (docs.rs, not memory) before adding; the OS window
//! chrome's own close control is enough for a discardable spike. See
//! `docs/superpowers/plans/2026-09-17-e30-notch-hud.md` Phase 1 Task 1.

use iced::widget::{column, container, text};
use iced::{Element, Length};

#[derive(Default)]
struct NotchApp;

#[derive(Debug, Clone)]
enum Message {}

fn update(_state: &mut NotchApp, message: Message) {
    match message {}
}

fn view(_state: &NotchApp) -> Element<'_, Message> {
    container(column![text("SingleCLI Notch").size(14)].padding(10))
        .width(Length::Fixed(128.0))
        .height(Length::Fixed(28.0))
        .into()
}

fn main() -> iced::Result {
    iced::application(NotchApp::default, update, view)
        .title("SingleCLI Notch")
        .run()
}

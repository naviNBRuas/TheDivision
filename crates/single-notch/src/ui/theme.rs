//! Color/radius/typography tokens (spec §Visual language). No purple.
//! iced 0.14 confirmed API only -- verified against the real vendored
//! `iced_core`/`iced_widget` 0.14 source, not memory.

use iced::Color;

pub const FILL: Color = Color::from_rgba8(0x0D, 0x0D, 0x0F, 0.88);
pub const HAIRLINE: Color = Color::from_rgba8(0xFF, 0xFF, 0xFF, 0.08);

pub const TEAL: Color = Color::from_rgb8(0x2E, 0xC4, 0xB6);
pub const AMBER: Color = Color::from_rgb8(0xE9, 0xA3, 0x19);
pub const RED: Color = Color::from_rgb8(0xE8, 0x5D, 0x4C);

pub const RADIUS_PILL: f32 = 14.0;
pub const RADIUS_CARD: f32 = 16.0;

pub const BODY_SIZE: f32 = 12.0;

use crate::model::HealthTone;

pub fn tone_color(tone: HealthTone) -> Color {
    match tone {
        HealthTone::Healthy => TEAL,
        HealthTone::Amber => AMBER,
        HealthTone::Degraded => RED,
    }
}

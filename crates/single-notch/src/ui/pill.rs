//! Collapsed ~128×28 pill: tone dot, compact tally, optional running
//! spinner glyph. See plan Phase 4 Task 8.

use crate::model::NotchSnapshot;
use crate::ui::theme;
use iced::widget::{container, row, text};
use iced::{Color, Element, Length};

/// `"{provider_count} providers · {total_keys} keys"`, or the two
/// busiest platforms (`nvidia: 2 · google: 2`) when there are 1-2
/// providers total, matching the spec's "or" phrasing for a compact pool.
fn tally_text(snapshot: &NotchSnapshot) -> String {
    if snapshot.providers.len() <= 2 && !snapshot.providers.is_empty() {
        snapshot
            .providers
            .iter()
            .map(|p| format!("{}: {}", p.platform, p.key_count))
            .collect::<Vec<_>>()
            .join(" · ")
    } else {
        format!("{} providers · {} keys", snapshot.provider_count, snapshot.total_keys)
    }
}

pub(crate) fn dot<Message: 'static>(color: Color) -> Element<'static, Message> {
    container(text(""))
        .width(Length::Fixed(6.0))
        .height(Length::Fixed(6.0))
        .style(move |_theme| container::Style {
            background: Some(iced::Background::Color(color)),
            border: iced::Border { radius: 3.0.into(), ..Default::default() },
            ..Default::default()
        })
        .into()
}

pub fn view<Message: 'static>(snapshot: &NotchSnapshot) -> Element<'static, Message> {
    let spinner = if snapshot.any_goal_running { " ⟳" } else { "" };
    let label = format!("{}{}", tally_text(snapshot), spinner);

    container(
        row![dot(theme::tone_color(snapshot.tone)), text(label).size(theme::BODY_SIZE)]
            .spacing(6)
            .align_y(iced::Alignment::Center),
    )
    .padding([6, 10])
    .width(Length::Fixed(128.0))
    .height(Length::Fixed(28.0))
    .style(|_theme| container::Style {
        background: Some(iced::Background::Color(theme::FILL)),
        border: iced::Border { color: theme::HAIRLINE, width: 1.0, radius: theme::RADIUS_PILL.into() },
        ..Default::default()
    })
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{HealthTone, ProviderTally};

    fn base_snapshot() -> NotchSnapshot {
        NotchSnapshot {
            tone: HealthTone::Healthy,
            degraded: false,
            healthy_ratio: 1.0,
            provider_count: 0,
            total_keys: 0,
            providers: vec![],
            benches: vec![],
            activity: vec![],
            agents: vec![],
            any_goal_running: false,
            detail: Default::default(),
        }
    }

    #[test]
    fn tally_text_uses_provider_summary_for_many_providers() {
        let mut s = base_snapshot();
        s.provider_count = 5;
        s.total_keys = 8;
        s.providers = vec![
            ProviderTally { platform: "a".into(), key_count: 3, cooldown: "clear".into(), headroom: "x".into() },
            ProviderTally { platform: "b".into(), key_count: 2, cooldown: "clear".into(), headroom: "x".into() },
            ProviderTally { platform: "c".into(), key_count: 1, cooldown: "clear".into(), headroom: "x".into() },
        ];
        assert_eq!(tally_text(&s), "5 providers · 8 keys");
    }

    #[test]
    fn tally_text_names_platforms_when_pool_is_small() {
        let mut s = base_snapshot();
        s.providers = vec![
            ProviderTally { platform: "nvidia".into(), key_count: 2, cooldown: "clear".into(), headroom: "x".into() },
            ProviderTally { platform: "google".into(), key_count: 1, cooldown: "clear".into(), headroom: "x".into() },
        ];
        assert_eq!(tally_text(&s), "nvidia: 2 · google: 1");
    }

    #[test]
    fn tally_text_falls_back_to_summary_when_no_providers() {
        let s = base_snapshot();
        assert_eq!(tally_text(&s), "0 providers · 0 keys");
    }
}

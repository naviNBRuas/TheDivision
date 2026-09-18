//! Expanded ~340×148 card: header, provider rows, benches, activity,
//! agent auth dots. Grows downward from the same top-center anchor the
//! pill sits at. See plan Phase 5 Task 10.

use crate::model::{AgentAuthDot, NotchSnapshot};
use crate::ui::pill::dot;
use crate::ui::theme;
use iced::widget::{column, container, row, text};
use iced::{Element, Length};
use single_protocol::AuthState;

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max.saturating_sub(1)).collect::<String>())
    }
}

fn agent_color(state: AuthState) -> iced::Color {
    match state {
        AuthState::Authenticated => theme::TEAL,
        AuthState::NotAuthenticated => theme::RED,
        AuthState::Unsupported => theme::AMBER,
    }
}

fn header<Message: 'static>(snapshot: &NotchSnapshot) -> Element<'static, Message> {
    let label = if snapshot.degraded { "DEGRADED".to_string() } else { "Pool healthy".to_string() };
    let pct = (snapshot.healthy_ratio * 100.0).round() as i64;
    row![dot(theme::tone_color(snapshot.tone)), text(format!("{label} · {pct}%")).size(theme::BODY_SIZE)]
        .spacing(6)
        .align_y(iced::Alignment::Center)
        .into()
}

fn provider_rows<Message: 'static>(snapshot: &NotchSnapshot) -> Element<'static, Message> {
    let rows: Vec<Element<'static, Message>> = snapshot
        .providers
        .iter()
        .map(|p| text(format!("{}  {} keys  {}  {}", p.platform, p.key_count, p.cooldown, p.headroom)).size(theme::BODY_SIZE).into())
        .collect();
    column(rows).spacing(2).into()
}

fn bench_rows<Message: 'static>(snapshot: &NotchSnapshot) -> Element<'static, Message> {
    let rows: Vec<Element<'static, Message>> = snapshot
        .benches
        .iter()
        .map(|b| text(format!("{}/{}  key_{}  {}s  {}", b.platform, b.model, b.key_id, b.remaining_secs, b.provenance)).size(theme::BODY_SIZE).into())
        .collect();
    column(rows).spacing(2).into()
}

fn activity_rows<Message: 'static>(snapshot: &NotchSnapshot) -> Element<'static, Message> {
    let rows: Vec<Element<'static, Message>> = snapshot
        .activity
        .iter()
        .take(3)
        .map(|a| text(format!("{}  {}", truncate(&a.text, 40), a.status)).size(theme::BODY_SIZE).into())
        .collect();
    column(rows).spacing(2).into()
}

fn agent_dots<Message: 'static>(agents: &[AgentAuthDot]) -> Element<'static, Message> {
    let items: Vec<Element<'static, Message>> = agents
        .iter()
        .map(|a| row![dot(agent_color(a.state)), text(a.name.clone()).size(theme::BODY_SIZE)].spacing(3).align_y(iced::Alignment::Center).into())
        .collect();
    row(items).spacing(8).into()
}

pub fn view<Message: 'static>(snapshot: &NotchSnapshot) -> Element<'static, Message> {
    container(
        column![
            header(snapshot),
            provider_rows(snapshot),
            bench_rows(snapshot),
            activity_rows(snapshot),
            agent_dots(&snapshot.agents),
        ]
        .spacing(8)
        .padding(12),
    )
    .width(Length::Fixed(340.0))
    .height(Length::Fixed(148.0))
    .style(|_theme| container::Style {
        background: Some(iced::Background::Color(theme::FILL)),
        border: iced::Border { color: theme::HAIRLINE, width: 1.0, radius: theme::RADIUS_CARD.into() },
        ..Default::default()
    })
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_leaves_short_text_untouched() {
        assert_eq!(truncate("short", 40), "short");
    }

    #[test]
    fn truncate_clips_long_text_with_ellipsis() {
        let long = "a".repeat(50);
        let out = truncate(&long, 40);
        assert_eq!(out.chars().count(), 40);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn agent_color_maps_auth_state_to_a_real_theme_color() {
        assert_eq!(agent_color(AuthState::Authenticated), theme::TEAL);
        assert_eq!(agent_color(AuthState::NotAuthenticated), theme::RED);
        assert_eq!(agent_color(AuthState::Unsupported), theme::AMBER);
    }
}

use crate::config::AppConfig;
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

/// Temporary placeholder that logs high-level configuration and demonstrates the TUI widget stack.
pub fn print_placeholder_overview(config: &AppConfig) {
    let widget = placeholder_widget(config);
    tracing::debug!(?widget, "TUI placeholder widget composed");

    println!(
        "[tui] Listening on {} and proxying to {} (TUI coming soon)",
        config.server.listen_addr, config.server.upstream_base_url
    );
}

fn placeholder_widget(config: &AppConfig) -> Paragraph<'static> {
    let lines = vec![
        Line::from(Span::styled(
            "codex-usage-proxy",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(format!(
            "Listen: {}   Upstream: {}",
            config.server.listen_addr, config.server.upstream_base_url
        )),
        Line::from("Usage dashboard coming soon..."),
    ];

    Paragraph::new(lines).block(
        Block::default()
            .title("Usage Dashboard (scaffold)")
            .borders(Borders::ALL),
    )
}

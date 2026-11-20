//! Rendering.

use crate::app::{App, Phase, HOLD_TO_READY, INSPECTION, INSPECTION_DNF};
use crate::big;
use crate::scramble::Face;
use crate::stats::{self, fmt_avg, fmt_cs, fmt_solve};
use crate::store::Penalty;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};
use ratatui::Frame;
use std::time::Instant;

const WHITE: Color = Color::White;
const RED: Color = Color::Red;
const GREEN: Color = Color::Green;
const DIM: Color = Color::DarkGray;

pub fn draw(f: &mut Frame, app: &App) {
    let now = Instant::now();
    let [main, footer] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(f.area());

    let center = if main.width >= 75 {
        let [left, center] =
            Layout::horizontal([Constraint::Length(30), Constraint::Min(1)]).areas(main);
        draw_left(f, app, left);
        center
    } else {
        main
    };
    draw_center(f, app, center, now);
    draw_footer(f, app, footer);
}

// ------------------------------------------------------------------- left side

fn draw_left(f: &mut Frame, app: &App, area: Rect) {
    const NET_H: u16 = 13;
    let show_net = area.height >= NET_H + 7;
    let net_h = if show_net { NET_H } else { 0 };
    let res_h = (area.height - net_h).min(14);

    let [net, _, results] = Layout::vertical([
        Constraint::Length(net_h),
        Constraint::Min(0),
        Constraint::Length(res_h),
    ])
    .areas(area);

    if show_net {
        draw_net(f, app, net);
    }
    draw_results(f, app, results);
}

fn sticker_color(face: Face) -> Color {
    match face {
        Face::U => Color::Indexed(231), // white
        Face::D => Color::Indexed(226), // yellow
        Face::F => Color::Indexed(34),  // green
        Face::B => Color::Indexed(27),  // blue
        Face::R => Color::Indexed(196), // red
        Face::L => Color::Indexed(208), // orange
    }
}

fn draw_net(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::bordered()
        .title(" scramble ")
        .border_style(Style::default().fg(DIM));
    let inner = block.inner(area);
    f.render_widget(block, area);

    use Face::*;
    let bands: [[Option<Face>; 4]; 3] = [
        [None, Some(U), None, None],
        [Some(L), Some(F), Some(R), Some(B)],
        [None, Some(D), None, None],
    ];

    let mut lines: Vec<Line> = Vec::new();
    for (bi, band) in bands.iter().enumerate() {
        if bi > 0 {
            lines.push(Line::raw(""));
        }
        let grids: Vec<_> = band.iter().map(|face| face.map(|x| app.cube.face_grid(x))).collect();
        for r in 0..3 {
            let mut spans: Vec<Span> = Vec::new();
            for (ci, grid) in grids.iter().enumerate() {
                if ci > 0 {
                    spans.push(Span::raw(" "));
                }
                match grid {
                    Some(g) => {
                        for c in 0..3 {
                            spans.push(Span::styled(
                                "  ",
                                Style::default().bg(sticker_color(g[r][c])),
                            ));
                        }
                    }
                    None => spans.push(Span::raw("      ")),
                }
            }
            lines.push(Line::from(spans));
        }
    }
    f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), inner);
}

fn draw_results(f: &mut Frame, app: &App, area: Rect) {
    let n = app.solves.len();
    let block = Block::bordered()
        .title(format!(" results ({n}) "))
        .border_style(Style::default().fg(DIM));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 {
        return;
    }

    let mut lines: Vec<Line> = vec![Line::styled(
        format!("{:>5} {:>9} {:>9}", "#", "time", "ao5"),
        Style::default().fg(DIM),
    )];

    let visible = (inner.height as usize).saturating_sub(1);
    let offset = if n <= visible {
        0
    } else {
        app.selected.saturating_sub(visible / 2).min(n - visible)
    };

    for i in offset..(offset + visible).min(n) {
        let s = &app.solves[i];
        let mut style = Style::default();
        if s.penalty == Penalty::Dnf {
            style = style.fg(RED);
        } else if app.stats.best_single == Some(i) {
            style = style.fg(GREEN);
        }
        if i == app.selected {
            style = style.add_modifier(Modifier::REVERSED);
        }
        let text = format!(
            "{:>5} {:>9} {:>9}",
            i + 1,
            fmt_solve(s),
            fmt_avg(app.stats.ao5[i])
        );
        lines.push(Line::styled(text, style));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

// ---------------------------------------------------------------------- center

/// What the big timer shows and in which colour.
fn timer_face(app: &App, now: Instant) -> (String, Color) {
    let countdown = |start: Instant| -> String {
        let e = now.saturating_duration_since(start);
        if e < INSPECTION {
            format!("{}", 15 - e.as_secs())
        } else if e <= INSPECTION_DNF {
            "+2".to_string()
        } else {
            "DNF".to_string()
        }
    };

    match app.phase {
        Phase::Idle => match app.selected_solve() {
            None => ("0.00".into(), WHITE),
            Some(s) if s.penalty == Penalty::Dnf => ("DNF".into(), RED),
            Some(s) => {
                let record = app.stats.best_single == Some(app.selected)
                    || app.stats.best_ao5 == Some(app.selected);
                (fmt_solve(s), if record { GREEN } else { WHITE })
            }
        },
        Phase::Inspecting { start } => (countdown(start), RED),
        Phase::Holding { since, inspect_start } => {
            if now.saturating_duration_since(since) >= HOLD_TO_READY {
                ("0.00".into(), GREEN)
            } else {
                match inspect_start {
                    Some(start) => (countdown(start), RED),
                    None => ("0.00".into(), RED),
                }
            }
        }
        Phase::Running { .. } => ("SOLVING".into(), WHITE),
    }
}

fn draw_center(f: &mut Frame, app: &App, area: Rect, now: Instant) {
    let [scramble, timer, badge, stats_area, info] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(3),
    ])
    .areas(area);

    // scramble text
    let lines = vec![
        Line::raw(""),
        Line::styled(
            app.scramble.clone(),
            Style::default().fg(WHITE).add_modifier(Modifier::BOLD),
        ),
    ];
    f.render_widget(
        Paragraph::new(lines).alignment(Alignment::Center).wrap(Wrap { trim: true }),
        scramble,
    );

    // big timer, centred in its area
    let (text, color) = timer_face(app, now);
    let big_lines = big::render(&text, color, timer.width, timer.height);
    let h = (big_lines.len() as u16).min(timer.height);
    let rect = Rect {
        x: timer.x,
        y: timer.y + (timer.height - h) / 2,
        width: timer.width,
        height: h,
    };
    f.render_widget(Paragraph::new(big_lines).alignment(Alignment::Center), rect);

    // badge / hint line
    let (msg, style) = badge_line(app, now);
    f.render_widget(
        Paragraph::new(Line::styled(msg, style)).alignment(Alignment::Center),
        badge,
    );

    // statistics (hidden while solving)
    if !matches!(app.phase, Phase::Running { .. }) {
        f.render_widget(
            Paragraph::new(stats_lines(app)).alignment(Alignment::Center),
            stats_area,
        );
        f.render_widget(
            Paragraph::new(info_lines(app))
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true }),
            info,
        );
    }
}

fn badge_line(app: &App, now: Instant) -> (String, Style) {
    if let Some(t) = app.active_toast() {
        return (t.to_string(), Style::default().fg(Color::Yellow));
    }
    let green = Style::default().fg(GREEN).add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(DIM);
    match app.phase {
        Phase::Idle => {
            if app.stats.best_single == Some(app.selected) && app.solves.len() > 1 {
                ("★ best single ★".into(), green)
            } else if app.stats.best_ao5 == Some(app.selected) {
                ("★ best ao5 ★".into(), green)
            } else {
                (String::new(), dim)
            }
        }
        Phase::Inspecting { .. } => (
            if app.hold_mode {
                "hold space, release to start".into()
            } else {
                "press space to start".into()
            },
            dim,
        ),
        Phase::Holding { since, .. } => {
            if now.saturating_duration_since(since) >= HOLD_TO_READY {
                ("release to start".into(), green)
            } else {
                ("keep holding…".into(), dim)
            }
        }
        Phase::Running { .. } => ("space: stop    esc: discard".into(), dim),
    }
}

fn stats_lines(app: &App) -> Vec<Line<'static>> {
    let dim = Style::default().fg(DIM);
    let sel = app.selected;
    let ao12 = if app.solves.is_empty() { None } else { stats::ao(&app.solves, sel, 12) };
    let ao100 = if app.solves.is_empty() { None } else { stats::ao(&app.solves, sel, 100) };
    let ao5 = app.stats.ao5.get(sel).copied().flatten();
    let mean = stats::mean(&app.solves).map(fmt_cs).unwrap_or_else(|| "-".into());

    let best = app
        .stats
        .best_single
        .map(|i| fmt_solve(&app.solves[i]))
        .unwrap_or_else(|| "-".into());
    let best_ao5 = app
        .stats
        .best_ao5
        .map(|i| fmt_avg(app.stats.ao5[i]))
        .unwrap_or_else(|| "-".into());

    vec![
        Line::from(vec![
            Span::styled("ao5 ", dim),
            Span::raw(fmt_avg(ao5)),
            Span::styled("   ao12 ", dim),
            Span::raw(fmt_avg(ao12)),
            Span::styled("   ao100 ", dim),
            Span::raw(fmt_avg(ao100)),
            Span::styled("   mean ", dim),
            Span::raw(mean),
        ]),
        Line::from(vec![
            Span::styled("best ", dim),
            Span::raw(best),
            Span::styled("   best ao5 ", dim),
            Span::raw(best_ao5),
        ]),
    ]
}

fn info_lines(app: &App) -> Vec<Line<'static>> {
    let dim = Style::default().fg(DIM);
    let Some(s) = app.selected_solve() else {
        return vec![Line::styled("no solves yet - press space to start", dim)];
    };
    let raw = match s.penalty {
        Penalty::None => fmt_cs(s.raw_cs()),
        Penalty::Plus2 => format!("{} +2", fmt_cs(s.raw_cs())),
        Penalty::Dnf => format!("DNF ({})", fmt_cs(s.raw_cs())),
    };
    vec![
        Line::from(""),
        Line::styled(format!("#{}  {}   {}", app.selected + 1, raw, s.scramble), dim),
    ]
}

// ---------------------------------------------------------------------- footer

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let key = Style::default().fg(Color::Yellow);
    let dim = Style::default().fg(DIM);
    let items: [(&str, &str); 9] = [
        ("space", "start/stop"),
        ("hjkl/←↓↑→", "select"),
        ("2", "+2"),
        ("d", "dnf"),
        ("del", "delete"),
        ("u", "undo"),
        ("r", "rescramble"),
        ("y", "copy"),
        ("q", "quit"),
    ];
    let mut spans: Vec<Span> = Vec::new();
    for (k, v) in items {
        spans.push(Span::styled(k, key));
        spans.push(Span::styled(format!(" {v}  "), dim));
    }
    if !app.hold_mode {
        spans.push(Span::styled("[tap mode]", Style::default().fg(Color::Yellow)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Solve, Store};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn solves(n: usize) -> Vec<Solve> {
        (0..n)
            .map(|i| {
                let mut s = Solve::new(9_000 + (i as u64 * 337) % 4000, Penalty::None, "R U".into());
                if i % 7 == 3 {
                    s.penalty = Penalty::Plus2;
                }
                if i % 11 == 5 {
                    s.penalty = Penalty::Dnf;
                }
                s
            })
            .collect()
    }

    #[test]
    fn renders_at_many_sizes_without_panicking() {
        for n in [0, 1, 6, 80] {
            for (w, h) in [(200, 60), (120, 40), (100, 30), (80, 24), (60, 20), (40, 10), (10, 3), (1, 1)] {
                let app = App::new(Store::memory(), solves(n), true, true);
                let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
                t.draw(|f| draw(f, &app)).unwrap();
            }
        }
    }

    #[test]
    fn timer_colours_follow_state() {
        let t0 = Instant::now();
        let mut app = App::new(Store::memory(), solves(0), true, true);
        assert_eq!(timer_face(&app, t0), ("0.00".to_string(), WHITE));

        app.phase = Phase::Inspecting { start: t0 };
        assert_eq!(timer_face(&app, t0 + std::time::Duration::from_millis(100)), ("15".to_string(), RED));
        assert_eq!(timer_face(&app, t0 + std::time::Duration::from_secs(16)).0, "+2");
        assert_eq!(timer_face(&app, t0 + std::time::Duration::from_secs(18)).0, "DNF");

        app.phase = Phase::Holding { since: t0, inspect_start: Some(t0) };
        assert_eq!(timer_face(&app, t0 + std::time::Duration::from_millis(100)).1, RED);
        assert_eq!(timer_face(&app, t0 + std::time::Duration::from_millis(400)).1, GREEN);

        app.phase = Phase::Running { start: t0, penalty: Penalty::None };
        assert_eq!(timer_face(&app, t0), ("SOLVING".to_string(), WHITE));
    }

    #[test]
    fn idle_colours_for_dnf_and_records() {
        let mut v = solves(0);
        for ms in [12_000, 11_000, 10_000] {
            v.push(Solve::new(ms, Penalty::None, String::new()));
        }
        let mut app = App::new(Store::memory(), v, true, true);
        assert_eq!(timer_face(&app, Instant::now()).1, GREEN); // newest is best
        app.selected = 0;
        assert_eq!(timer_face(&app, Instant::now()).1, WHITE);
        app.solves[0].penalty = Penalty::Dnf;
        assert_eq!(timer_face(&app, Instant::now()), ("DNF".to_string(), RED));
    }
}

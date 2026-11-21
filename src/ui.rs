//! Rendering.
//!
//! Screen layout (main area above the one-line footer):
//!
//! ```text
//!                 scramble text (centred)
//!                       big timer
//!  .- results ---.   badge / ao5 ... / info   .- scramble -.
//!  | last 5 solves |                          |  cube net   |
//!  '--------------'                          '------------'
//! ```
//!
//! The two panels in the bottom row have the same height: just enough for the
//! results header plus five solves. Older results are reached by moving the
//! selection (the list scrolls with it).

use crate::app::{App, Phase, HOLD_TO_READY, INSPECTION, INSPECTION_DNF};
use crate::big;
use crate::cube::Cube;
use crate::scramble::Face;
use crate::stats::{self, fmt_avg, fmt_cs, fmt_solve};
use crate::store::Penalty;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};
use ratatui::Frame;
use std::time::Instant;

const WHITE: Color = Color::White;
const RED: Color = Color::Red;
const GREEN: Color = Color::Green;
const DIM: Color = Color::DarkGray;

/// Rows of the scramble text above the timer (one blank, then the moves; the
/// moves wrap onto the blank row's neighbour on narrow windows).
const SCRAMBLE_H: u16 = 3;
/// Solves visible in the results panel at once.
const VISIBLE_SOLVES: u16 = 5;
/// Height of both bottom panels: borders (2) + header (1) + visible solves.
const BOTTOM_H: u16 = VISIBLE_SOLVES + 3;
const RESULTS_W: u16 = 30;
/// The cube net is drawn with half blocks: one column and half a text row per
/// sticker, which is about square. 4 faces of 3 + 3 gaps wide; 3 faces of 3
/// + 2 gaps tall.
const NET_COLS: usize = 15;
const NET_ROWS: usize = 11;
const NET_W: u16 = NET_COLS as u16 + 2;
/// Smallest timer area kept free when the bottom row has to shrink.
const MIN_TIMER_H: u16 = 7;
/// Results panel appears from this width on, the net from `SHOW_NET_MIN_W`
/// (the statistics line in the middle needs about 57 columns).
const SHOW_RESULTS_MIN_W: u16 = 75;
const SHOW_NET_MIN_W: u16 = 104;
/// Width of the help window.
const HELP_W: u16 = 70;

/// Rectangles of every part of the main screen.
struct Parts {
    scramble: Rect,
    timer: Rect,
    results: Option<Rect>,
    /// Between the two panels: badge, statistics, info.
    middle: Rect,
    net: Option<Rect>,
}

fn layout(main: Rect) -> Parts {
    let bottom_h = BOTTOM_H.min(main.height.saturating_sub(SCRAMBLE_H + MIN_TIMER_H));
    let [scramble, timer, bottom] = Layout::vertical([
        Constraint::Length(SCRAMBLE_H.min(main.height)),
        Constraint::Min(0),
        Constraint::Length(bottom_h),
    ])
    .areas(main);

    let show_results = main.width >= SHOW_RESULTS_MIN_W && bottom_h >= 4;
    let show_net = main.width >= SHOW_NET_MIN_W && bottom_h >= BOTTOM_H;
    let mut constraints = Vec::new();
    if show_results {
        constraints.push(Constraint::Length(RESULTS_W));
    }
    constraints.push(Constraint::Min(0));
    if show_net {
        constraints.push(Constraint::Length(NET_W));
    }
    let cols = Layout::horizontal(constraints).split(bottom);

    let mut i = 0;
    let results = show_results.then(|| {
        i += 1;
        cols[0]
    });
    let middle = cols[i];
    let net = show_net.then(|| cols[i + 1]);
    Parts { scramble, timer, results, middle, net }
}

pub fn draw(f: &mut Frame, app: &App) {
    let now = Instant::now();
    let [main, footer] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(f.area());
    let parts = layout(main);

    draw_scramble_text(f, app, parts.scramble);
    draw_timer(f, app, parts.timer, now);
    if let Some(r) = parts.results {
        draw_results(f, app, r);
    }
    draw_middle(f, app, parts.middle, now);
    if let Some(n) = parts.net {
        draw_net(f, app, n);
    }
    draw_footer(f, app, footer);
    if app.show_help {
        draw_help(f, app, f.area());
    }
}

// ------------------------------------------------------------------- results

fn draw_results(f: &mut Frame, app: &App, area: Rect) {
    let n = app.solves.len();
    let visible = (area.height.saturating_sub(3)) as usize; // borders + header
    let mut block = Block::bordered()
        .title(format!(" results ({n}) "))
        .border_style(Style::default().fg(DIM));
    if n > visible {
        // Older solves exist: show where the selection is.
        block = block.title_bottom(format!(" {}/{} ", app.selected + 1, n));
    }
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 {
        return;
    }

    let mut lines: Vec<Line> = vec![Line::styled(
        format!("{:>5} {:>9} {:>9}", "#", "time", "ao5"),
        Style::default().fg(DIM),
    )];

    // Visual row 0 is the newest solve (index n - 1); row n - 1 is the oldest (index 0).
    let sel_row = n.saturating_sub(1).saturating_sub(app.selected);
    let offset = if n <= visible {
        0
    } else {
        sel_row.saturating_sub(visible / 2).min(n - visible)
    };

    for row in offset..(offset + visible).min(n) {
        let i = n - 1 - row;
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
            fmt_avg(app.stats.ao5.get(i).copied().flatten())
        );
        lines.push(Line::styled(text, style));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

// ------------------------------------------------------------------ scramble

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

/// The scramble moves, centred above the timer.
fn draw_scramble_text(f: &mut Frame, app: &App, area: Rect) {
    let lines = vec![
        Line::raw(""),
        Line::styled(
            app.scramble.clone(),
            Style::default().fg(WHITE).add_modifier(Modifier::BOLD),
        ),
    ];
    f.render_widget(
        Paragraph::new(lines).alignment(Alignment::Center).wrap(Wrap { trim: true }),
        area,
    );
}

/// The cube net (U / L F R B / D) as half-block lines: every text row carries
/// two sticker rows, the upper one as foreground, the lower one as background.
fn net_lines(cube: &Cube) -> Vec<Line<'static>> {
    use Face::*;
    let bands: [[Option<Face>; 4]; 3] = [
        [None, Some(U), None, None],
        [Some(L), Some(F), Some(R), Some(B)],
        [None, Some(D), None, None],
    ];

    // sticker grid, bands 4 rows apart (3 + a gap row), faces 4 columns apart
    let mut grid: Vec<Vec<Option<Face>>> = vec![vec![None; NET_COLS]; NET_ROWS];
    for (bi, band) in bands.iter().enumerate() {
        for (ci, face) in band.iter().enumerate() {
            if let Some(face) = face {
                let g = cube.face_grid(*face);
                for r in 0..3 {
                    for c in 0..3 {
                        grid[bi * 4 + r][ci * 4 + c] = Some(g[r][c]);
                    }
                }
            }
        }
    }

    (0..NET_ROWS.div_ceil(2))
        .map(|tr| {
            let spans: Vec<Span<'static>> = (0..NET_COLS)
                .map(|x| {
                    let top = grid[2 * tr][x];
                    let bottom = grid.get(2 * tr + 1).and_then(|row| row[x]);
                    match (top, bottom) {
                        (Some(t), Some(b)) => Span::styled(
                            "▀",
                            Style::default().fg(sticker_color(t)).bg(sticker_color(b)),
                        ),
                        (Some(t), None) => Span::styled("▀", Style::default().fg(sticker_color(t))),
                        (None, Some(b)) => Span::styled("▄", Style::default().fg(sticker_color(b))),
                        (None, None) => Span::raw(" "),
                    }
                })
                .collect();
            Line::from(spans)
        })
        .collect()
}

fn draw_net(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::bordered()
        .title(" scramble ")
        .border_style(Style::default().fg(DIM));
    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(
        Paragraph::new(net_lines(&app.cube)).alignment(Alignment::Center),
        inner,
    );
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

fn draw_timer(f: &mut Frame, app: &App, timer: Rect, now: Instant) {
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
}

/// Badge / hint line, statistics and the selected solve's details, between the
/// two bottom panels (statistics and details hidden while solving).
fn draw_middle(f: &mut Frame, app: &App, area: Rect, now: Instant) {
    let [badge, stats_area, info, _] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(3),
        Constraint::Min(0),
    ])
    .areas(area);

    let (msg, style) = badge_line(app, now);
    f.render_widget(
        Paragraph::new(Line::styled(msg, style)).alignment(Alignment::Center),
        badge,
    );

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
        .and_then(|i| app.solves.get(i))
        .map(fmt_solve)
        .unwrap_or_else(|| "-".into());
    let best_ao5 = app
        .stats
        .best_ao5
        .map(|i| fmt_avg(app.stats.ao5.get(i).copied().flatten()))
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

/// The key bindings live in the help window; the footer only says where.
fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let yellow = Style::default().fg(Color::Yellow);
    let mut spans = vec![Span::styled("?", yellow), Span::styled(" help", Style::default().fg(DIM))];
    if !app.hold_mode {
        spans.push(Span::styled("   [tap mode]", yellow));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

// ------------------------------------------------------------------------ help

fn help_lines(app: &App) -> Vec<Line<'static>> {
    let head = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    let key = Style::default().fg(Color::Yellow);
    let dim = Style::default().fg(DIM);

    let row = |k: &str, d: &str| -> Line<'static> {
        Line::from(vec![
            Span::styled(format!(" {k:<20}"), key),
            Span::raw(d.to_string()),
        ])
    };
    let heading = |t: &str| -> Line<'static> { Line::styled(format!(" {t}"), head) };

    let mut v: Vec<Line<'static>> = vec![heading("Timer")];
    if app.hold_mode {
        v.push(row("space", "inspect, hold until green, release to start"));
        v.push(row("", "press space again to stop"));
    } else {
        v.push(row("space", "inspect, then tap to start, tap to stop"));
    }
    v.push(row("Esc", "cancel inspection / discard a running solve"));

    v.push(Line::raw(""));
    v.push(heading("Results"));
    v.push(row("h j k l / arrows", "select: j l = older, k h = newer"));
    v.push(row("g / G, Home / End", "newest / oldest result"));
    v.push(row("2", "toggle +2 on the selected result"));
    v.push(row("d", "toggle DNF on the selected result"));
    v.push(row("Delete, x", "delete the selected result"));
    v.push(row("u", "undo the last delete"));

    v.push(Line::raw(""));
    v.push(heading("Scramble"));
    v.push(row("r", "new scramble"));
    v.push(row("y", "copy the scramble"));
    v.push(row("Y", "copy the selected result's scramble"));

    v.push(Line::raw(""));
    v.push(heading("General"));
    v.push(row("? / F1", "show / hide this help"));
    v.push(row("q", "quit (Ctrl-C works anywhere)"));

    v.push(Line::raw(""));
    v.push(heading("Timer colours"));
    v.push(Line::from(vec![
        Span::raw(" "),
        Span::styled("white", Style::default().fg(WHITE)),
        Span::styled(" idle   ", dim),
        Span::styled("red", Style::default().fg(RED)),
        Span::styled(" inspection / DNF   ", dim),
        Span::styled("green", Style::default().fg(GREEN)),
        Span::styled(" ready / best", dim),
    ]));

    v.push(Line::raw(""));
    v.push(Line::styled(
        if app.hold_mode {
            " hold mode: key releases are reported"
        } else {
            " tap mode: this terminal sends no key releases (--tap forces it)"
        },
        dim,
    ));
    v
}

/// Centered pop-up with all key bindings. Scrolls with j/k when the window is
/// too small to show everything.
fn draw_help(f: &mut Frame, app: &App, area: Rect) {
    let lines = help_lines(app);
    let w = HELP_W.min(area.width);
    let h = (lines.len() as u16 + 2).min(area.height);
    if w < 3 || h < 3 {
        return;
    }
    let rect = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    };
    f.render_widget(Clear, rect);
    let block = Block::bordered()
        .title(" help ")
        .title_bottom(" ? / esc close   j k scroll ")
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let max_scroll = (lines.len() as u16).saturating_sub(inner.height);
    let scroll = app.help_scroll.get().min(max_scroll);
    app.help_scroll.set(scroll);
    f.render_widget(Paragraph::new(lines).scroll((scroll, 0)), inner);
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

    const SIZES: [(u16, u16); 14] = [
        (250, 60),
        (200, 60),
        (130, 40),
        (120, 40),
        (104, 30),
        (100, 20),
        (80, 24),
        (74, 50),
        (60, 20),
        (40, 10),
        (10, 3),
        (3, 3),
        (1, 1),
        (0, 0),
    ];

    #[test]
    fn renders_at_many_sizes_without_panicking() {
        for n in [0, 1, 6, 80] {
            for (w, h) in SIZES {
                let mut app = App::new(Store::memory(), solves(n), true, true);
                let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
                t.draw(|f| draw(f, &app)).unwrap();
                app.hold_mode = false;
                t.draw(|f| draw(f, &app)).unwrap();
            }
        }
    }

    #[test]
    fn help_renders_at_many_sizes_and_clamps_scroll() {
        for (w, h) in SIZES {
            for hold in [true, false] {
                let mut app = App::new(Store::memory(), solves(3), hold, true);
                app.show_help = true;
                app.help_scroll.set(60);
                let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
                t.draw(|f| draw(f, &app)).unwrap();
                // never scrolled past the end of the content (when the
                // window is big enough to be drawn at all)
                if w >= 3 && h >= 3 {
                    let lines = help_lines(&app).len() as u16;
                    assert!(app.help_scroll.get() <= lines, "{w}x{h}");
                }
            }
        }
    }

    fn screen(app: &App, w: u16, h: u16) -> String {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f, app)).unwrap();
        let buf = t.backend().buffer();
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn key_bindings_only_appear_in_help() {
        let mut app = App::new(Store::memory(), solves(5), true, true);
        let main = screen(&app, 120, 40);
        assert!(main.contains("? help"));
        for hidden in ["rescramble", "start/stop", "undo", "delete"] {
            assert!(!main.contains(hidden), "{hidden:?} still on the main screen");
        }
        app.show_help = true;
        let help = screen(&app, 120, 40);
        for shown in ["Timer", "Results", "Scramble", "toggle DNF", "undo the last delete", "quit"] {
            assert!(help.contains(shown), "{shown:?} missing from help");
        }
    }

    #[test]
    fn bottom_panels_share_height_and_sit_on_the_bottom_row() {
        for w in [0u16, 1, 40, 74, 75, 96, 103, 104, 105, 150, 250] {
            for h in [0u16, 1, 5, 9, 10, 12, 14, 17, 18, 24, 40, 80] {
                let main = Rect { x: 2, y: 1, width: w, height: h };
                let p = layout(main);
                let inside = |r: Rect| {
                    r.x >= main.x
                        && r.y >= main.y
                        && r.right() <= main.right()
                        && r.bottom() <= main.bottom()
                };
                for r in [Some(p.scramble), Some(p.timer), p.results, Some(p.middle), p.net]
                    .into_iter()
                    .flatten()
                {
                    assert!(inside(r), "{w}x{h}: {r:?} outside");
                }
                if let Some(r) = p.results {
                    assert_eq!(r.x, main.x);
                    assert_eq!(r.bottom(), main.bottom(), "{w}x{h}");
                    assert!(r.height <= BOTTOM_H);
                }
                if let Some(n) = p.net {
                    assert_eq!(n.right(), main.right());
                    assert_eq!(n.bottom(), main.bottom(), "{w}x{h}");
                    assert_eq!(n.height, BOTTOM_H);
                }
                if let (Some(r), Some(n)) = (p.results, p.net) {
                    assert_eq!((r.y, r.height), (n.y, n.height), "{w}x{h}: panels differ");
                }
                // the scramble text is always on top of the timer
                assert_eq!(p.scramble.y, main.y);
                assert!(p.scramble.bottom() <= p.timer.y || p.timer.height == 0);
            }
        }
    }

    #[test]
    fn results_panel_shows_five_solves_and_scrolls_with_the_selection() {
        let mut app = App::new(Store::memory(), solves(80), true, true);
        let top = screen(&app, 120, 40);
        // newest five are visible, with the latest on top...
        for n in 76..=80 {
            assert!(top.contains(&format!("│{n:>5} ")), "#{n} missing");
        }
        assert!(top.find("│   80 ").unwrap() < top.find("│   76 ").unwrap());
        // ...and nothing older
        assert!(!top.contains(&format!("│{:>5} ", 75)));
        assert!(top.contains(" 80/80 "));

        app.selected = 0;
        let first = screen(&app, 120, 40);
        for n in 1..=5 {
            assert!(first.contains(&format!("│{n:>5} ")), "#{n} missing");
        }
        assert!(first.find("│    5 ").unwrap() < first.find("│    1 ").unwrap());
        assert!(!first.contains(&format!("│{:>5} ", 6)));
        assert!(!first.contains(&format!("│{:>5} ", 80)));
        assert!(first.contains(" 1/80 "));
    }

    #[test]
    fn results_panel_height_is_exactly_header_plus_five() {
        let app = App::new(Store::memory(), solves(80), true, true);
        for (w, h) in [(120u16, 40u16), (160, 60), (80, 24)] {
            let text = screen(&app, w, h);
            let rows: Vec<&str> = text.lines().collect();
            let top = rows.iter().position(|r| r.contains("results (80)")).unwrap();
            let bottom = rows.iter().rposition(|r| r.starts_with('└')).unwrap();
            assert_eq!(bottom - top + 1, BOTTOM_H as usize, "{w}x{h}");
        }
    }

    #[test]
    fn scramble_text_is_centred_above_the_timer() {
        let app = App::new(Store::memory(), solves(3), true, true);
        for w in [100u16, 120, 161] {
            let text = screen(&app, w, 40);
            let rows: Vec<&str> = text.lines().collect();
            let row = rows[1]; // blank row first, then the moves
            assert_eq!(row.trim(), app.scramble, "{w}: scramble not on the second row");
            let left = row.len() - row.trim_start().len();
            let right = row.chars().count() - row.trim_end().chars().count();
            assert!(left.abs_diff(right) <= 1, "{w}: not centred ({left} vs {right})");
        }
    }

    #[test]
    fn net_is_compact_and_correct() {
        let solved = Cube::solved();
        let lines = net_lines(&solved);
        assert_eq!(lines.len() as u16 + 2, BOTTOM_H); // fits the shared panel height
        for l in &lines {
            assert_eq!(l.spans.len(), NET_COLS);
        }
        // U face (columns 4..=6) is white; rows 0-1 are two stickers per cell
        for x in 4..=6 {
            let s = &lines[0].spans[x];
            assert_eq!(s.content, "▀");
            assert_eq!(s.style.fg, Some(sticker_color(Face::U)));
            assert_eq!(s.style.bg, Some(sticker_color(Face::U)));
        }
        // third U row sits above the gap row: only a foreground half block
        assert_eq!(lines[1].spans[4].style.fg, Some(sticker_color(Face::U)));
        assert_eq!(lines[1].spans[4].style.bg, None);
        // gaps between faces stay empty
        assert_eq!(lines[2].spans[3].content, " ");
        // middle band: L F R B left to right
        for (x, face) in [(0, Face::L), (4, Face::F), (8, Face::R), (12, Face::B)] {
            assert_eq!(lines[2].spans[x].style.fg, Some(sticker_color(face)));
        }
        // D face at the bottom
        assert_eq!(lines[5].spans[5].style.fg, Some(sticker_color(Face::D)));
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

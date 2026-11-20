//! Big block-letter rendering for the main timer.
//!
//! Glyphs are 7 pixels tall. The renderer picks the biggest scale that fits:
//!   * scale 2: every pixel = 4x2 cells  (14 rows tall)
//!   * scale 1: every pixel = 2x1 cells  (7 rows tall)
//!   * compact: half-block characters    (4 rows tall)

use ratatui::style::{Color, Style};
use ratatui::text::Line;

const H: usize = 7;

fn glyph(c: char) -> [&'static str; H] {
    match c {
        '0' => [".###.", "#...#", "#..##", "#.#.#", "##..#", "#...#", ".###."],
        '1' => ["..#..", ".##..", "..#..", "..#..", "..#..", "..#..", ".###."],
        '2' => [".###.", "#...#", "....#", "...#.", "..#..", ".#...", "#####"],
        '3' => [".###.", "#...#", "....#", "..##.", "....#", "#...#", ".###."],
        '4' => ["...#.", "..##.", ".#.#.", "#..#.", "#####", "...#.", "...#."],
        '5' => ["#####", "#....", "####.", "....#", "....#", "#...#", ".###."],
        '6' => ["..##.", ".#...", "#....", "####.", "#...#", "#...#", ".###."],
        '7' => ["#####", "....#", "...#.", "..#..", ".#...", ".#...", ".#..."],
        '8' => [".###.", "#...#", "#...#", ".###.", "#...#", "#...#", ".###."],
        '9' => [".###.", "#...#", "#...#", ".####", "....#", "...#.", ".##.."],
        '.' => ["..", "..", "..", "..", "..", "##", "##"],
        ':' => ["..", "..", "##", "..", "##", "..", ".."],
        '+' => [".....", "..#..", "..#..", "#####", "..#..", "..#..", "....."],
        'S' => [".####", "#....", "#....", ".###.", "....#", "....#", "####."],
        'O' => [".###.", "#...#", "#...#", "#...#", "#...#", "#...#", ".###."],
        'L' => ["#....", "#....", "#....", "#....", "#....", "#....", "#####"],
        'V' => ["#...#", "#...#", "#...#", "#...#", "#...#", ".#.#.", "..#.."],
        'I' => ["###", ".#.", ".#.", ".#.", ".#.", ".#.", "###"],
        'N' => ["#...#", "##..#", "##..#", "#.#.#", "#..##", "#..##", "#...#"],
        'G' => [".###.", "#...#", "#....", "#.###", "#...#", "#...#", ".###."],
        'D' => ["####.", "#...#", "#...#", "#...#", "#...#", "#...#", "####."],
        'F' => ["#####", "#....", "#....", "####.", "#....", "#....", "#...."],
        _ => ["...", "...", "...", "...", "...", "...", "..."],
    }
}

fn pixels(text: &str) -> Vec<Vec<bool>> {
    let mut rows = vec![Vec::new(); H];
    for (i, c) in text.chars().enumerate() {
        let g = glyph(c.to_ascii_uppercase());
        for (r, row) in rows.iter_mut().enumerate() {
            if i > 0 {
                row.push(false); // 1px gap between glyphs
            }
            row.extend(g[r].chars().map(|ch| ch == '#'));
        }
    }
    rows
}

pub fn render(text: &str, color: Color, max_w: u16, max_h: u16) -> Vec<Line<'static>> {
    let px = pixels(text);
    let w = px[0].len();
    let style = Style::default().fg(color);

    for k in [2usize, 1] {
        if w * 2 * k <= max_w as usize && H * k <= max_h as usize {
            let mut lines = Vec::with_capacity(H * k);
            for row in &px {
                let s: String = row
                    .iter()
                    .map(|&on| if on { "█" } else { " " }.repeat(2 * k))
                    .collect();
                for _ in 0..k {
                    lines.push(Line::styled(s.clone(), style));
                }
            }
            return lines;
        }
    }

    // compact: two pixel rows per text row
    let mut lines = Vec::with_capacity(H.div_ceil(2));
    for pair in px.chunks(2) {
        let s: String = (0..w)
            .map(|x| {
                let top = pair[0][x];
                let bottom = pair.get(1).is_some_and(|r| r[x]);
                match (top, bottom) {
                    (true, true) => '█',
                    (true, false) => '▀',
                    (false, true) => '▄',
                    (false, false) => ' ',
                }
            })
            .collect();
        lines.push(Line::styled(s, style));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyphs_are_rectangular() {
        for c in "0123456789.:+SOLVINGDF?".chars() {
            let g = glyph(c);
            assert!(g.iter().all(|r| r.len() == g[0].len()), "glyph {c}");
        }
    }

    #[test]
    fn picks_scale_by_available_space() {
        let big = render("12.34", Color::White, 200, 30);
        assert_eq!(big.len(), 14);
        let mid = render("12.34", Color::White, 60, 30);
        assert_eq!(mid.len(), 7);
        let small = render("12.34", Color::White, 30, 30);
        assert_eq!(small.len(), 4);
    }
}

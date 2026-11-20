//! Application state and the timer state machine.

use crate::clipboard;
use crate::cube::Cube;
use crate::scramble::{self, Move};
use crate::stats::Stats;
use crate::store::{Penalty, Solve, Store};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::time::{Duration, Instant};

/// How long space must be held before the timer turns green.
pub const HOLD_TO_READY: Duration = Duration::from_millis(300);
pub const INSPECTION: Duration = Duration::from_secs(15);
/// Starting after 15s costs +2, after 17s the solve is a DNF (WCA rules).
pub const INSPECTION_DNF: Duration = Duration::from_secs(17);
const TAP_DEBOUNCE: Duration = Duration::from_millis(800);
const COOLDOWN: Duration = Duration::from_millis(300);
const TOAST_TIME: Duration = Duration::from_millis(2500);
const SCRAMBLE_LEN: usize = 20;

#[derive(Clone, Copy, Debug)]
pub enum Phase {
    Idle,
    /// Inspection countdown running, space not held.
    Inspecting { start: Instant },
    /// Space is held down (only with key-release support).
    Holding { since: Instant, inspect_start: Option<Instant> },
    Running { start: Instant, penalty: Penalty },
}

pub struct App {
    pub store: Store,
    pub solves: Vec<Solve>,
    pub stats: Stats,
    pub selected: usize,
    pub phase: Phase,
    pub moves: Vec<Move>,
    pub scramble: String,
    pub cube: Cube,
    pub toast: Option<(String, Instant)>,
    pub deleted: Vec<(usize, Solve)>,
    /// True when the terminal reports key releases (kitty keyboard protocol):
    /// hold space -> green -> release to start. Otherwise "tap mode".
    pub hold_mode: bool,
    pub inspection: bool,
    pub should_quit: bool,
    last_space: Option<Instant>,
    cooldown_until: Option<Instant>,
}

impl App {
    pub fn new(store: Store, solves: Vec<Solve>, hold_mode: bool, inspection: bool) -> Self {
        let stats = Stats::compute(&solves);
        let selected = solves.len().saturating_sub(1);
        let mut app = App {
            store,
            solves,
            stats,
            selected,
            phase: Phase::Idle,
            moves: Vec::new(),
            scramble: String::new(),
            cube: Cube::solved(),
            toast: None,
            deleted: Vec::new(),
            hold_mode,
            inspection,
            should_quit: false,
            last_space: None,
            cooldown_until: None,
        };
        app.new_scramble();
        app
    }

    // ---------------------------------------------------------------- helpers

    pub fn selected_solve(&self) -> Option<&Solve> {
        self.solves.get(self.selected)
    }

    pub fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    pub fn active_toast(&self) -> Option<&str> {
        self.toast
            .as_ref()
            .filter(|(_, t)| t.elapsed() < TOAST_TIME)
            .map(|(m, _)| m.as_str())
    }

    pub fn tick_interval(&self) -> Duration {
        match self.phase {
            Phase::Idle => Duration::from_millis(250),
            Phase::Running { .. } => Duration::from_millis(100),
            _ => Duration::from_millis(40),
        }
    }

    pub fn on_tick(&mut self, _now: Instant) {
        if self.toast.as_ref().is_some_and(|(_, t)| t.elapsed() >= TOAST_TIME) {
            self.toast = None;
        }
    }

    fn new_scramble(&mut self) {
        self.moves = scramble::generate(&mut rand::thread_rng(), SCRAMBLE_LEN);
        self.scramble = scramble::to_string(&self.moves);
        self.cube = Cube::solved();
        self.cube.apply_all(&self.moves);
    }

    /// Called after every change to `solves`: refresh stats and persist.
    fn changed(&mut self) {
        self.stats = Stats::compute(&self.solves);
        self.selected = self.selected.min(self.solves.len().saturating_sub(1));
        if let Err(e) = self.store.save(&self.solves) {
            self.toast(format!("SAVE FAILED: {e}"));
        }
    }

    // ------------------------------------------------------------ key handling

    pub fn on_key(&mut self, key: KeyEvent, now: Instant) {
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c'))
            && key.kind != KeyEventKind::Release
        {
            self.should_quit = true;
            return;
        }
        match (key.code, key.kind) {
            (KeyCode::Char(' '), KeyEventKind::Press) => self.space_down(now),
            (KeyCode::Char(' '), KeyEventKind::Release) => self.space_up(now),
            (KeyCode::Char(' '), KeyEventKind::Repeat) => {}
            (_, KeyEventKind::Release) => {}
            (_, kind) => self.command(key, kind == KeyEventKind::Repeat, now),
        }
    }

    fn space_down(&mut self, now: Instant) {
        // Without release events a held key shows up as a stream of presses.
        // Ignore anything arriving shortly after the previous space event.
        if !self.hold_mode {
            let too_soon = self
                .last_space
                .is_some_and(|t| now.saturating_duration_since(t) < TAP_DEBOUNCE);
            self.last_space = Some(now);
            if too_soon {
                return;
            }
        }

        match self.phase {
            Phase::Idle => {
                if self.cooldown_until.is_some_and(|t| now < t) {
                    return;
                }
                if self.inspection {
                    self.phase = Phase::Inspecting { start: now };
                } else if self.hold_mode {
                    self.phase = Phase::Holding { since: now, inspect_start: None };
                } else {
                    self.start_run(None, now);
                }
            }
            Phase::Inspecting { start } => {
                if self.hold_mode {
                    self.phase = Phase::Holding { since: now, inspect_start: Some(start) };
                } else {
                    self.start_run(Some(start), now);
                }
            }
            Phase::Holding { .. } => {}
            Phase::Running { .. } => self.stop_run(now),
        }
    }

    fn space_up(&mut self, now: Instant) {
        if let Phase::Holding { since, inspect_start } = self.phase {
            if now.saturating_duration_since(since) >= HOLD_TO_READY {
                self.start_run(inspect_start, now);
            } else {
                // released too early: go back
                self.phase = match inspect_start {
                    Some(start) => Phase::Inspecting { start },
                    None => Phase::Idle,
                };
            }
        }
    }

    fn start_run(&mut self, inspect_start: Option<Instant>, now: Instant) {
        let penalty = match inspect_start.map(|s| now.saturating_duration_since(s)) {
            Some(e) if e > INSPECTION_DNF => Penalty::Dnf,
            Some(e) if e > INSPECTION => Penalty::Plus2,
            _ => Penalty::None,
        };
        self.phase = Phase::Running { start: now, penalty };
    }

    fn stop_run(&mut self, now: Instant) {
        if let Phase::Running { start, penalty } = self.phase {
            let ms = now.saturating_duration_since(start).as_millis() as u64;
            self.solves.push(Solve::new(ms, penalty, self.scramble.clone()));
            self.selected = self.solves.len() - 1;
            self.phase = Phase::Idle;
            self.cooldown_until = Some(now + COOLDOWN);
            self.new_scramble();
            self.changed();
        }
    }

    fn cancel(&mut self, now: Instant) {
        match self.phase {
            Phase::Idle => {}
            Phase::Running { .. } => {
                self.toast("solve discarded");
                self.phase = Phase::Idle;
                self.cooldown_until = Some(now + COOLDOWN);
            }
            _ => {
                self.phase = Phase::Idle;
                self.cooldown_until = Some(now + COOLDOWN);
            }
        }
    }

    /// Everything that isn't the space bar.
    fn command(&mut self, key: KeyEvent, repeat: bool, now: Instant) {
        if key.code == KeyCode::Esc {
            self.cancel(now);
            return;
        }
        let idle = matches!(self.phase, Phase::Idle);
        let inspecting = matches!(self.phase, Phase::Inspecting { .. });
        if !idle && !inspecting {
            return;
        }

        // Normalise letters: 'G' may arrive as 'G' or as 'g' + SHIFT.
        let (code, shift) = match key.code {
            KeyCode::Char(c) => (
                KeyCode::Char(c.to_ascii_lowercase()),
                key.modifiers.contains(KeyModifiers::SHIFT) || c.is_ascii_uppercase(),
            ),
            other => (other, key.modifiers.contains(KeyModifiers::SHIFT)),
        };

        // Copying is allowed during inspection too.
        match code {
            KeyCode::Char('y') if !repeat => {
                if shift {
                    if let Some(s) = self.selected_solve().map(|s| s.scramble.clone()) {
                        self.copy(s, "solve scramble");
                    }
                } else {
                    self.copy(self.scramble.clone(), "scramble");
                }
                return;
            }
            _ => {}
        }
        if !idle {
            return;
        }

        match code {
            // navigation (vim + arrows); repeats allowed so holding j scrolls
            KeyCode::Char('k') | KeyCode::Char('h') | KeyCode::Up | KeyCode::Left => {
                self.selected = self.selected.saturating_sub(1)
            }
            KeyCode::Char('j') | KeyCode::Char('l') | KeyCode::Down | KeyCode::Right => {
                self.selected = (self.selected + 1).min(self.solves.len().saturating_sub(1))
            }
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = self.solves.len().saturating_sub(1),
            KeyCode::Char('g') => {
                self.selected = if shift { self.solves.len().saturating_sub(1) } else { 0 }
            }
            _ if repeat => {}
            KeyCode::Char('2') => self.toggle_penalty(Penalty::Plus2),
            KeyCode::Char('d') if !shift => self.toggle_penalty(Penalty::Dnf),
            KeyCode::Delete | KeyCode::Char('x') => self.delete_selected(),
            KeyCode::Char('u') => self.undo_delete(),
            KeyCode::Char('r') => {
                self.new_scramble();
                self.toast("new scramble");
            }
            KeyCode::Char('q') => self.should_quit = true,
            _ => {}
        }
    }

    fn toggle_penalty(&mut self, p: Penalty) {
        if let Some(s) = self.solves.get_mut(self.selected) {
            s.penalty = if s.penalty == p { Penalty::None } else { p };
            self.changed();
        }
    }

    fn delete_selected(&mut self) {
        if self.selected < self.solves.len() {
            let s = self.solves.remove(self.selected);
            self.deleted.push((self.selected, s));
            self.changed();
            self.toast("solve deleted  (u to undo)");
        }
    }

    fn undo_delete(&mut self) {
        if let Some((idx, s)) = self.deleted.pop() {
            let idx = idx.min(self.solves.len());
            self.solves.insert(idx, s);
            self.selected = idx;
            self.changed();
            self.toast("delete undone");
        } else {
            self.toast("nothing to undo");
        }
    }

    fn copy(&mut self, text: String, what: &str) {
        match clipboard::copy(&text) {
            Ok(how) => self.toast(format!("{what} copied ({how})")),
            Err(e) => self.toast(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(c: KeyCode, kind: KeyEventKind) -> KeyEvent {
        KeyEvent::new_with_kind(c, KeyModifiers::NONE, kind)
    }
    fn press(app: &mut App, c: KeyCode, t: Instant) {
        app.on_key(key(c, KeyEventKind::Press), t);
    }
    fn release(app: &mut App, c: KeyCode, t: Instant) {
        app.on_key(key(c, KeyEventKind::Release), t);
    }
    fn ms(base: Instant, n: u64) -> Instant {
        base + Duration::from_millis(n)
    }
    const SP: KeyCode = KeyCode::Char(' ');

    fn app(hold: bool, inspection: bool) -> App {
        App::new(Store::memory(), vec![], hold, inspection)
    }

    #[test]
    fn hold_mode_full_flow() {
        let t = Instant::now();
        let mut a = app(true, true);

        press(&mut a, SP, t); // start inspection
        assert!(matches!(a.phase, Phase::Inspecting { .. }));
        release(&mut a, SP, ms(t, 80)); // releasing must not do anything
        assert!(matches!(a.phase, Phase::Inspecting { .. }));

        press(&mut a, SP, ms(t, 5000)); // hold
        assert!(matches!(a.phase, Phase::Holding { .. }));
        release(&mut a, SP, ms(t, 5100)); // too early -> back to inspecting
        assert!(matches!(a.phase, Phase::Inspecting { .. }));

        press(&mut a, SP, ms(t, 6000));
        release(&mut a, SP, ms(t, 6400)); // held long enough -> run
        assert!(matches!(a.phase, Phase::Running { penalty: Penalty::None, .. }));

        press(&mut a, SP, ms(t, 6400 + 9870)); // stop
        assert!(matches!(a.phase, Phase::Idle));
        assert_eq!(a.solves.len(), 1);
        assert_eq!(a.solves[0].time_ms, 9870);
        assert_eq!(a.solves[0].penalty, Penalty::None);
        assert!(!a.scramble.is_empty());
    }

    #[test]
    fn late_start_gets_penalty() {
        let t = Instant::now();
        let mut a = app(true, true);
        press(&mut a, SP, t);
        press(&mut a, SP, ms(t, 15_500));
        release(&mut a, SP, ms(t, 16_000)); // 16s of inspection -> +2
        assert!(matches!(a.phase, Phase::Running { penalty: Penalty::Plus2, .. }));

        let mut a = app(true, true);
        press(&mut a, SP, t);
        press(&mut a, SP, ms(t, 17_500));
        release(&mut a, SP, ms(t, 18_000)); // > 17s -> DNF
        assert!(matches!(a.phase, Phase::Running { penalty: Penalty::Dnf, .. }));
    }

    #[test]
    fn tap_mode_ignores_autorepeat() {
        let t = Instant::now();
        let mut a = app(false, true);
        press(&mut a, SP, t); // inspect
        // key held: first repeat after 500ms, then every 30ms
        press(&mut a, SP, ms(t, 500));
        for i in 1..40 {
            press(&mut a, SP, ms(t, 500 + i * 30));
        }
        assert!(matches!(a.phase, Phase::Inspecting { .. }));
        press(&mut a, SP, ms(t, 4000)); // real tap -> run
        assert!(matches!(a.phase, Phase::Running { .. }));
        press(&mut a, SP, ms(t, 14000)); // stop
        assert_eq!(a.solves.len(), 1);
        assert_eq!(a.solves[0].time_ms, 10_000);
    }

    #[test]
    fn no_inspection_hold_mode() {
        let t = Instant::now();
        let mut a = app(true, false);
        press(&mut a, SP, t);
        assert!(matches!(a.phase, Phase::Holding { .. }));
        release(&mut a, SP, ms(t, 500));
        assert!(matches!(a.phase, Phase::Running { .. }));
    }

    fn with_solves(n: usize) -> App {
        let solves = (0..n)
            .map(|i| Solve::new(10_000 + i as u64 * 100, Penalty::None, "R".into()))
            .collect();
        App::new(Store::memory(), solves, true, true)
    }

    #[test]
    fn editing_and_navigation() {
        let t = Instant::now();
        let mut a = with_solves(3);
        assert_eq!(a.selected, 2);

        press(&mut a, KeyCode::Char('k'), t);
        press(&mut a, KeyCode::Up, t);
        assert_eq!(a.selected, 0);
        press(&mut a, KeyCode::Char('k'), t);
        assert_eq!(a.selected, 0);
        press(&mut a, KeyCode::Char('j'), t);
        press(&mut a, KeyCode::Right, t);
        press(&mut a, KeyCode::Char('l'), t);
        assert_eq!(a.selected, 2);

        press(&mut a, KeyCode::Char('2'), t);
        assert_eq!(a.solves[2].penalty, Penalty::Plus2);
        press(&mut a, KeyCode::Char('2'), t);
        assert_eq!(a.solves[2].penalty, Penalty::None);
        press(&mut a, KeyCode::Char('d'), t);
        assert_eq!(a.solves[2].penalty, Penalty::Dnf);
        press(&mut a, KeyCode::Char('d'), t);
        assert_eq!(a.solves[2].penalty, Penalty::None);

        press(&mut a, KeyCode::Delete, t);
        assert_eq!(a.solves.len(), 2);
        assert_eq!(a.selected, 1);
        press(&mut a, KeyCode::Char('u'), t);
        assert_eq!(a.solves.len(), 3);
        assert_eq!(a.selected, 2);

        let old = a.scramble.clone();
        press(&mut a, KeyCode::Char('r'), t);
        // (astronomically unlikely to collide)
        assert_ne!(a.scramble, old);

        press(&mut a, KeyCode::Char('G'), t);
        assert_eq!(a.selected, 2);
        press(&mut a, KeyCode::Char('g'), t);
        assert_eq!(a.selected, 0);
    }

    #[test]
    fn edit_keys_do_nothing_while_running() {
        let t = Instant::now();
        let mut a = with_solves(2);
        press(&mut a, SP, t);
        press(&mut a, SP, ms(t, 100));
        release(&mut a, SP, ms(t, 500));
        assert!(matches!(a.phase, Phase::Running { .. }));
        press(&mut a, KeyCode::Char('d'), ms(t, 600));
        press(&mut a, KeyCode::Delete, ms(t, 600));
        press(&mut a, KeyCode::Char('q'), ms(t, 600));
        assert_eq!(a.solves.len(), 2);
        assert!(!a.should_quit);
        press(&mut a, KeyCode::Esc, ms(t, 700));
        assert!(matches!(a.phase, Phase::Idle));
        assert_eq!(a.solves.len(), 2);
    }
}

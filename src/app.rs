//! Application state and the timer state machine.

use crate::clipboard;
use crate::cube::Cube;
use crate::scramble::{self, Move};
use crate::stats::Stats;
use crate::store::{Penalty, Solve, Store};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::cell::Cell;
use std::time::{Duration, Instant};

/// How long space must be held before the timer turns green.
pub const HOLD_TO_READY: Duration = Duration::from_millis(300);
pub const INSPECTION: Duration = Duration::from_secs(15);
/// Starting after 15s costs +2, after 17s the solve is a DNF (WCA rules).
pub const INSPECTION_DNF: Duration = Duration::from_secs(17);
/// Tap mode: after a space press has been acted on, ignore further space
/// events for this long. Covers the initial auto-repeat delay of a held key
/// (typically 250-660 ms).
const TAP_DEBOUNCE: Duration = Duration::from_millis(800);
/// Tap mode: space events closer together than this are an auto-repeat stream
/// (repeat rates are ~30 ms), never two deliberate taps.
const REPEAT_GAP: Duration = Duration::from_millis(120);
/// A release this soon after its press cannot come from a human finger (even
/// the fastest tap lasts several tens of ms). Some terminals and multiplexers
/// fabricate a release right after every press; holding can never work there.
const SYNTHETIC_RELEASE: Duration = Duration::from_millis(15);
/// Letting go before the timer is green this many times in a row (within
/// `EARLY_RELEASE_WINDOW`) means holding is not working: use tap mode.
const EARLY_RELEASES_BEFORE_TAP: u8 = 2;
const EARLY_RELEASE_WINDOW: Duration = Duration::from_secs(20);
const COOLDOWN: Duration = Duration::from_millis(300);
const TOAST_TIME: Duration = Duration::from_millis(2500);
const SCRAMBLE_LEN: usize = 20;
/// Upper bound for the help window's scroll offset (the renderer clamps it to
/// what actually fits).
const HELP_SCROLL_MAX: u16 = 64;

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
    /// Key-binding help window (`?`).
    pub show_help: bool,
    /// Scroll offset of the help window. A `Cell` so the renderer can clamp it
    /// to the window size without needing `&mut App`.
    pub help_scroll: Cell<u16>,

    // --- space-bar bookkeeping -------------------------------------------
    /// Last space event of any kind (tap mode: detects auto-repeat streams).
    last_space_event: Option<Instant>,
    /// Last space press that was acted on (tap mode: debounce reference).
    last_space_accept: Option<Instant>,
    /// Hold mode: a space press has been seen and its release has not.
    space_is_down: bool,
    /// Hold mode: at least one space release has ever been delivered, i.e. the
    /// terminal really does report releases.
    releases_seen: bool,
    /// Hold mode: when the current/last space press arrived.
    last_space_press: Option<Instant>,
    /// Hold mode: consecutive releases before the timer turned green.
    early_releases: u8,
    last_early_release: Option<Instant>,
    cooldown_until: Option<Instant>,
}

/// Normalise a key: letters to lowercase + a "shift" flag, and the kitty
/// protocol's `/`+Shift to `?` (it reports the unshifted key plus a modifier).
fn normalize(key: &KeyEvent) -> (KeyCode, bool) {
    let shift_mod = key.modifiers.contains(KeyModifiers::SHIFT);
    match key.code {
        KeyCode::Char('/') if shift_mod => (KeyCode::Char('?'), true),
        KeyCode::Char(c) => (
            KeyCode::Char(c.to_ascii_lowercase()),
            shift_mod || c.is_ascii_uppercase(),
        ),
        other => (other, shift_mod),
    }
}

/// Ctrl/Alt/Super combinations are never commands (Ctrl-X must not delete a
/// solve), apart from Ctrl-C which is handled separately.
fn has_command_modifier(key: &KeyEvent) -> bool {
    key.modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
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
            show_help: false,
            help_scroll: Cell::new(0),
            last_space_event: None,
            last_space_accept: None,
            space_is_down: false,
            releases_seen: false,
            last_space_press: None,
            early_releases: 0,
            last_early_release: None,
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
        if self.show_help {
            self.help_key(key, now);
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

    /// Keys while the help window is open: close or scroll, nothing else.
    fn help_key(&mut self, key: KeyEvent, now: Instant) {
        if key.code == KeyCode::Char(' ') {
            // Keep the space bookkeeping right if its release arrives now.
            if key.kind == KeyEventKind::Release {
                self.space_up(now);
            }
            return;
        }
        if key.kind == KeyEventKind::Release {
            return;
        }
        if key.code != KeyCode::Esc && has_command_modifier(&key) {
            return;
        }
        let (code, _) = normalize(&key);
        let scroll = self.help_scroll.get();
        match code {
            KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') | KeyCode::F(1) => {
                self.show_help = false
            }
            KeyCode::Char('k') | KeyCode::Up => self.help_scroll.set(scroll.saturating_sub(1)),
            KeyCode::Char('j') | KeyCode::Down => {
                self.help_scroll.set((scroll + 1).min(HELP_SCROLL_MAX))
            }
            KeyCode::Home | KeyCode::Char('g') => self.help_scroll.set(0),
            KeyCode::End => self.help_scroll.set(HELP_SCROLL_MAX),
            _ => {}
        }
    }

    /// Tap mode only: is this space event a key auto-repeat / bounce rather
    /// than a deliberate press?
    ///
    /// A held key shows up as a stream of presses: the first repeat comes after
    /// the keyboard's repeat delay, then one every ~30 ms. Both are caught: the
    /// first by the time since the last press we *acted on*, the rest because
    /// they follow each other closely.
    ///
    /// Important: the reference for the debounce is the last *accepted* press.
    /// (Measuring from the last event of any kind would let a player who taps
    /// every ~half second keep postponing the debounce forever, so the second
    /// tap - the one that should start the timer - would never be accepted.)
    fn is_bounce(&mut self, now: Instant) -> bool {
        let in_stream = self
            .last_space_event
            .is_some_and(|t| now.saturating_duration_since(t) < REPEAT_GAP);
        let after_accept = self
            .last_space_accept
            .is_some_and(|t| now.saturating_duration_since(t) < TAP_DEBOUNCE);
        self.last_space_event = Some(now);
        in_stream || after_accept
    }

    fn space_down(&mut self, now: Instant) {
        if self.hold_mode {
            if self.space_is_down {
                // A press with no release in between.
                if self.releases_seen {
                    // The terminal does send releases, so one got lost (focus
                    // change, ...). Treat this press as the missing release so
                    // the timer can never get stuck in "holding".
                    let was_holding = matches!(self.phase, Phase::Holding { .. });
                    self.space_up(now);
                    if was_holding && matches!(self.phase, Phase::Running { .. }) {
                        self.space_is_down = true; // the key is physically down again
                        return;
                    }
                } else {
                    // Releases have never been delivered: this terminal does not
                    // really report them. Fall back to tap mode instead of
                    // waiting for a release that will never come.
                    self.fall_back_to_tap("no key-release events from this terminal: tap mode");
                }
            }
            self.space_is_down = true;
            self.last_space_press = Some(now);
        }

        if !self.hold_mode && self.is_bounce(now) {
            return;
        }
        if matches!(self.phase, Phase::Idle) && self.cooldown_until.is_some_and(|t| now < t) {
            return;
        }
        self.last_space_accept = Some(now);

        match self.phase {
            Phase::Idle => {
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
        self.releases_seen = true;
        self.space_is_down = false;
        let held = self.last_space_press.map(|t| now.saturating_duration_since(t));

        // A release that arrives together with its press is fabricated by the
        // terminal / multiplexer: the key's real state is unknowable, so
        // "hold until green" can never work. Switch to tap mode right away.
        if self.hold_mode && held.is_some_and(|h| h < SYNTHETIC_RELEASE) {
            self.fall_back_to_tap(
                "terminal reports key releases instantly: tap mode (space starts, space stops)",
            );
            self.treat_next_events_as_repeat(now);
            return;
        }

        if let Phase::Holding { since, inspect_start } = self.phase {
            if now.saturating_duration_since(since) >= HOLD_TO_READY {
                self.early_releases = 0;
                self.start_run(inspect_start, now);
                return;
            }
            // released too early: go back
            self.phase = match inspect_start {
                Some(start) => Phase::Inspecting { start },
                None => Phase::Idle,
            };
            let recent = self
                .last_early_release
                .is_some_and(|t| now.saturating_duration_since(t) < EARLY_RELEASE_WINDOW);
            self.early_releases = if recent { self.early_releases + 1 } else { 1 };
            self.last_early_release = Some(now);
            if self.early_releases >= EARLY_RELEASES_BEFORE_TAP {
                // Holding keeps failing (typically: the terminal's releases do
                // not match the physical key). Don't leave the player stuck.
                self.fall_back_to_tap("hold not detected: tap mode (space starts, space stops)");
                self.treat_next_events_as_repeat(now);
            } else {
                let ms = held.map_or(0, |h| h.as_millis());
                self.toast(format!(
                    "released after {ms} ms: hold space until the timer turns green"
                ));
            }
        }
    }

    /// Leave hold mode (the terminal can't be trusted with key releases).
    fn fall_back_to_tap(&mut self, why: &str) {
        self.hold_mode = false;
        self.space_is_down = false;
        if let Phase::Holding { inspect_start, .. } = self.phase {
            self.phase = match inspect_start {
                Some(start) => Phase::Inspecting { start },
                None => Phase::Idle,
            };
        }
        self.toast(why);
    }

    /// The key may still be physically held after a mode switch: whatever
    /// auto-repeat it produces must not count as a fresh tap.
    fn treat_next_events_as_repeat(&mut self, now: Instant) {
        self.last_space_accept = Some(now);
        self.last_space_event = Some(now);
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
        if has_command_modifier(&key) {
            return;
        }
        let idle = matches!(self.phase, Phase::Idle);
        let inspecting = matches!(self.phase, Phase::Inspecting { .. });
        if !idle && !inspecting {
            return;
        }

        let (code, shift) = normalize(&key);

        // Copying is allowed during inspection too.
        if let KeyCode::Char('y') = code {
            if !repeat {
                if shift {
                    if let Some(s) = self.selected_solve().map(|s| s.scramble.clone()) {
                        self.copy(s, "solve scramble");
                    }
                } else {
                    self.copy(self.scramble.clone(), "scramble");
                }
            }
            return;
        }
        if !idle {
            return;
        }

        match code {
            // navigation (vim + arrows); repeats allowed so holding j scrolls
            KeyCode::Char('k') | KeyCode::Char('h') | KeyCode::Up | KeyCode::Left => {
                self.selected = (self.selected + 1).min(self.solves.len().saturating_sub(1))
            }
            KeyCode::Char('j') | KeyCode::Char('l') | KeyCode::Down | KeyCode::Right => {
                self.selected = self.selected.saturating_sub(1)
            }
            KeyCode::Home => self.selected = self.solves.len().saturating_sub(1),
            KeyCode::End => self.selected = 0,
            KeyCode::Char('g') => {
                self.selected = if shift { 0 } else { self.solves.len().saturating_sub(1) }
            }
            _ if repeat => {}
            KeyCode::Char('?') | KeyCode::F(1) => {
                self.help_scroll.set(0);
                self.show_help = true;
            }
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
    fn press_mod(app: &mut App, c: KeyCode, m: KeyModifiers, t: Instant) {
        app.on_key(KeyEvent::new_with_kind(c, m, KeyEventKind::Press), t);
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
    fn early_release_explains_itself() {
        let t = Instant::now();
        let mut a = app(true, true);
        press(&mut a, SP, t);
        release(&mut a, SP, ms(t, 50));
        press(&mut a, SP, ms(t, 2000));
        release(&mut a, SP, ms(t, 2100)); // quick tap
        assert!(matches!(a.phase, Phase::Inspecting { .. }));
        assert!(a.active_toast().is_some_and(|m| m.contains("hold space")));
    }

    #[test]
    fn late_start_gets_penalty() {
        let t = Instant::now();
        let mut a = app(true, true);
        press(&mut a, SP, t);
        release(&mut a, SP, ms(t, 50));
        press(&mut a, SP, ms(t, 15_500));
        release(&mut a, SP, ms(t, 16_000)); // 16s of inspection -> +2
        assert!(matches!(a.phase, Phase::Running { penalty: Penalty::Plus2, .. }));

        let mut a = app(true, true);
        press(&mut a, SP, t);
        release(&mut a, SP, ms(t, 50));
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

    /// Regression: the old debounce was measured from the last space event of
    /// any kind, so repeatedly pressing space every ~300 ms (a player who is
    /// trying to start the timer) kept being swallowed forever.
    #[test]
    fn tap_mode_repeated_presses_are_not_swallowed_forever() {
        let t = Instant::now();
        let mut a = app(false, true);
        press(&mut a, SP, t); // inspect
        for i in 1..=2 {
            press(&mut a, SP, ms(t, i * 300)); // too soon: ignored
            assert!(matches!(a.phase, Phase::Inspecting { .. }));
        }
        press(&mut a, SP, ms(t, 900)); // 900 ms after the accepted press: start
        assert!(matches!(a.phase, Phase::Running { .. }));
    }

    #[test]
    fn tap_mode_without_inspection() {
        let t = Instant::now();
        let mut a = app(false, false);
        press(&mut a, SP, t);
        assert!(matches!(a.phase, Phase::Running { .. }));
        press(&mut a, SP, ms(t, 5000));
        assert_eq!(a.solves.len(), 1);
        assert_eq!(a.solves[0].time_ms, 5000);
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

    /// A terminal that claims key-release support but never sends releases
    /// must not leave the timer stuck: the second press starts it (tap mode).
    #[test]
    fn hold_mode_without_releases_falls_back_to_tap() {
        let t = Instant::now();
        let mut a = app(true, true);
        press(&mut a, SP, t);
        assert!(matches!(a.phase, Phase::Inspecting { .. }));
        press(&mut a, SP, ms(t, 3000)); // no release in between
        assert!(!a.hold_mode);
        assert!(matches!(a.phase, Phase::Running { .. }));
        press(&mut a, SP, ms(t, 13_000));
        assert_eq!(a.solves.len(), 1);
    }

    /// ...and the first auto-repeat of the very first press (reported as a
    /// press by such a terminal) must not start the timer by accident.
    #[test]
    fn fallback_does_not_misread_autorepeat_as_start() {
        let t = Instant::now();
        let mut a = app(true, true);
        press(&mut a, SP, t);
        press(&mut a, SP, ms(t, 500)); // repeat of the held key
        for i in 1..30 {
            press(&mut a, SP, ms(t, 500 + i * 30));
        }
        assert!(!a.hold_mode);
        assert!(matches!(a.phase, Phase::Inspecting { .. }));
        press(&mut a, SP, ms(t, 3000));
        assert!(matches!(a.phase, Phase::Running { .. }));
    }

    #[test]
    fn fallback_from_holding_without_inspection() {
        let t = Instant::now();
        let mut a = app(true, false);
        press(&mut a, SP, t);
        assert!(matches!(a.phase, Phase::Holding { .. }));
        press(&mut a, SP, ms(t, 2000)); // release never came
        assert!(!a.hold_mode);
        assert!(matches!(a.phase, Phase::Running { .. }));
    }

    /// If one release is lost on a terminal that normally sends them, the next
    /// press acts as that release instead of leaving the timer stuck.
    #[test]
    fn lost_release_is_recovered() {
        let t = Instant::now();
        let mut a = app(true, true);
        press(&mut a, SP, t);
        release(&mut a, SP, ms(t, 60)); // proves releases work
        press(&mut a, SP, ms(t, 2000)); // -> Holding
        assert!(matches!(a.phase, Phase::Holding { .. }));
        // release lost; user presses again 600 ms later
        press(&mut a, SP, ms(t, 2600));
        assert!(a.hold_mode);
        assert!(matches!(a.phase, Phase::Running { .. }));
        // the stop press still works and its own release is harmless
        release(&mut a, SP, ms(t, 2700));
        press(&mut a, SP, ms(t, 9000));
        assert_eq!(a.solves.len(), 1);
    }

    /// Terminals / multiplexers that fabricate a release right after every
    /// press made "hold until green" impossible: the timer never turned green.
    #[test]
    fn instant_fake_release_falls_back_to_tap() {
        let t = Instant::now();
        let mut a = app(true, true);
        press(&mut a, SP, t);
        release(&mut a, SP, t); // same instant: fabricated
        assert!(!a.hold_mode);
        assert!(a.active_toast().is_some_and(|m| m.contains("tap mode")));
        assert!(matches!(a.phase, Phase::Inspecting { .. }));
        // the key is held: fake press/release pairs keep coming, none counts
        for i in 1..20 {
            press(&mut a, SP, ms(t, i * 30));
            release(&mut a, SP, ms(t, i * 30));
        }
        assert!(matches!(a.phase, Phase::Inspecting { .. }));
        // a real tap later starts the timer, another one stops it
        press(&mut a, SP, ms(t, 3000));
        release(&mut a, SP, ms(t, 3000));
        assert!(matches!(a.phase, Phase::Running { .. }));
        press(&mut a, SP, ms(t, 9000));
        release(&mut a, SP, ms(t, 9000));
        assert_eq!(a.solves.len(), 1);
        assert_eq!(a.solves[0].time_ms, 6000);
    }

    /// Releases that arrive a little later than "instantly" but still before
    /// the timer is green, twice in a row: holding does not work -> tap mode.
    #[test]
    fn repeated_early_releases_fall_back_to_tap() {
        let t = Instant::now();
        let mut a = app(true, true);
        press(&mut a, SP, t);
        release(&mut a, SP, ms(t, 60));
        press(&mut a, SP, ms(t, 1000));
        release(&mut a, SP, ms(t, 1100)); // early #1: just a hint
        assert!(a.hold_mode);
        assert!(a.active_toast().is_some_and(|m| m.contains("released after 100 ms")));
        press(&mut a, SP, ms(t, 2000));
        release(&mut a, SP, ms(t, 2100)); // early #2: give up on holding
        assert!(!a.hold_mode);
        assert!(matches!(a.phase, Phase::Inspecting { .. }));
        press(&mut a, SP, ms(t, 5000)); // tap starts the solve
        assert!(matches!(a.phase, Phase::Running { .. }));
    }

    #[test]
    fn a_successful_hold_resets_the_early_release_count() {
        let t = Instant::now();
        let mut a = app(true, false);
        press(&mut a, SP, t);
        release(&mut a, SP, ms(t, 100)); // early #1
        press(&mut a, SP, ms(t, 1000));
        release(&mut a, SP, ms(t, 1400)); // proper hold -> running
        assert!(matches!(a.phase, Phase::Running { .. }));
        press(&mut a, SP, ms(t, 6000)); // stop
        release(&mut a, SP, ms(t, 6080));
        press(&mut a, SP, ms(t, 7000));
        release(&mut a, SP, ms(t, 7100)); // early again, but only #1 now
        assert!(a.hold_mode);
    }

    #[test]
    fn early_releases_far_apart_do_not_add_up() {
        let t = Instant::now();
        let mut a = app(true, false);
        press(&mut a, SP, t);
        release(&mut a, SP, ms(t, 100)); // early #1
        press(&mut a, SP, ms(t, 60_000));
        release(&mut a, SP, ms(t, 60_100)); // a minute later: counts as #1 again
        assert!(a.hold_mode);
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

        press(&mut a, KeyCode::Char('j'), t);
        press(&mut a, KeyCode::Down, t);
        assert_eq!(a.selected, 0);
        press(&mut a, KeyCode::Char('j'), t);
        assert_eq!(a.selected, 0);
        press(&mut a, KeyCode::Char('k'), t);
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
        assert_eq!(a.selected, 0);
        press(&mut a, KeyCode::Char('g'), t);
        assert_eq!(a.selected, 2);
    }

    #[test]
    fn ctrl_and_alt_combinations_are_not_commands() {
        let t = Instant::now();
        let mut a = with_solves(3);
        press_mod(&mut a, KeyCode::Char('x'), KeyModifiers::CONTROL, t); // not "delete"
        press_mod(&mut a, KeyCode::Char('d'), KeyModifiers::CONTROL, t); // not "DNF"
        press_mod(&mut a, KeyCode::Char('q'), KeyModifiers::ALT, t); // not "quit"
        press_mod(&mut a, KeyCode::Char('?'), KeyModifiers::CONTROL, t);
        assert_eq!(a.solves.len(), 3);
        assert_eq!(a.solves[2].penalty, Penalty::None);
        assert!(!a.should_quit);
        assert!(!a.show_help);
        press_mod(&mut a, KeyCode::Char('c'), KeyModifiers::CONTROL, t); // Ctrl-C still quits
        assert!(a.should_quit);
    }

    #[test]
    fn help_window_opens_and_closes() {
        let t = Instant::now();
        let mut a = with_solves(3);
        press(&mut a, KeyCode::Char('?'), t);
        assert!(a.show_help);

        // everything else is inert while it is open
        press(&mut a, KeyCode::Char('d'), t);
        press(&mut a, KeyCode::Delete, t);
        press(&mut a, SP, t);
        assert_eq!(a.solves.len(), 3);
        assert_eq!(a.solves[2].penalty, Penalty::None);
        assert!(matches!(a.phase, Phase::Idle));

        press(&mut a, KeyCode::Char('j'), t);
        assert_eq!(a.help_scroll.get(), 1);
        press(&mut a, KeyCode::Char('k'), t);
        press(&mut a, KeyCode::Char('k'), t);
        assert_eq!(a.help_scroll.get(), 0);

        press(&mut a, KeyCode::Esc, t);
        assert!(!a.show_help);
        press(&mut a, KeyCode::F(1), t);
        assert!(a.show_help);
        press(&mut a, KeyCode::Char('?'), t); // toggles
        assert!(!a.show_help);
        press(&mut a, KeyCode::Char('?'), t);
        press(&mut a, KeyCode::Char('q'), t); // closes help, does not quit
        assert!(!a.show_help);
        assert!(!a.should_quit);
    }

    /// With the kitty protocol, `?` arrives as `/` + Shift.
    #[test]
    fn help_opens_with_kitty_style_question_mark() {
        let t = Instant::now();
        let mut a = with_solves(1);
        press_mod(&mut a, KeyCode::Char('/'), KeyModifiers::SHIFT, t);
        assert!(a.show_help);
        press_mod(&mut a, KeyCode::Char('/'), KeyModifiers::SHIFT, t);
        assert!(!a.show_help);
        // plain `/` does nothing
        press(&mut a, KeyCode::Char('/'), t);
        assert!(!a.show_help);
    }

    #[test]
    fn help_is_not_available_mid_solve() {
        let t = Instant::now();
        let mut a = app(true, true);
        press(&mut a, SP, t); // inspecting
        press(&mut a, KeyCode::Char('?'), ms(t, 100));
        assert!(!a.show_help);
        release(&mut a, SP, ms(t, 120));
        press(&mut a, SP, ms(t, 1000));
        release(&mut a, SP, ms(t, 1400));
        assert!(matches!(a.phase, Phase::Running { .. }));
        press(&mut a, KeyCode::Char('?'), ms(t, 1500));
        assert!(!a.show_help);
    }

    #[test]
    fn help_does_not_break_space_bookkeeping() {
        // A space release that lands while help is open must still count, or
        // the next press would be mistaken for a lost release (and the app
        // would wrongly drop out of hold mode).
        let t = Instant::now();
        let mut a = with_solves(1);
        press(&mut a, SP, t); // inspecting, space physically down
        press(&mut a, KeyCode::Esc, ms(t, 50)); // cancel -> idle
        press(&mut a, KeyCode::Char('?'), ms(t, 100));
        assert!(a.show_help);
        release(&mut a, SP, ms(t, 150)); // release arrives while help is open
        press(&mut a, KeyCode::Esc, ms(t, 200)); // close help
        assert!(!a.show_help);

        press(&mut a, SP, ms(t, 1000));
        assert!(a.hold_mode);
        assert!(matches!(a.phase, Phase::Inspecting { .. }));
    }

    #[test]
    fn edit_keys_do_nothing_while_running() {
        let t = Instant::now();
        let mut a = with_solves(2);
        press(&mut a, SP, t);
        release(&mut a, SP, ms(t, 40));
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

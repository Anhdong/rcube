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
/// Tap mode: minimum time between two accepted presses, by what the press
/// would do. Stopping must be fast (sub-second solves); starting after the
/// inspection guards against the first auto-repeat of a held key (it arrives
/// 250-660 ms after the press) being taken for a second tap.
const TAP_GAP_STOP: Duration = Duration::from_millis(150);
const TAP_GAP_START: Duration = Duration::from_millis(700);
/// Tap mode: space events closer together than this are an auto-repeat stream
/// (repeat rates are ~30 ms), never two deliberate taps.
const REPEAT_GAP: Duration = Duration::from_millis(120);
/// Hold mode: a press while space is already down, less than this after the
/// previous space event, is an auto-repeat that the terminal reported as a
/// press, not a new press.
const HOLD_REPEAT_GAP: Duration = Duration::from_millis(900);
/// Hold mode: a release is held back this long. Many systems (X11, some
/// terminals) report the auto-repeat of a held key as "release + press" pairs
/// (every ~25 ms). A press that follows a release within this time cancels it,
/// so a held key stays held. Real releases are still timestamped exactly.
const RELEASE_GRACE: Duration = Duration::from_millis(40);
/// Ignore space for this long after a solve ends (Idle only).
const COOLDOWN: Duration = Duration::from_millis(150);
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
    /// The terminal can report key releases, so `t` may switch to hold mode.
    pub can_hold: bool,
    pub inspection: bool,
    pub should_quit: bool,
    /// Key-binding help window (`?`).
    pub show_help: bool,
    /// Details window for the selected solve (`Enter`).
    pub show_detail: bool,
    /// Scroll offset of the help window. A `Cell` so the renderer can clamp it
    /// to the window size without needing `&mut App`.
    pub help_scroll: Cell<u16>,

    // --- space-bar bookkeeping -------------------------------------------
    /// Last space press event (hold mode: repeat detection; tap mode: detects
    /// auto-repeat streams).
    last_space_event: Option<Instant>,
    /// Last space press that was acted on (tap mode: debounce reference).
    last_space_accept: Option<Instant>,
    /// Hold mode: a space press has been seen and its release has not.
    space_is_down: bool,
    /// Hold mode: at least one space release has ever been delivered, i.e. the
    /// terminal really does report releases.
    releases_seen: bool,
    /// Hold mode: a release that is waiting to see whether a press follows
    /// immediately (an auto-repeat pair). Holds the real release time.
    pending_release: Option<Instant>,
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
            can_hold: hold_mode,
            inspection,
            should_quit: false,
            show_help: false,
            show_detail: false,
            help_scroll: Cell::new(0),
            last_space_event: None,
            last_space_accept: None,
            space_is_down: false,
            releases_seen: false,
            pending_release: None,
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
        if self.pending_release.is_some() {
            return Duration::from_millis(5);
        }
        match self.phase {
            Phase::Idle => Duration::from_millis(250),
            Phase::Running { .. } => Duration::from_millis(100),
            // redraw often enough that the timer turns green on time
            Phase::Holding { .. } => Duration::from_millis(16),
            Phase::Inspecting { .. } => Duration::from_millis(40),
        }
    }

    pub fn on_tick(&mut self, now: Instant) {
        self.flush_release(now);
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
        if self.show_detail {
            self.detail_key(key, now);
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

    /// Space events while a pop-up window is open: they never start anything,
    /// but a release must still be tracked so the next press is not mistaken
    /// for a lost release. Returns true when the key was a space event.
    fn swallow_space(&mut self, key: &KeyEvent, now: Instant) -> bool {
        if key.code != KeyCode::Char(' ') {
            return false;
        }
        if key.kind == KeyEventKind::Release {
            self.pending_release = None;
            self.finish_release(now);
        }
        true
    }

    /// Keys while the help window is open: close or scroll, nothing else.
    fn help_key(&mut self, key: KeyEvent, now: Instant) {
        if self.swallow_space(&key, now) {
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

    /// Keys while the solve details window is open: close it, or browse to
    /// another solve (the window follows the selection). Nothing else.
    fn detail_key(&mut self, key: KeyEvent, now: Instant) {
        if self.swallow_space(&key, now) {
            return;
        }
        if key.kind == KeyEventKind::Release {
            return;
        }
        if key.code != KeyCode::Esc && has_command_modifier(&key) {
            return;
        }
        let (code, shift) = normalize(&key);
        let last = self.solves.len().saturating_sub(1);
        match code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => self.show_detail = false,
            KeyCode::Char('k') | KeyCode::Char('h') | KeyCode::Up | KeyCode::Left => {
                self.selected = (self.selected + 1).min(last)
            }
            KeyCode::Char('j') | KeyCode::Char('l') | KeyCode::Down | KeyCode::Right => {
                self.selected = self.selected.saturating_sub(1)
            }
            KeyCode::Home => self.selected = last,
            KeyCode::End => self.selected = 0,
            KeyCode::Char('g') => self.selected = if shift { 0 } else { last },
            _ => {}
        }
    }

    /// Minimum time since the last accepted press, for a press in the current
    /// phase (tap mode).
    fn tap_gap(&self) -> Duration {
        match self.phase {
            Phase::Running { .. } => TAP_GAP_STOP,
            Phase::Inspecting { .. } => TAP_GAP_START,
            _ => Duration::ZERO,
        }
    }

    /// Tap mode only: is this space event a key auto-repeat rather than a
    /// deliberate press?
    ///
    /// A held key shows up as a stream of presses: the first repeat comes after
    /// the keyboard's repeat delay, then one every ~30 ms. The stream is caught
    /// because its events follow each other closely; the first repeat by the
    /// per-phase gap since the last press we *acted on*.
    ///
    /// Important: the reference is the last *accepted* press. (Measuring from
    /// the last event of any kind would let a player who taps every ~half
    /// second keep postponing the debounce forever.) Stopping only needs a
    /// short gap so that sub-second solves can be stopped.
    fn is_bounce(&mut self, now: Instant) -> bool {
        let in_stream = self
            .last_space_event
            .is_some_and(|t| now.saturating_duration_since(t) < REPEAT_GAP);
        let gap = self.tap_gap();
        let after_accept = self
            .last_space_accept
            .is_some_and(|t| now.saturating_duration_since(t) < gap);
        self.last_space_event = Some(now);
        in_stream || after_accept
    }

    fn space_down(&mut self, now: Instant) {
        if self.hold_mode {
            // A release that has waited long enough is a real one.
            self.flush_release(now);
            if self.pending_release.take().is_some() {
                // Release immediately followed by a press: that is how the
                // auto-repeat of a held key shows up on some systems. The key
                // never went up; ignore both events.
                self.last_space_event = Some(now);
                return;
            }
            if self.space_is_down {
                let since_last = self
                    .last_space_event
                    .map_or(Duration::MAX, |t| now.saturating_duration_since(t));
                if since_last < HOLD_REPEAT_GAP {
                    // Auto-repeat that the terminal reported as a press.
                    self.last_space_event = Some(now);
                    return;
                }
                // A genuinely new press with no release in between.
                if self.releases_seen {
                    // The terminal does send releases, so one got lost (focus
                    // change, ...). Treat this press as the missing release so
                    // the timer can never get stuck in "holding".
                    let was_holding = matches!(self.phase, Phase::Holding { .. });
                    self.finish_release(now);
                    if was_holding && matches!(self.phase, Phase::Running { .. }) {
                        self.space_is_down = true; // the key is physically down again
                        self.last_space_event = Some(now);
                        return;
                    }
                } else {
                    // Releases have never been delivered: this terminal does not
                    // really report them. Fall back to tap mode instead of
                    // waiting for a release that will never come.
                    self.fall_back_to_tap("no key-release events from this terminal: tap mode");
                }
            }
            if self.hold_mode {
                self.space_is_down = true;
                self.last_space_event = Some(now);
            }
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

    /// A space release event arrived.
    fn space_up(&mut self, now: Instant) {
        self.releases_seen = true;
        if !self.hold_mode {
            self.space_is_down = false;
            return;
        }
        self.flush_release(now);
        if self.space_is_down && self.pending_release.is_none() {
            // Wait briefly: a press right behind it means auto-repeat.
            self.pending_release = Some(now);
        }
    }

    /// Turn a pending release into a real one once nothing followed it.
    fn flush_release(&mut self, now: Instant) {
        if let Some(at) = self.pending_release {
            if now.saturating_duration_since(at) >= RELEASE_GRACE {
                self.pending_release = None;
                self.finish_release(at);
            }
        }
    }

    /// The key really went up at `now` (the release's own timestamp).
    fn finish_release(&mut self, now: Instant) {
        self.releases_seen = true;
        self.space_is_down = false;
        if let Phase::Holding { since, inspect_start } = self.phase {
            if now.saturating_duration_since(since) >= HOLD_TO_READY {
                self.start_run(inspect_start, now);
            } else {
                // Released too early: silently go back to where we were.
                self.phase = match inspect_start {
                    Some(start) => Phase::Inspecting { start },
                    None => Phase::Idle,
                };
            }
        }
    }

    /// Leave hold mode (only used when no release has ever been delivered).
    fn fall_back_to_tap(&mut self, why: &str) {
        self.hold_mode = false;
        self.space_is_down = false;
        self.pending_release = None;
        if let Phase::Holding { inspect_start, .. } = self.phase {
            self.phase = match inspect_start {
                Some(start) => Phase::Inspecting { start },
                None => Phase::Idle,
            };
        }
        self.toast(why);
    }

    /// `t`: switch between hold and tap mode by hand.
    fn toggle_mode(&mut self) {
        if self.hold_mode {
            self.hold_mode = false;
            self.space_is_down = false;
            self.pending_release = None;
            self.toast("tap mode: space starts, space stops");
        } else if self.can_hold {
            self.hold_mode = true;
            self.space_is_down = false;
            self.pending_release = None;
            self.releases_seen = false;
            self.toast("hold mode: hold space until green, release to start");
        } else {
            self.toast("this terminal can't report key releases: tap mode only");
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
        // So is switching between hold and tap mode.
        if let KeyCode::Char('t') = code {
            if !repeat {
                self.toggle_mode();
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
            KeyCode::Enter => {
                if self.selected_solve().is_some() {
                    self.show_detail = true;
                }
            }
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
    /// A release as it arrives from the terminal, with no event loop tick.
    fn release_raw(app: &mut App, c: KeyCode, t: Instant) {
        app.on_key(key(c, KeyEventKind::Release), t);
    }
    /// A real release: nothing follows it, so the event loop's tick (a little
    /// later) turns it into a finished release.
    fn release(app: &mut App, c: KeyCode, t: Instant) {
        release_raw(app, c, t);
        app.on_tick(t + Duration::from_millis(60));
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

    /// Letting go too early silently returns to the inspection: no message.
    #[test]
    fn early_release_goes_back_silently() {
        let t = Instant::now();
        let mut a = app(true, true);
        press(&mut a, SP, t);
        release(&mut a, SP, ms(t, 50));
        press(&mut a, SP, ms(t, 2000));
        release(&mut a, SP, ms(t, 2100)); // quick tap
        assert!(matches!(a.phase, Phase::Inspecting { .. }));
        assert!(a.active_toast().is_none());
        assert!(a.hold_mode);
        // ...and a proper hold still works afterwards
        press(&mut a, SP, ms(t, 3000));
        release(&mut a, SP, ms(t, 3400));
        assert!(matches!(a.phase, Phase::Running { .. }));
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

    /// Regression: a flat 0.8 s debounce made solves under 0.8 s impossible to
    /// stop in tap mode.
    #[test]
    fn tap_mode_allows_sub_second_solves() {
        let t = Instant::now();
        let mut a = app(false, false);
        press(&mut a, SP, t); // start
        press(&mut a, SP, ms(t, 400)); // stop
        assert_eq!(a.solves.len(), 1);
        assert_eq!(a.solves[0].time_ms, 400);

        let mut a = app(false, true);
        press(&mut a, SP, t); // inspect
        press(&mut a, SP, ms(t, 1000)); // start
        press(&mut a, SP, ms(t, 1400)); // stop
        assert_eq!(a.solves.len(), 1);
        assert_eq!(a.solves[0].time_ms, 400);
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

    /// Auto-repeat of a held key that a terminal reports as presses must not
    /// start the timer, and must not be mistaken for "no releases": it is just
    /// ignored and hold mode stays on.
    #[test]
    fn autorepeat_reported_as_press_is_ignored() {
        let t = Instant::now();
        let mut a = app(true, true);
        press(&mut a, SP, t);
        press(&mut a, SP, ms(t, 500)); // repeat of the held key
        for i in 1..30 {
            press(&mut a, SP, ms(t, 500 + i * 30));
        }
        assert!(a.hold_mode);
        assert!(matches!(a.phase, Phase::Inspecting { .. }));
        // a real press much later, with still no release ever seen: tap mode
        press(&mut a, SP, ms(t, 3000));
        assert!(!a.hold_mode);
        assert!(matches!(a.phase, Phase::Running { .. }));
    }

    #[test]
    fn hold_mode_survives_repeats_reported_as_presses() {
        let t = Instant::now();
        let mut a = app(true, true);
        press(&mut a, SP, t);
        for i in 0..40 {
            press(&mut a, SP, ms(t, 600 + i * 30)); // auto-repeat as Press
        }
        assert!(a.hold_mode);
        assert!(matches!(a.phase, Phase::Inspecting { .. }));
        release(&mut a, SP, ms(t, 2000));
        press(&mut a, SP, ms(t, 3000)); // real press: hold
        for i in 0..20 {
            press(&mut a, SP, ms(t, 3600 + i * 30)); // repeats while holding
        }
        assert!(matches!(a.phase, Phase::Holding { .. }));
        release(&mut a, SP, ms(t, 4400));
        assert!(a.hold_mode);
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
        // release lost; user presses again 1.5 s later
        press(&mut a, SP, ms(t, 3500));
        assert!(a.hold_mode);
        assert!(matches!(a.phase, Phase::Running { .. }));
        // the stop press still works and its own release is harmless
        release(&mut a, SP, ms(t, 3600));
        press(&mut a, SP, ms(t, 9000));
        assert_eq!(a.solves.len(), 1);
    }

    /// Regression: an instant release used to throw the app into tap mode for
    /// good. Now it only fails that one hold; holding works afterwards.
    #[test]
    fn instant_release_does_not_leave_hold_mode() {
        let t = Instant::now();
        let mut a = app(true, true);
        press(&mut a, SP, t);
        release(&mut a, SP, t); // same instant
        assert!(a.hold_mode);
        assert!(matches!(a.phase, Phase::Inspecting { .. }));
        press(&mut a, SP, ms(t, 1000));
        assert!(matches!(a.phase, Phase::Holding { .. }));
        release(&mut a, SP, ms(t, 1400));
        assert!(matches!(a.phase, Phase::Running { .. }));
    }

    #[test]
    fn t_toggles_mode_only_when_releases_are_available() {
        let t = Instant::now();
        let mut a = app(true, true);
        press(&mut a, KeyCode::Char('t'), t);
        assert!(!a.hold_mode);
        press(&mut a, KeyCode::Char('t'), t);
        assert!(a.hold_mode);

        // a terminal without release support stays in tap mode
        let mut b = app(false, true);
        press(&mut b, KeyCode::Char('t'), t);
        assert!(!b.hold_mode);
        assert!(b.active_toast().is_some_and(|m| m.contains("can't report")));
    }

    /// Regression: on X11-like systems the auto-repeat of a held key arrives as
    /// "release + press" pairs every ~25 ms. That used to restart the hold timer
    /// each time, so the timer never turned green.
    #[test]
    fn autorepeat_as_release_press_pairs_keeps_the_key_held() {
        let t = Instant::now();
        let mut a = app(true, true);
        press(&mut a, SP, t); // start inspection, key still held
        for i in 0..20 {
            release_raw(&mut a, SP, ms(t, 200 + i * 25));
            press(&mut a, SP, ms(t, 201 + i * 25));
            assert!(matches!(a.phase, Phase::Inspecting { .. }), "pair {i}");
        }
        release(&mut a, SP, ms(t, 800)); // real release
        assert!(matches!(a.phase, Phase::Inspecting { .. }));

        press(&mut a, SP, ms(t, 2000)); // hold
        for i in 0..40 {
            release_raw(&mut a, SP, ms(t, 2200 + i * 25));
            press(&mut a, SP, ms(t, 2201 + i * 25));
        }
        match a.phase {
            Phase::Holding { since, .. } => assert_eq!(since, ms(t, 2000)),
            other => panic!("expected Holding, got {other:?}"),
        }
        assert!(a.hold_mode);
        release(&mut a, SP, ms(t, 3300)); // real release, held 1.3 s
        assert!(matches!(a.phase, Phase::Running { .. }));
        // the solve starts at the real release time, not when it was flushed
        press(&mut a, SP, ms(t, 3300 + 700));
        assert_eq!(a.solves[0].time_ms, 700);
    }

    /// Holding the stop key down must not start a new inspection through its
    /// auto-repeat pairs.
    #[test]
    fn held_stop_key_does_not_restart_inspection() {
        let t = Instant::now();
        let mut a = app(true, false);
        press(&mut a, SP, t);
        release(&mut a, SP, ms(t, 400)); // running
        assert!(matches!(a.phase, Phase::Running { .. }));
        press(&mut a, SP, ms(t, 5000)); // stop, key stays down
        assert_eq!(a.solves.len(), 1);
        for i in 0..30 {
            release_raw(&mut a, SP, ms(t, 5400 + i * 25));
            press(&mut a, SP, ms(t, 5401 + i * 25));
        }
        assert!(matches!(a.phase, Phase::Idle));
        release(&mut a, SP, ms(t, 6300));
        assert!(matches!(a.phase, Phase::Idle));
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
        press_mod(&mut a, KeyCode::Char('t'), KeyModifiers::CONTROL, t); // not "toggle mode"
        press_mod(&mut a, KeyCode::Enter, KeyModifiers::CONTROL, t); // not "details"
        assert_eq!(a.solves.len(), 3);
        assert_eq!(a.solves[2].penalty, Penalty::None);
        assert!(a.hold_mode);
        assert!(!a.should_quit);
        assert!(!a.show_help);
        assert!(!a.show_detail);
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
        // the mode can't be switched while the timer runs either
        press(&mut a, KeyCode::Char('t'), ms(t, 1600));
        assert!(a.hold_mode);
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
    fn enter_opens_and_closes_the_detail_window() {
        let t = Instant::now();
        let mut a = with_solves(3);
        assert_eq!(a.selected, 2);
        press(&mut a, KeyCode::Enter, t);
        assert!(a.show_detail);

        // browsing moves the selection while the window stays open
        press(&mut a, KeyCode::Char('j'), t);
        assert_eq!(a.selected, 1);
        press(&mut a, KeyCode::Down, t);
        assert_eq!(a.selected, 0);
        press(&mut a, KeyCode::Char('k'), t);
        assert_eq!(a.selected, 1);
        press(&mut a, KeyCode::Char('G'), t);
        assert_eq!(a.selected, 0);
        press(&mut a, KeyCode::Char('g'), t);
        assert_eq!(a.selected, 2);
        assert!(a.show_detail);

        // everything else is inert while it is open
        press(&mut a, KeyCode::Char('d'), t);
        press(&mut a, KeyCode::Char('2'), t);
        press(&mut a, KeyCode::Delete, t);
        press(&mut a, KeyCode::Char('r'), t);
        press(&mut a, SP, t);
        assert_eq!(a.solves.len(), 3);
        assert_eq!(a.solves[2].penalty, Penalty::None);
        assert!(matches!(a.phase, Phase::Idle));
        assert!(!a.show_help);

        // Enter, Esc and q all close it; q does not quit
        press(&mut a, KeyCode::Enter, t);
        assert!(!a.show_detail);
        press(&mut a, KeyCode::Enter, t);
        press(&mut a, KeyCode::Esc, t);
        assert!(!a.show_detail);
        press(&mut a, KeyCode::Enter, t);
        press(&mut a, KeyCode::Char('q'), t);
        assert!(!a.show_detail);
        assert!(!a.should_quit);
    }

    #[test]
    fn detail_window_needs_a_solve_and_an_idle_timer() {
        let t = Instant::now();
        let mut a = with_solves(0);
        press(&mut a, KeyCode::Enter, t);
        assert!(!a.show_detail);

        let mut a = with_solves(2);
        press(&mut a, SP, t); // inspecting
        press(&mut a, KeyCode::Enter, ms(t, 100));
        assert!(!a.show_detail);
    }

    #[test]
    fn detail_window_does_not_break_space_bookkeeping() {
        let t = Instant::now();
        let mut a = with_solves(1);
        press(&mut a, SP, t); // inspecting, space physically down
        press(&mut a, KeyCode::Esc, ms(t, 50)); // cancel -> idle
        press(&mut a, KeyCode::Enter, ms(t, 100));
        assert!(a.show_detail);
        release(&mut a, SP, ms(t, 150)); // release arrives while the window is open
        press(&mut a, KeyCode::Esc, ms(t, 200));
        assert!(!a.show_detail);

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
# rcube

A vim-style **3x3 speedcubing timer** for the Linux terminal, written in Rust
with [ratatui](https://ratatui.rs) and crossterm.

## Features

- Big block-digit timer in the centre that changes colour with its state
- WCA-style 15 s inspection with automatic +2 (after 15 s) and DNF (after 17 s)
- Time hidden while solving and replaced by a white `SOLVING`
- Result list with an ao5 next to every result, plus ao12, ao100, mean and bests
- Statistics centred under the timer
- Scramble centred above the timer, with some margin to the screen edge; compact cube net (U / L F R B / D) in the bottom-right corner
- Results panel (bottom-left) sized for the last 5 solves; the selection scrolls through older ones
- Details window for the highlighted solve (`Enter`): time, penalty, averages, date and scramble
- Clean screen: all key bindings live in a help window, opened with `?`
- Vim-style navigation and editing of results
- Single-file JSON storage that is easy to back up
- Works over SSH; clipboard copy via `wl-copy`, `xclip`, `xsel` or OSC 52

## Build and run

```sh
cargo build --release
./target/release/rcube      # or: cargo install --path .
```

Rust 1.80+ recommended (developed and tested with 1.89).
Dependencies: `ratatui`, `rand`, `serde`, `serde_json`.

### Options

```
--tap            never rely on key-release events
--no-inspect     disable the 15 s inspection
--data <FILE>    use a different solves file
-h, --help       show help
```

## Keys

Press **`?`** (or `F1`) inside rcube to see the key bindings; `?`, `F1`, `Esc`
or `q` closes the window again, `j` / `k` scroll it on small terminals. The
help is available when no solve is in progress.

| Key                       | Action                                                          |
|---------------------------|-----------------------------------------------------------------|
| `space`                   | inspect, hold (turns green), release to start, `space` to stop  |
| `h` `j` `k` `l`, arrows   | move between results (`k`/`h` = older, `j`/`l` = newer)         |
| `g` / `G`, Home / End     | first / last result                                             |
| `Enter`                   | show the details of the selected result in a window             |
| `2`                       | toggle +2 on the selected result                                |
| `d`                       | toggle DNF on the selected result                               |
| `Delete` (or `x`)         | delete the selected result                                      |
| `u`                       | undo the last delete                                            |
| `r`                       | new scramble                                                    |
| `y` / `Y`                 | copy the scramble / copy the selected result's scramble         |
| `?` / `F1`                | show / hide the help window                                     |
| `Esc`                     | cancel inspection, or discard a running solve                   |
| `q`, `Ctrl-C`             | quit                                                            |

Ctrl/Alt combinations are never treated as commands (so `Ctrl-X` can not delete
a solve); only `Ctrl-C` is special.

### Solve details window

`Enter` opens a window with everything about the highlighted solve: the time
as it counts (with +2 or DNF applied), the raw time, the penalty, the ao5 /
ao12 / ao100 ending at that solve, whether it is a best single or best ao5,
when it was solved (UTC) and its scramble.

While the window is open, `h` `j` `k` `l` / arrows / `g` / `G` / Home / End
browse to other solves and the window follows the selection. `Enter`, `Esc`
or `q` closes it (`q` does not quit). Editing keys do nothing while it is open;
close it first. It is only available when no solve is in progress.

## Timer colours

| Colour | Meaning                                                              |
|--------|----------------------------------------------------------------------|
| White  | idle, and the "SOLVING" text while the timer runs                    |
| Red    | inspection countdown, or a DNF result                                |
| Green  | space is held and ready, or the shown result is a best single / ao5  |

## Hold-to-start needs key-release events

Terminals normally report only key presses. rcube requests key-release
events through the kitty keyboard protocol (supported by kitty, foot, Ghostty,
recent Alacritty, and WezTerm with `enable_kitty_keyboard = true`). If the
terminal does not support it (for example GNOME Terminal), rcube switches to
**tap mode** automatically and shows `[tap mode]` in the footer:
`space` = inspect, `space` = start, `space` = stop. Use `--tap` to force it.

In hold mode, the first `space` starts the inspection. To start the solve, press
and **hold** `space` until the timer turns green (0.3 s), then release. Letting
go too early returns to the inspection and shows a hint.

The timer cannot get stuck when the terminal's key releases don't match the
physical key:

- If a release arrives practically together with its press (under 15 ms, which
  no finger can do), the terminal or multiplexer is fabricating releases. rcube
  switches to tap mode immediately.
- If you let go before the timer is green twice in a row, holding evidently
  isn't being detected, and rcube switches to tap mode too. The first early
  release shows how long the key was actually down ("released after 4 ms: ...").

- If the terminal claims key-release support but never delivers a release, rcube
  notices on the second press and falls back to tap mode on the fly.
- If a single release is lost on a terminal that normally sends them (for
  example because the window lost focus mid-press), the next `space` press acts
  as that release. `Esc` always cancels.

In tap mode a held key (auto-repeat) is told apart from deliberate taps: events
within 0.8 s of the last accepted press, and events that follow each other
within 0.12 s, are ignored. Repeatedly tapping `space` is never swallowed
forever; the first tap after 0.8 s is accepted.

## Layout

```
                       scramble moves (centred)

                            big timer

 .- results (n) ---.        badge                    .- scramble -.
 | #   time    ao5 |   ao5 / ao12 / ao100 / mean     |  cube net   |
 | 8  10.21   9.86 |      best / best ao5            |  (compact)  |
 | ...  5 rows     |                                 |             |
 '- 12/12 ---------'                                 '-------------'
 ? help
```

The two bottom panels always have the same height: borders, the header, and
five solves. Move the selection (`k`/`j`, `g`/`G`, arrows) to scroll the list;
the bottom border shows the position (`4/12`) once there are more than five
solves.

The badge and statistics are centred on the screen, as long as the window is
wide enough (about 117 columns with both panels visible). On narrower windows
they sit in the column between the two panels instead.

The results panel needs 75 columns, the cube net 104 (the statistics line
needs 57 columns of room). When the window is short, the bottom row shrinks
first, keeping the timer at least 7 rows tall.

The solve details window and the help window are pop-ups drawn in the middle
of the screen on top of everything else.

## Data storage

All solves live in one human-readable file:
`$XDG_DATA_HOME/rcube/solves.json` (default
`~/.local/share/rcube/solves.json`). **To back up, copy that file.**

```json
{
  "format": "rcube",
  "version": 1,
  "solves": [
    {
      "id": "019350a1b2c3a1b2c3d4",
      "event": "333",
      "time_ms": 12345,
      "penalty": "none",
      "scramble": "R U R' U' ...",
      "timestamp": 1732100000000
    }
  ]
}
```

- `time_ms` is the raw time; `penalty` (`none`, `plus2`, `dnf`) is stored
  separately, so toggling a penalty never loses information.
- `event` allows other puzzles to share the file later. Solves of events this
  version does not know are preserved untouched.
- `id` is unique and time-sortable, so files from two machines can be merged.
- `timestamp` is Unix milliseconds; the details window shows it as UTC.
- Writes are atomic (temp file, fsync, rename, directory fsync), a failed write
  leaves no stray temp file, and the previous file is copied to
  `solves.json.bak` on every start.
- A corrupt or unsupported file is never overwritten: rcube refuses to
  start and explains why.

## Code layout

```
src/main.rs       terminal setup, event loop, command-line options
src/app.rs        state machine (idle / inspect / hold / run), key handling, help and details state
src/ui.rs         rendering: results, timer, scramble panel, help and details windows
src/big.rs        big block-letter font (three sizes, chosen to fit the window)
src/cube.rs       sticker-level cube model used for the scramble preview
src/scramble.rs   random-move scramble generator (20 moves, no redundant turns)
src/stats.rs      ao5 / ao12 / ao100, mean, record tracking
src/store.rs      JSON persistence
src/clipboard.rs  clipboard helpers
```

## Testing

`cargo test` runs the unit tests covering cube moves and net orientation,
scramble rules, averages with DNF and +2, the timer state machine in both hold
and tap mode (including auto-repeat, lost key releases and terminals that never
send releases), the help and details windows (opening, closing, browsing, space
bookkeeping, timestamp formatting), storage round-trips, and rendering at many
terminal sizes (panel geometry, the 5-solve results window and its scrolling,
centred scramble text and statistics, the compact cube net, both pop-up
windows, no panics).

The release binary was also driven through a pseudo-terminal, once as a legacy
terminal (tap mode) and once emulating a kitty-protocol terminal (hold mode),
including terminals that announce key-release support but never send
releases, or fabricate one right after every press.
Clipboard integration and real-terminal appearance have not been tested.

## Limitations and ideas

- 3x3 only for now; the `event` field in the data file is the hook for other puzzles.
- All solves share one list (no sessions yet).
- The results panel is hidden below 75 columns, the cube net below 104.
- The details window shows times in UTC and does not draw the scramble's cube net.
- Two rcube instances sharing one solves file overwrite each other's changes
  (last writer wins); run only one at a time per file.
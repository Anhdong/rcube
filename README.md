# rcube

A vim-style **3x3 speedcubing timer** for the Linux terminal, written in Rust
with [ratatui](https://ratatui.rs) and crossterm.

## Features

- Big block-digit timer in the centre that changes colour with its state
- WCA-style 15 s inspection with automatic +2 (after 15 s) and DNF (after 17 s)
- Time hidden while solving and replaced by a white `SOLVING`
- Result list with an ao5 next to every result, plus ao12, ao100, mean and bests
- Scramble cube net (U / L F R B / D) drawn from the current scramble
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

| Key                       | Action                                                          |
|---------------------------|-----------------------------------------------------------------|
| `space`                   | inspect, hold (turns green), release to start, `space` to stop  |
| `h` `j` `k` `l`, arrows   | move between results (`k`/`h` = older, `j`/`l` = newer)         |
| `g` / `G`, Home / End     | first / last result                                             |
| `2`                       | toggle +2 on the selected result                                |
| `d`                       | toggle DNF on the selected result                               |
| `Delete` (or `x`)         | delete the selected result                                      |
| `u`                       | undo the last delete                                            |
| `r`                       | new scramble                                                    |
| `y` / `Y`                 | copy the scramble / copy the selected result's scramble         |
| `Esc`                     | cancel inspection, or discard a running solve                   |
| `q`, `Ctrl-C`             | quit                                                            |

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
- Writes are atomic (temp file, fsync, rename), and the previous file is copied
  to `solves.json.bak` on every start.
- A corrupt or unsupported file is never overwritten: rcube refuses to
  start and explains why.

## Code layout

```
src/main.rs       terminal setup, event loop, command-line options
src/app.rs        state machine (idle / inspect / hold / run) and key handling
src/ui.rs         rendering
src/big.rs        big block-letter font (three sizes, chosen to fit the window)
src/cube.rs       sticker-level cube model used for the scramble preview
src/scramble.rs   random-move scramble generator (20 moves, no redundant turns)
src/stats.rs      ao5 / ao12 / ao100, mean, record tracking
src/store.rs      JSON persistence
src/clipboard.rs  clipboard helpers
```

## Testing

`cargo test` runs 29 unit tests covering cube moves and net orientation,
scramble rules, averages with DNF and +2, the timer state machine in both hold
and tap mode, storage round-trips, and rendering at many terminal sizes.

The release binary was also driven through a pseudo-terminal, once as a legacy
terminal (tap mode) and once emulating a kitty-protocol terminal (hold mode).
Clipboard integration and real-terminal appearance have not been tested.

## Limitations and ideas

- 3x3 only for now; the `event` field in the data file is the hook for other puzzles.
- All solves share one list (no sessions yet).
- The left panel is hidden below 75 columns, and the cube net is hidden when the
  terminal is shorter than about 20 rows.

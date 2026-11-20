mod app;
mod big;
mod clipboard;
mod cube;
mod scramble;
mod stats;
mod store;
mod ui;

use app::App;
use ratatui::crossterm::event::{
    self, Event, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::supports_keyboard_enhancement;
use ratatui::DefaultTerminal;
use std::error::Error;
use std::io::{self, stdout};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const HELP: &str = "\
rcube - a vim-style 3x3 speedcubing timer for the terminal

USAGE:
    rcube [OPTIONS]

OPTIONS:
        --tap            Tap mode: never rely on key-release events
                         (space = inspect/start/stop; chosen automatically
                         when the terminal can't report key releases)
        --no-inspect     Disable the 15 second inspection
        --data <FILE>    Use a different solves file
    -h, --help           Show this help

KEYS:
    space            inspect -> hold (green) -> release to start -> space to stop
    h j k l / arrows move between results
    g / G            first / last result
    2                toggle +2 on the selected result
    d                toggle DNF
    Delete (or x)    delete the selected result      u = undo delete
    r                new scramble
    y                copy scramble (Y: copy the selected result's scramble)
    Esc              cancel inspection / discard running solve
    q or Ctrl-C      quit

DATA:
    solves are stored as JSON at $XDG_DATA_HOME/rcube/solves.json
    (default ~/.local/share/rcube/solves.json) - copy that file to back up.
";

struct Options {
    tap: bool,
    inspect: bool,
    data: Option<PathBuf>,
}

fn parse_args() -> Result<Options, String> {
    let mut o = Options { tap: false, inspect: true, data: None };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                std::process::exit(0);
            }
            "--tap" => o.tap = true,
            "--no-inspect" => o.inspect = false,
            "--data" => {
                o.data = Some(args.next().ok_or("--data needs a file path")?.into());
            }
            other => return Err(format!("unknown option {other:?} (see --help)")),
        }
    }
    Ok(o)
}

fn main() -> Result<(), Box<dyn Error>> {
    let opts = parse_args()?;
    let path = opts.data.unwrap_or_else(store::Store::default_path);
    let (store, solves) = store::Store::open(path)?;
    let mut app = App::new(store, solves, false, opts.inspect);

    let mut terminal = ratatui::init();

    // Ask the terminal to report key *releases* (kitty keyboard protocol) so
    // "hold space, release to start" works. Not available everywhere.
    let enhanced = !opts.tap && supports_keyboard_enhancement().unwrap_or(false);
    if enhanced {
        execute!(
            stdout(),
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                    | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
            )
        )?;
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = execute!(stdout(), PopKeyboardEnhancementFlags);
            prev(info);
        }));
    }
    app.hold_mode = enhanced;

    let result = run(&mut terminal, &mut app);

    if enhanced {
        let _ = execute!(stdout(), PopKeyboardEnhancementFlags);
    }
    ratatui::restore();
    Ok(result?)
}

fn run(terminal: &mut DefaultTerminal, app: &mut App) -> io::Result<()> {
    while !app.should_quit {
        terminal.draw(|f| ui::draw(f, app))?;

        if event::poll(app.tick_interval())? {
            // Drain everything that's queued so key handling never lags behind.
            loop {
                let now = Instant::now(); // taken before read(): that's the key's timestamp
                if let Event::Key(key) = event::read()? {
                    app.on_key(key, now);
                }
                if app.should_quit || !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }
        app.on_tick(Instant::now());
    }
    Ok(())
}

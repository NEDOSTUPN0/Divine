# Working on Divine

Divine is a GTK4/libadwaita launcher for Epic Games, written in Rust. It drives
the `legendary` CLI rather than reimplementing it.

Much of this code was written by AI, and that is fine — the bar is the same
either way: code a person can read six months from now. These are the rules that
keep it looking like one hand wrote it.

## Language

Everything inside the repository is English: identifiers, comments, commit
messages, UI strings, test names. No exceptions, whatever language the
conversation is in.

## Where code goes

Put a thing where its name already says it belongs. Check before adding a module:

- `epic/` — anything Divine asks Epic directly, because legendary has no command
  for it. Use `epic::store_agent()` / `epic::graphql()`; never build a request
  by hand.
- `http.rs` — every other service (ProtonDB, GOG, umu, GitHub, Lutris). Use
  `http::agent()`, `http::ask()`, `http::remembered()`.
- `legendary.rs` — the only place that spawns `legendary`.
- `log.rs` — all output. Never `println!` or `eprintln!`.
- `ui/` — GTK only. Keep logic out of it: a function worth testing does not
  belong in a widget builder.

When a file grows past a thousand lines, ask whether it is still one job.
`ui/game_page/` was split into a module per block it shows; `legendary.rs` is
that long and stays, because it is one wrapper around one CLI.

## Comments

Every module opens with `//!` saying what it is for and what is not obvious
about it. Public items get a `///` when the name does not already tell the whole
story — `covers_dir()` needs nothing, `same_disk()` needs a paragraph.

Comments explain **why**, never what. The code already says what.

```rust
// f_bavail rather than f_bfree: the blocks held back for root are not ours.
```

Prefer one sentence of real reason over three of restatement. If nothing
non-obvious is happening, write no comment. Do not leave `TODO`, commented-out
code, or a doc comment describing the item above or below it.

## Rust

- Errors: `anyhow`, with `.context()` / `.with_context()` at every step that can
  fail. Print them as `{e:#}` so the chain survives.
- `Result` when something failed; `Option` when "no answer" is an ordinary
  outcome, such as a service that has never heard of this game.
- No `unwrap`, `expect` or `panic!` outside tests and genuine invariants.
- No new dependency unless it earns its place. This project writes its own
  UUIDv4 rather than pull a crate for it.
- Logging: `use crate::log::{debug, info, warn, error};` — `error!` for things
  the user must know, `warn!` for things that degraded but recovered, `info!` for
  events, `debug!` for tracing.

## User interface

Follow the GNOME HIG and use libadwaita widgets, not hand-built equivalents.

- Header capitalisation for buttons, rows, titles: "Cloud Saves", not "Cloud
  saves". Sentence case for descriptions and subtitles.
- An ellipsis only when more input follows: "Move…" opens a picker, "Uninstall…"
  asks first, "Verify and Repair" acts at once and gets none.
- Strings are shown to the user, so they get the same care as code.

## Never

- Delete anything Divine did not create. Proton builds installed by hand, save
  files, game folders outside the install directory: leave them alone.
- Log or display a credential. The Epic exchange code appears in launch
  arguments — redact it.
- Guess how legendary or Epic behaves. Read legendary's source, capture the real
  answer, then write the code. A comment claiming behaviour nobody checked is
  worse than none.

## Before committing

Run all three; all three must be clean:

```
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

Test what would hurt a user: what deletes files, what loses settings, what
builds a command line. Do not test that a default is a default. Name tests as
sentences — `a_busy_game_keeps_its_files`, `hdr_never_travels_without_wayland` —
and use real data from a live answer, not invented shapes.

If you changed something with a visible effect, run the app and look at it. "It
compiles" is not "it works", and neither is worth reporting as done.

## Commits

One line, conventional, lower case after the type, no trailing full stop. Say
what the change does for the user, not which files moved.

```
feat: hold downloads to a speed limit through a local proxy
fix: refuse to sync saves before the prefix can say where they live
refactor: gather everything Divine asks Epic directly into one module
test: cover launch settings, config round-trips and what the cleanup deletes
```

Never commit unless asked to.

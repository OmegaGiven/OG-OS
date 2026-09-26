# omegagiven-search

Centered popup launcher for sway, replacing a top-bar launcher like
`wmenu-run`. Type a query and get ranked suggestions: the best-matching
installed app first, then a web search, then "Ask AI", then the rest of
the matching apps. Type `go/<alias>` to skip suggestions entirely and open
that as a direct link (not a search).

## What it does

- **Best app match** — ranked exact match > starts-with > contains, so the
  right app tends to land first once you've typed enough.
- **Web search** — opens your configured default browser with a search for
  whatever you typed.
- **Ask AI** — opens your configured terminal running your configured AI
  CLI with the query as a prompt.
- **`go/<alias>`** — opens `http://go/<alias>` directly in your browser, no
  search involved. Requires a `go` alias/shortcut service actually running
  on your network (see [galias](https://github.com/OmegaGiven) for a local
  equivalent) — if nothing answers at `go`, the browser will just fail to
  load it, same as any other bad address.
- Arrow keys move the selection, Enter launches it, Escape closes the
  popup.

## Requirements

- A sway session (uses `platform_specific.application_id` for window
  matching, and expects sway to float + center the window — see below)
- Rust/Cargo to build
- [sway-control](https://github.com/OmegaGiven) (or any app writing the
  same shared config) for a settings UI — optional, this app works fine
  reading just the shared config's defaults if that file doesn't exist yet

## Build & install

```sh
cargo build --release
cp target/release/omegagiven-search ~/.local/bin/
```

(Make sure `~/.local/bin` is on your `$PATH`.)

## Wire it into sway

Replace your existing launcher binding in `~/.config/sway/config`:

```
set $menu ~/.local/bin/omegagiven-search
bindsym $mod+d exec $menu
```

And add a floating + centered rule so it pops up as a small centered box
instead of tiling into the layout:

```
for_window [app_id="omegagiven-search"] floating enable, resize set 640 420, move position center
```

Then `swaymsg reload` (or restart sway).

## Configuration

Reads `~/.config/sway-power/config.json` — the same file
[sway-control](https://github.com/OmegaGiven)'s settings-manager writes.
Every field is optional and falls back to a sane default if the file is
missing or incomplete:

| Field | Purpose |
|---|---|
| `bar_bg`, `sec_bg`, `bar_text`, `accent` | Theme colors for the popup |
| `corner_radius` | Corner rounding (pixels), `0` = square |
| `color_variance_enabled`, `color_variance_amount` | Slightly tints this app's background differently from other apps sharing the same theme, so overlapping windows are easier to tell apart |
| `terminal` | Terminal emulator used for the "Ask AI" action |
| `default_browser` | Browser used for web search and `go/<alias>` |
| `default_ai_cli` | CLI command run for "Ask AI" (e.g. `claude`) |

If you don't have settings-manager, you can hand-write this file — only
the keys above are read, everything else in it is ignored.

## Notes on rendering

Uses `iced` with the `wgpu` renderer specifically — the `tiny-skia`
(software) renderer doesn't reliably support the real window transparency
this app depends on for its rounded/transparent popup background.

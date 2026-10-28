# keydock

Pin applications to Super+letter keys on wlroots compositors (labwc, niri, Hyprland, sway).

- **Super+letter** focuses the pinned app, hides it when it is already focused, or launches it.
  All of the app's windows come forward together and the most recently used one ends on top.
  Windows you minimized one by one stay minimized.
- **Hiding** minimizes the app's windows. Tiling compositors (niri, Hyprland, sway) ignore
  minimize requests; keydock notices on the first hide and switches to focusing the previous
  app instead, like Cmd+Tab. `keydock status` shows which behaviour is in use.
- **Super+/** shows an overlay with the pinned apps and the open, unpinned apps. Letters and
  numbers in the overlay switch to an app; Esc closes it.
- **An unpinned Super+letter** opens the overlay so you can pin an open app to that letter.

keydock tracks its own most-recently-used window order through
`wlr-foreign-toplevel-management`, so the behaviour does not depend on compositor internals.
The overlay is a layer-shell surface drawn with cairo and pango, with fractional scaling.
Its text follows the session language (`LANGUAGE`, `LC_ALL`, `LC_MESSAGES`, `LANG`):
English, Simplified Chinese and Traditional Chinese are included, with English as the fallback.

## Commands

```text
keydock key <letter>   focus, hide or launch the app pinned to <letter>
keydock overlay        show or hide the overlay
keydock status         list tracked windows, most recently used first
keydock reload         re-read desktop entries and icons
keydock daemon         run the background service (other commands start it if needed)
```

## Configuration

`~/.config/keydock/config.toml`:

```toml
font = "sans"          # optional overlay font
hide = "auto"          # auto | minimize | previous (focus the previous app)
key_label = "Super"    # modifier name shown in the overlay, e.g. "Hyper"
warp = "super+ctrl+alt+shift+p"  # chord for the compositor's cursor warp; "" disables it

[[app]]
key = "c"
app_id = "kitty"       # as shown by `keydock status`; "emacs" also matches "emacs-32-0-50"
exec = "kitty"         # launch command when no window exists
```

Overlay colors are read from `~/.config/keydock/colors.toml` each time the overlay opens.
Without the file a built-in dark palette is used. Any of these keys can be set:

```toml
surface = "#282828"
on_surface = "#fbf1c7"
on_surface_variant = "#ebdbb2"
primary = "#b8bb26"
outline = "#57514e"
```

With [Noctalia](https://github.com/noctalia-dev/noctalia), a user template keeps it in sync
with the shell palette. Put the file above in `~/.config/noctalia/templates/keydock.toml` with
values such as `"{{colors.primary.default.hex}}"`, and register it in Noctalia's config:

```toml
[theme.templates.user.keydock]
input_path = "$XDG_CONFIG_HOME/noctalia/templates/keydock.toml"
output_path = "$XDG_CONFIG_HOME/keydock/colors.toml"
```

## Compositor support

Requires `wlr-foreign-toplevel-management` and `wlr-layer-shell`; `virtual-keyboard` is used for
the cursor warp, and `fractional-scale` with `viewporter` for crisp text. Tested on labwc.
niri, Hyprland and sway implement these protocols but are untested; GNOME and KDE do not.

## Compositor setup (labwc)

Bind every free Super+letter to `keydock key <letter>` (on release), Super+/ to
`keydock overlay`, and add the cursor-warp callback keydock sends after focusing an app.
Any modifier works: the letter after `key` only names the app, so Hyper or Alt bindings are
fine. Set `key_label` to match, and keep the `warp` chord free of app bindings.

```xml
<keybind key="W-c" onRelease="yes"><action name="Execute" command="keydock key c" /></keybind>
<keybind key="W-slash"><action name="Execute" command="keydock overlay" /></keybind>
<keybind key="W-C-A-S-p"><action name="WarpCursor" to="window" x="center" y="center" /></keybind>
```

Start `keydock daemon` from the session autostart. Any other command starts it if needed.

## Other compositors (untested)

Bind the same commands; the cursor-warp chord is only needed where the compositor does not
move the pointer to the focused window by itself.

```text
# sway
bindsym --release Mod4+c exec keydock key c
bindsym Mod4+slash exec keydock overlay

# Hyprland
bindr = SUPER, C, exec, keydock key c
bind = SUPER, slash, exec, keydock overlay

# niri (binds section)
Mod+C { spawn "keydock" "key" "c"; }
Mod+Slash { spawn "keydock" "overlay"; }
```

## Build

```sh
cargo build --release && install -Dm755 target/release/keydock ~/.local/bin/keydock
```

Needs Rust 1.88 or newer, and cairo, pango and libxkbcommon.

## Development

```sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```

CI runs the same checks. The code is split so the decisions can be tested without a compositor:

| File | Role |
|---|---|
| `src/tracker.rs` | Window list in most-recently-used order and the show/hide decisions, returned as actions. No Wayland types; most tests live here. |
| `src/windows.rs` | Feeds foreign-toplevel events into the tracker and sends its actions to the compositor. |
| `src/config.rs` | Loading, validating and appending to `config.toml`. |
| `src/overlay.rs` | The layer-shell overlay, drawn with cairo and pango. |
| `src/desktop.rs` | Desktop entries and icon-theme lookup. |
| `src/i18n.rs` | Session language and overlay text. |
| `src/main.rs` | Command line, the daemon's socket and event loop, and wiring. |

## License

MIT, see [LICENSE](LICENSE).

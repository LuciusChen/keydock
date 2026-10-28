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

## Configuration

`~/.config/keydock/config.toml`:

```toml
font = "sans"          # optional overlay font
hide = "auto"          # auto | minimize | previous (focus the previous app)

[[app]]
key = "c"
app_id = "kitty"       # as shown by `keydock status`; "emacs" also matches "emacs-32-0-50"
exec = "kitty"         # launch command when no window exists
```

Overlay colors come from `~/.config/keydock/colors.toml` (`surface`, `on_surface`,
`on_surface_variant`, `primary`, `outline` as hex), which a Noctalia user template writes.

## Compositor support

Requires `wlr-foreign-toplevel-management` and `wlr-layer-shell`; `virtual-keyboard` is used for
the cursor warp, and `fractional-scale` with `viewporter` for crisp text. Tested on labwc.
niri, Hyprland and sway implement these protocols but are untested; GNOME and KDE do not.

## Compositor setup (labwc)

Bind every free Super+letter to `keydock key <letter>` (on release), Super+/ to
`keydock overlay`, and add the cursor-warp callback keydock sends after focusing an app:

```xml
<keybind key="W-c" onRelease="yes"><action name="Execute" command="keydock key c" /></keybind>
<keybind key="W-slash"><action name="Execute" command="keydock overlay" /></keybind>
<keybind key="W-C-A-S-p"><action name="WarpCursor" to="window" x="center" y="center" /></keybind>
```

Start `keydock daemon` from the session autostart. Any other command starts it if needed.

## Build

```sh
cargo build --release && install -Dm755 target/release/keydock ~/.local/bin/keydock
```

Needs cairo, pango and libxkbcommon at runtime.

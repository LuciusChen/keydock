//! keydock: pin applications to Super+letter keys on wlroots compositors.
//!
//! `keydock daemon` tracks windows and draws the overlay; every other
//! subcommand is a thin client that talks to it over a Unix socket.

mod config;
mod desktop;
mod i18n;
mod overlay;
mod tracker;
mod tray;
mod windows;

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_dispatch2, delegate_registry,
    dispatch2::Dispatch2,
    output::{OutputHandler, OutputState},
    reexports::calloop::{
        EventLoop, Interest, LoopHandle, Mode as IoMode, PostAction,
        generic::Generic,
        timer::{TimeoutAction, Timer},
    },
    reexports::calloop_wayland_source::WaylandSource,
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
    },
    shell::wlr_layer::{LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure},
    shm::{Shm, ShmHandler},
};
use wayland_client::{
    Connection, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_seat, wl_surface},
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1;

use config::Config;
use desktop::{DesktopIndex, IconFinder};
use overlay::{Content, Mode, Overlay, Row, Section};
use tracker::MinimizeCheck;
use windows::Windows;

const USAGE: &str = "usage: keydock <command>

  daemon        run the background service (started automatically)
  key <letter>  focus, hide or launch the app pinned to <letter>;
                an unpinned letter opens the overlay to pin an open app
  overlay       show or hide the overlay of pinned and open apps
  status        list tracked windows, most recently used first
  reload        re-read desktop entries and icons";

fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    dir.join("keydock.sock")
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("daemon") => match daemon() {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("keydock: {e}");
                1
            }
        },
        Some("-h" | "--help") | None => {
            println!("{USAGE}");
            if args.is_empty() { 2 } else { 0 }
        }
        Some(_) => client(&args.join(" ")),
    };
    std::process::exit(code);
}

/// Sends one command line to the daemon, starting it first if needed.
fn client(line: &str) -> i32 {
    let path = socket_path();
    let mut stream = UnixStream::connect(&path);
    if stream.is_err() {
        let exe = std::env::current_exe().unwrap_or_else(|_| "keydock".into());
        let _ = std::process::Command::new("setsid")
            .arg("-f")
            .arg(exe)
            .arg("daemon")
            .status();
        let start = Instant::now();
        while stream.is_err() && start.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(20));
            stream = UnixStream::connect(&path);
        }
    }
    let Ok(mut stream) = stream else {
        eprintln!("keydock: daemon is not running and could not be started");
        return 1;
    };
    if writeln!(stream, "{line}").is_err() {
        return 1;
    }
    let mut reply = Vec::new();
    let _ = stream.read_to_end(&mut reply);
    let reply = String::from_utf8_lossy(&reply);
    let reply = reply.trim_end();
    match reply.strip_prefix("error: ") {
        Some(err) => {
            eprintln!("keydock: {err}");
            1
        }
        None => {
            if !reply.is_empty() && reply != "ok" {
                println!("{reply}");
            }
            0
        }
    }
}

/// Commands the daemon accepts, one per connection.
#[derive(Debug, PartialEq, Eq)]
enum Request {
    Key(char),
    Overlay,
    Status,
    Reload,
}

impl Request {
    fn parse(line: &str) -> Result<Request, String> {
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            ["key", k] => match k.chars().collect::<Vec<_>>().as_slice() {
                [c] if c.is_ascii_alphabetic() => Ok(Request::Key(c.to_ascii_lowercase())),
                _ => Err(format!("not a letter: {k}")),
            },
            ["overlay"] => Ok(Request::Overlay),
            ["status"] => Ok(Request::Status),
            ["reload"] => Ok(Request::Reload),
            _ => Err(format!("unknown command: {line}\n{USAGE}")),
        }
    }
}

pub struct State {
    registry: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    pub shm: Shm,
    qh: QueueHandle<State>,
    conn: Connection,
    event_loop: LoopHandle<'static, State>,
    pub windows: Windows,
    pub overlay: Overlay,
    /// App ids behind the overlay's number keys, in display order.
    overlay_targets: Vec<String>,
    /// Counts overlay openings, so a pending auto-close skips a newer overlay.
    overlay_serial: u64,
    desktop: DesktopIndex,
    desktop_loaded: Instant,
    icons: IconFinder,
}

fn daemon() -> Result<(), String> {
    let path = socket_path();
    if UnixStream::connect(&path).is_ok() {
        return Err("already running".into());
    }
    let _ = std::fs::remove_file(&path);
    let listener =
        UnixListener::bind(&path).map_err(|e| format!("bind {}: {e}", path.display()))?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;

    let conn = Connection::connect_to_env().map_err(|e| format!("wayland: {e}"))?;
    let (globals, queue) =
        registry_queue_init::<State>(&conn).map_err(|e| format!("wayland: {e}"))?;
    let qh = queue.handle();
    let missing = |what: &str| format!("compositor does not support {what}");

    let overlay_globals = overlay::Globals {
        compositor: CompositorState::bind(&globals, &qh).map_err(|_| missing("wl_compositor"))?,
        layer_shell: LayerShell::bind(&globals, &qh).map_err(|_| missing("wlr-layer-shell"))?,
        fractional: globals.bind(&qh, 1..=1, Noop).ok(),
        viewporter: globals.bind(&qh, 1..=1, Noop).ok(),
    };
    let shm = Shm::bind(&globals, &qh).map_err(|_| missing("wl_shm"))?;
    let _toplevels: ZwlrForeignToplevelManagerV1 = globals
        .bind(&qh, 1..=3, windows::ToplevelManager)
        .map_err(|_| missing("wlr-foreign-toplevel-management"))?;
    let vk_manager = globals.bind(&qh, 1..=1, Noop).ok();

    let mut event_loop: EventLoop<State> = EventLoop::try_new().map_err(|e| e.to_string())?;
    let mut state = State {
        registry: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        shm,
        qh: qh.clone(),
        conn: conn.clone(),
        event_loop: event_loop.handle(),
        windows: Windows::new(vk_manager),
        overlay: Overlay::new(overlay_globals),
        overlay_targets: Vec::new(),
        overlay_serial: 0,
        desktop: DesktopIndex::load(),
        desktop_loaded: Instant::now(),
        icons: IconFinder::new(),
    };

    WaylandSource::new(conn, queue)
        .insert(event_loop.handle())
        .map_err(|e| e.to_string())?;
    event_loop
        .handle()
        .insert_source(
            Generic::new(listener, Interest::READ, IoMode::Level),
            |_, listener, state| {
                while let Ok((stream, _)) = listener.accept() {
                    state.serve(stream);
                }
                Ok(PostAction::Continue)
            },
        )
        .map_err(|e| e.to_string())?;

    // Pick up existing seats and windows before serving commands.
    event_loop
        .dispatch(Duration::from_millis(100), &mut state)
        .map_err(|e| e.to_string())?;
    if let Some(seat) = state.seat_state.seats().next() {
        state.windows.set_seat(seat);
    }
    loop {
        event_loop
            .dispatch(None, &mut state)
            .map_err(|e| e.to_string())?;
    }
}

impl State {
    fn serve(&mut self, stream: UnixStream) {
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
        let mut line = String::new();
        if BufReader::new(&stream).read_line(&mut line).is_err() {
            return;
        }
        let reply = match self.command(line.trim()) {
            Ok(text) if text.is_empty() => "ok".to_string(),
            Ok(text) => text,
            Err(e) => format!("error: {e}"),
        };
        let _ = (&stream).write_all(reply.as_bytes());
        let _ = self.conn.flush();
    }

    fn command(&mut self, line: &str) -> Result<String, String> {
        match Request::parse(line)? {
            Request::Key(key) => self.key(key)?,
            Request::Overlay if self.overlay.is_open() => self.overlay.close(),
            Request::Overlay => self.open_overlay(Mode::Browse)?,
            Request::Status => return Ok(self.status()),
            Request::Reload => self.reload_desktop(),
        }
        Ok(String::new())
    }

    fn key(&mut self, key: char) -> Result<(), String> {
        let cfg = Config::load()?;
        self.overlay.close();
        let Some(app) = cfg.by_key(key) else {
            return self.open_overlay(Mode::Assign(key));
        };
        let (actions, check) =
            self.windows
                .tracker
                .toggle(&app.app_id, app.exec.as_deref(), cfg.hide)?;
        self.windows.run(&self.qh, &actions, cfg.warp())?;
        if let Some(check) = check {
            self.verify_minimize(check, cfg.warp().map(str::to_string));
        }
        Ok(())
    }

    /// Gives the compositor time to report the minimized state, then lets the
    /// tracker fall back to focusing the previous app if it did not.
    fn verify_minimize(&mut self, check: MinimizeCheck, warp: Option<String>) {
        let timer = Timer::from_duration(Duration::from_millis(150));
        let _ = self.event_loop.insert_source(timer, move |_, _, state| {
            let actions = state.windows.tracker.check_minimized(&check);
            if let Err(e) = state.windows.run(&state.qh, &actions, warp.as_deref()) {
                eprintln!("keydock: {e}");
            }
            let _ = state.conn.flush();
            TimeoutAction::Drop
        });
    }

    fn status(&self) -> String {
        let minimize = match self.windows.tracker.minimize_works {
            Some(true) => "supported",
            Some(false) => "ignored by compositor, hiding focuses the previous app",
            None => "not tested yet",
        };
        let windows = self
            .windows
            .tracker
            .list
            .iter()
            .map(|w| {
                let flags = match (w.activated, w.minimized) {
                    (true, _) => " [active]",
                    (_, true) => " [minimized]",
                    _ => "",
                };
                format!("{}\t{}{}", w.app_id, w.title, flags)
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!("# minimize: {minimize}\n{windows}")
    }

    fn reload_desktop(&mut self) {
        self.desktop = DesktopIndex::load();
        self.desktop_loaded = Instant::now();
        self.icons = IconFinder::new();
    }

    /// Open apps without a pinned letter, most recently used first.
    fn unpinned(&self, cfg: &Config) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for w in &self.windows.tracker.list {
            if w.app_id.is_empty() || cfg.by_app_id(&w.app_id).is_some() || out.contains(&w.app_id)
            {
                continue;
            }
            out.push(w.app_id.clone());
        }
        out.truncate(9);
        out
    }

    fn row(&mut self, key: char, app_id: &str, name: Option<&str>, temporary: bool) -> Row {
        let entry = self.desktop.lookup(app_id).cloned();
        let icon = entry
            .as_ref()
            .and_then(|e| e.icon.as_deref())
            .and_then(|i| self.icons.find(i));
        let name = name
            .map(str::to_string)
            .or(entry.map(|e| e.name))
            .unwrap_or_else(|| app_id.to_string());
        let running = self.windows.tracker.of_app(app_id).next().is_some();
        Row {
            key,
            name,
            icon,
            running,
            temporary,
        }
    }

    fn open_overlay(&mut self, mode: Mode) -> Result<(), String> {
        let cfg = Config::load()?;
        if self.desktop_loaded.elapsed() > Duration::from_secs(60) {
            self.reload_desktop();
        }
        let targets = self.unpinned(&cfg);
        let numbered: Vec<Row> = targets
            .iter()
            .enumerate()
            .map(|(i, id)| self.row(char::from(b'1' + i as u8), id, None, true))
            .collect();
        let text = i18n::strings(&i18n::languages());
        // An unpinned key with nothing to pin: say so and close on its own.
        let nothing_to_pin = matches!(mode, Mode::Assign(_)) && targets.is_empty();
        let content = match mode {
            Mode::Browse => {
                let pinned: Vec<Row> = cfg
                    .apps
                    .iter()
                    .map(|a| self.row(a.key, &a.app_id, a.name.as_deref(), false))
                    .collect();
                Content {
                    title: None,
                    sections: vec![
                        Section {
                            title: text.pinned.into(),
                            rows: pinned,
                        },
                        Section {
                            title: text.open.into(),
                            rows: numbered,
                        },
                    ],
                    hint: text.browse_hint.into(),
                    empty: text.none.into(),
                }
            }
            Mode::Assign(k) => Content {
                title: Some(text.unassigned.replace(
                    "{key}",
                    &format!("{}+{}", cfg.key_label(), k.to_ascii_uppercase()),
                )),
                sections: if nothing_to_pin {
                    Vec::new()
                } else {
                    vec![Section {
                        title: text.assign_section.into(),
                        rows: numbered,
                    }]
                },
                hint: if nothing_to_pin {
                    text.assign_nothing.into()
                } else {
                    text.assign_hint.into()
                },
                empty: text.none.into(),
            },
        };
        self.overlay_targets = targets;
        self.overlay_serial += 1;
        self.overlay.open(&self.qh, mode, content, &cfg);
        if nothing_to_pin {
            self.close_overlay_later();
        }
        Ok(())
    }

    /// Closes the overlay shortly, unless another one opened meanwhile.
    fn close_overlay_later(&mut self) {
        let serial = self.overlay_serial;
        let delay = Duration::from_millis(1500);
        let _ = self
            .event_loop
            .insert_source(Timer::from_duration(delay), move |_, _, state| {
                if state.overlay_serial == serial {
                    state.overlay.close();
                    let _ = state.conn.flush();
                }
                TimeoutAction::Drop
            });
    }

    /// A key typed into the overlay: Esc closes it, letters and numbers
    /// switch to an app, and in assign mode a number pins that app.
    fn overlay_key(&mut self, event: &KeyEvent) {
        if event.keysym == Keysym::Escape {
            self.overlay.close();
            return;
        }
        let Some(c) = event.utf8.as_deref().and_then(|s| s.chars().next()) else {
            return;
        };
        if let Err(e) = self.overlay_choice(c.to_ascii_lowercase()) {
            eprintln!("keydock: {e}");
        }
    }

    fn overlay_choice(&mut self, c: char) -> Result<(), String> {
        let cfg = Config::load()?;
        let numbered = c
            .to_digit(10)
            .and_then(|d| self.overlay_targets.get((d as usize).checked_sub(1)?))
            .cloned();
        let (app_id, exec) = match (&self.overlay.mode, numbered) {
            (Some(Mode::Assign(key)), Some(app_id)) => {
                let exec = self
                    .desktop
                    .lookup(&app_id)
                    .map(|e| format!("gtk-launch {}", e.id));
                Config::append_to(&Config::path(), *key, &app_id, exec.as_deref())
                    .map_err(|e| e.to_string())?;
                (app_id, None)
            }
            (Some(Mode::Browse), Some(app_id)) => (app_id, None),
            (Some(Mode::Browse), None) => match cfg.by_key(c) {
                Some(app) => (app.app_id.clone(), app.exec.clone()),
                None => return Ok(()),
            },
            _ => return Ok(()),
        };
        self.overlay.close();
        let actions = self.windows.tracker.show(&app_id, exec.as_deref())?;
        self.windows.run(&self.qh, &actions, cfg.warp())
    }
}

impl CompositorHandler for State {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: i32,
    ) {
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for State {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.overlay.close();
    }
    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &LayerSurface,
        _: LayerSurfaceConfigure,
        _: u32,
    ) {
        self.overlay.configured(&self.shm);
    }
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        self.windows.set_seat(seat);
    }
    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        cap: Capability,
    ) {
        if cap == Capability::Keyboard {
            let _ = self.seat_state.get_keyboard(qh, &seat, None);
        }
    }
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        _: Capability,
    ) {
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for State {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
    }
    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        if self.overlay.owns(surface) {
            self.overlay.close();
        }
    }
    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        if self.overlay.is_open() {
            self.overlay_key(&event);
        }
    }
    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }
    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }
    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: Modifiers,
        _: RawModifiers,
        _: u32,
    ) {
    }
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

delegate_registry!(State);
delegate_dispatch2!(State);

/// User data for protocol objects whose events keydock ignores.
pub struct Noop;

impl<I: wayland_client::Proxy> Dispatch2<I, State> for Noop {
    fn event(&self, _: &mut State, _: &I, _: I::Event, _: &Connection, _: &QueueHandle<State>) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_requests() {
        assert_eq!(Request::parse("key C"), Ok(Request::Key('c')));
        assert_eq!(Request::parse("  overlay "), Ok(Request::Overlay));
        assert_eq!(Request::parse("status"), Ok(Request::Status));
        assert!(Request::parse("key 1").is_err());
        assert!(Request::parse("key ab").is_err());
        assert!(Request::parse("key").is_err());
        assert!(Request::parse("overlay now").is_err());
    }
}

//! Window tracking through wlr-foreign-toplevel-management, plus the
//! show/hide logic that keeps per-app window order stable.
//!
//! keydock keeps its own most-recently-used list instead of relying on the
//! compositor's stacking order, so the behaviour does not depend on
//! compositor internals.

use std::collections::HashSet;
use std::io::Write;
use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::time::Instant;

use wayland_client::protocol::wl_seat::WlSeat;
use std::sync::Arc;

use smithay_client_toolkit::dispatch2::Dispatch2;
use wayland_client::backend::ObjectData;
use wayland_client::{Connection, Proxy, QueueHandle};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1, zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{self as handle, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self as manager, ZwlrForeignToplevelManagerV1},
};

use crate::State;
use crate::config::{Hide, app_id_matches};

pub struct Window {
    pub handle: ZwlrForeignToplevelHandleV1,
    pub app_id: String,
    pub title: String,
    pub activated: bool,
    pub minimized: bool,
}

/// Super+Ctrl+Alt+Shift+P: a compositor keybind that warps the pointer to the
/// active window. The foreign-toplevel protocol exposes no geometry, so the
/// compositor has to do the warp.
const WARP_KEY: u32 = 25; // evdev KEY_P

pub struct Windows {
    /// Most recently used first. Windows never activated sit at the end.
    pub list: Vec<Window>,
    /// Windows keydock minimized as part of hiding a whole app.
    hidden: HashSet<u32>,
    /// Apps launched recently, to ignore repeated presses while they start.
    launching: Vec<(String, Instant)>,
    seat: Option<WlSeat>,
    vk_manager: Option<ZwpVirtualKeyboardManagerV1>,
    vk: Option<(ZwpVirtualKeyboardV1, u32)>,
    time: Instant,
    /// Learned on the first automatic hide: some compositors ignore minimize requests.
    pub minimize_works: Option<bool>,
}

/// Windows whose minimize request still needs confirming (see `Hide::Auto`).
pub struct MinimizeCheck {
    pub app_id: String,
    pub windows: Vec<u32>,
}

fn key_of(handle: &ZwlrForeignToplevelHandleV1) -> u32 {
    handle.id().protocol_id()
}

impl Windows {
    pub fn new(vk_manager: Option<ZwpVirtualKeyboardManagerV1>) -> Self {
        Windows {
            list: Vec::new(),
            hidden: HashSet::new(),
            launching: Vec::new(),
            seat: None,
            vk_manager,
            vk: None,
            time: Instant::now(),
            minimize_works: None,
        }
    }

    pub fn set_seat(&mut self, seat: WlSeat) {
        self.seat = Some(seat);
    }

    fn index(&self, h: &ZwlrForeignToplevelHandleV1) -> Option<usize> {
        self.list.iter().position(|w| &w.handle == h)
    }

    pub fn active(&self) -> Option<&Window> {
        self.list.iter().find(|w| w.activated)
    }

    /// Windows of an app, most recently used first.
    pub fn of_app<'a>(&'a self, app_id: &'a str) -> impl Iterator<Item = &'a Window> + 'a {
        self.list.iter().filter(move |w| app_id_matches(app_id, &w.app_id))
    }

    /// Focuses the app, hides it when it is already focused, or launches it.
    /// Returns windows to re-check when minimize support is not known yet.
    pub fn toggle(
        &mut self,
        qh: &QueueHandle<State>,
        app_id: &str,
        exec: Option<&str>,
        hide: Hide,
    ) -> Result<Option<MinimizeCheck>, String> {
        let focused = self.active().is_some_and(|w| app_id_matches(app_id, &w.app_id));
        if focused {
            Ok(self.hide(qh, app_id, hide))
        } else {
            self.show(qh, app_id, exec).map(|_| None)
        }
    }

    /// Brings all of an app's windows forward, most recent on top, or launches it.
    pub fn show(&mut self, qh: &QueueHandle<State>, app_id: &str, exec: Option<&str>) -> Result<(), String> {
        let wins: Vec<(ZwlrForeignToplevelHandleV1, bool)> =
            self.of_app(app_id).map(|w| (w.handle.clone(), w.minimized)).collect();
        if wins.is_empty() {
            return self.launch(app_id, exec);
        }
        let Some(seat) = self.seat.clone() else { return Err("no seat".into()) };
        // Windows the user minimized one by one stay minimized (like macOS),
        // unless every window is minimized; then the most recent one returns.
        let mut targets: Vec<_> = wins
            .iter()
            .filter(|(h, minimized)| !minimized || self.hidden.contains(&key_of(h)))
            .cloned()
            .collect();
        if targets.is_empty() {
            targets.push(wins[0].clone());
        }
        // Activate least recent first so the most recent ends up on top.
        for (h, minimized) in targets.iter().rev() {
            if *minimized {
                h.unset_minimized();
            }
            h.activate(&seat);
            self.hidden.remove(&key_of(h));
        }
        self.warp_pointer(qh);
        Ok(())
    }

    /// Hides the app: minimizes its windows, or focuses the previous app
    /// where minimizing is unsupported or not wanted.
    pub fn hide(&mut self, qh: &QueueHandle<State>, app_id: &str, mode: Hide) -> Option<MinimizeCheck> {
        let minimize = match mode {
            Hide::Minimize => true,
            Hide::Previous => false,
            Hide::Auto => self.minimize_works != Some(false),
        };
        if !minimize {
            self.focus_previous(qh, app_id);
            return None;
        }
        let keys = self.minimize_all(app_id);
        (mode == Hide::Auto && self.minimize_works.is_none() && !keys.is_empty())
            .then(|| MinimizeCheck { app_id: app_id.to_string(), windows: keys })
    }

    /// Minimizes every window of the app, the focused one last so focus
    /// moves to another app instead of a sibling window.
    fn minimize_all(&mut self, app_id: &str) -> Vec<u32> {
        let mut wins: Vec<(ZwlrForeignToplevelHandleV1, bool)> = self
            .of_app(app_id)
            .filter(|w| !w.minimized)
            .map(|w| (w.handle.clone(), w.activated))
            .collect();
        wins.sort_by_key(|(_, active)| *active);
        let mut keys = Vec::new();
        for (h, _) in wins {
            h.set_minimized();
            self.hidden.insert(key_of(&h));
            keys.push(key_of(&h));
        }
        keys
    }

    /// Called shortly after an automatic hide: if none of the windows became
    /// minimized, the compositor ignores minimize requests, so remember that
    /// and fall back to focusing the previous app.
    pub fn check_minimized(&mut self, qh: &QueueHandle<State>, check: MinimizeCheck) {
        if self.minimize_works.is_some() {
            return;
        }
        let tracked: Vec<&Window> = self.list.iter().filter(|w| check.windows.contains(&key_of(&w.handle))).collect();
        if tracked.is_empty() {
            return; // windows closed meanwhile; decide next time
        }
        if tracked.iter().any(|w| w.minimized) {
            self.minimize_works = Some(true);
        } else {
            self.minimize_works = Some(false);
            for k in &check.windows {
                self.hidden.remove(k);
            }
            self.focus_previous(qh, &check.app_id);
        }
    }

    /// Focuses the most recently used visible window of any other app.
    fn focus_previous(&mut self, qh: &QueueHandle<State>, app_id: &str) {
        let Some(seat) = self.seat.clone() else { return };
        let target = self
            .list
            .iter()
            .find(|w| !w.minimized && !w.app_id.is_empty() && !app_id_matches(app_id, &w.app_id))
            .map(|w| w.handle.clone());
        if let Some(h) = target {
            h.activate(&seat);
            self.warp_pointer(qh);
        }
    }

    fn launch(&mut self, app_id: &str, exec: Option<&str>) -> Result<(), String> {
        let Some(exec) = exec else { return Err(format!("{app_id} is not running and has no exec")) };
        self.launching.retain(|(_, t)| t.elapsed().as_secs() < 5);
        if self.launching.iter().any(|(a, _)| a == app_id) {
            return Ok(());
        }
        // setsid -f detaches immediately, so no zombie is left behind.
        std::process::Command::new("setsid")
            .args(["-f", "sh", "-c", exec])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|e| format!("launch {exec}: {e}"))?;
        self.launching.push((app_id.to_string(), Instant::now()));
        Ok(())
    }

    /// Sends the warp chord on the same connection right after the activate
    /// requests, so the compositor handles it after focus has moved.
    fn warp_pointer(&mut self, qh: &QueueHandle<State>) {
        let Some(vk) = self.virtual_keyboard(qh) else { return };
        let (vk, mods) = (vk.0.clone(), vk.1);
        let t = self.time.elapsed().as_millis() as u32;
        vk.modifiers(mods, 0, 0, 0);
        vk.key(t, WARP_KEY, 1);
        vk.key(t + 1, WARP_KEY, 0);
        vk.modifiers(0, 0, 0, 0);
    }

    fn virtual_keyboard(&mut self, qh: &QueueHandle<State>) -> Option<&(ZwpVirtualKeyboardV1, u32)> {
        if self.vk.is_none() {
            let (manager, seat) = (self.vk_manager.as_ref()?, self.seat.as_ref()?);
            let (fd, size, mods) = us_keymap()?;
            let vk = manager.create_virtual_keyboard(seat, qh, crate::Noop);
            vk.keymap(1, fd.as_fd(), size);
            self.vk = Some((vk, mods));
        }
        self.vk.as_ref()
    }
}

/// A US keymap in a memfd, plus the Shift|Control|Mod1|Mod4 mask.
fn us_keymap() -> Option<(OwnedFd, u32, u32)> {
    use xkbcommon::xkb;
    let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    let keymap = xkb::Keymap::new_from_names(&ctx, "", "", "us", "", None, xkb::KEYMAP_COMPILE_NO_FLAGS)?;
    let mut text = keymap.get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1).into_bytes();
    text.push(0);
    let mask = ["Shift", "Control", "Mod1", "Mod4"]
        .iter()
        .map(|m| keymap.mod_get_index(m))
        .filter(|&i| i != xkb::MOD_INVALID)
        .fold(0u32, |acc, i| acc | (1 << i));
    let fd = unsafe { libc::memfd_create(c"keydock-keymap".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return None;
    }
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    file.write_all(&text).ok()?;
    Some((file.into(), text.len() as u32, mask))
}

/// User data for the foreign-toplevel manager.
pub struct ToplevelManager;
/// User data for each foreign-toplevel handle.
pub struct Toplevel;

impl Dispatch2<ZwlrForeignToplevelManagerV1, State> for ToplevelManager {
    fn event(
        &self,
        state: &mut State,
        _: &ZwlrForeignToplevelManagerV1,
        event: manager::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let manager::Event::Toplevel { toplevel } = event {
            state.windows.list.push(Window {
                handle: toplevel,
                app_id: String::new(),
                title: String::new(),
                activated: false,
                minimized: false,
            });
        }
    }

    fn event_created_child(opcode: u16, qh: &QueueHandle<State>) -> Arc<dyn ObjectData> {
        assert_eq!(opcode, manager::EVT_TOPLEVEL_OPCODE);
        qh.make_data::<ZwlrForeignToplevelHandleV1, _>(Toplevel)
    }
}

impl Dispatch2<ZwlrForeignToplevelHandleV1, State> for Toplevel {
    fn event(
        &self,
        state: &mut State,
        h: &ZwlrForeignToplevelHandleV1,
        event: handle::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        let ws = &mut state.windows;
        let Some(i) = ws.index(h) else { return };
        match event {
            handle::Event::Title { title } => ws.list[i].title = title,
            handle::Event::AppId { app_id } => {
                ws.launching.retain(|(a, _)| !app_id_matches(a, &app_id));
                ws.list[i].app_id = app_id;
            }
            handle::Event::State { state: raw } => {
                let states: Vec<u32> =
                    raw.chunks_exact(4).map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]])).collect();
                let activated = states.contains(&(handle::State::Activated as u32));
                let minimized = states.contains(&(handle::State::Minimized as u32));
                let was_activated = ws.list[i].activated;
                ws.list[i].activated = activated;
                ws.list[i].minimized = minimized;
                if !minimized {
                    ws.hidden.remove(&key_of(h));
                }
                if activated && !was_activated {
                    let w = ws.list.remove(i);
                    ws.list.insert(0, w);
                }
            }
            handle::Event::Closed => {
                ws.hidden.remove(&key_of(h));
                ws.list.remove(i);
                h.destroy();
            }
            _ => {}
        }
    }
}

//! The Wayland side of window management: feeds foreign-toplevel events into
//! the [`Tracker`] and carries out the [`Action`]s it decides on.

use std::collections::HashMap;
use std::io::Write;
use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Instant;

use smithay_client_toolkit::dispatch2::Dispatch2;
use wayland_client::backend::ObjectData;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Proxy, QueueHandle};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{self as handle, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self as manager, ZwlrForeignToplevelManagerV1},
};
use xkbcommon::xkb;

use crate::State;
use crate::tracker::{Action, Id, Tracker};

pub struct Windows {
    pub tracker: Tracker,
    handles: HashMap<Id, ZwlrForeignToplevelHandleV1>,
    seat: Option<WlSeat>,
    vk_manager: Option<ZwpVirtualKeyboardManagerV1>,
    vk: Option<(ZwpVirtualKeyboardV1, xkb::Keymap)>,
    started: Instant,
}

impl Windows {
    pub fn new(vk_manager: Option<ZwpVirtualKeyboardManagerV1>) -> Self {
        Windows {
            tracker: Tracker::default(),
            handles: HashMap::new(),
            seat: None,
            vk_manager,
            vk: None,
            started: Instant::now(),
        }
    }

    pub fn set_seat(&mut self, seat: WlSeat) {
        self.seat = Some(seat);
    }

    /// Sends the actions to the compositor. `warp` is the chord for
    /// [`Action::Warp`]; None skips it.
    pub fn run(
        &mut self,
        qh: &QueueHandle<State>,
        actions: &[Action],
        warp: Option<&str>,
    ) -> Result<(), String> {
        for action in actions {
            match action {
                Action::Activate(id) => {
                    let seat = self.seat.as_ref().ok_or("no seat")?;
                    self.handle(*id)?.activate(seat);
                }
                Action::Unminimize(id) => self.handle(*id)?.unset_minimized(),
                Action::Minimize(id) => self.handle(*id)?.set_minimized(),
                Action::Launch(exec) => launch(exec)?,
                Action::Warp => {
                    if let Some(chord) = warp {
                        self.send_chord(qh, chord)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn handle(&self, id: Id) -> Result<&ZwlrForeignToplevelHandleV1, String> {
        self.handles
            .get(&id)
            .ok_or_else(|| format!("window {id} is gone"))
    }

    /// Types the chord on a virtual keyboard. The foreign-toplevel protocol
    /// has no window geometry, so the compositor binds this chord to its own
    /// "warp pointer to the focused window" action. Being on the same
    /// connection, it arrives after the activate requests.
    fn send_chord(&mut self, qh: &QueueHandle<State>, chord: &str) -> Result<(), String> {
        let time = self.started.elapsed().as_millis() as u32;
        let (vk, keymap) = self
            .virtual_keyboard(qh)
            .ok_or("no virtual keyboard support")?;
        let (code, mods) = parse_chord(keymap, chord)
            .ok_or_else(|| format!("cannot parse warp chord {chord:?}"))?;
        vk.modifiers(mods, 0, 0, 0);
        vk.key(time, code, 1);
        vk.key(time, code, 0);
        vk.modifiers(0, 0, 0, 0);
        Ok(())
    }

    fn virtual_keyboard(
        &mut self,
        qh: &QueueHandle<State>,
    ) -> Option<&(ZwpVirtualKeyboardV1, xkb::Keymap)> {
        if self.vk.is_none() {
            let (manager, seat) = (self.vk_manager.as_ref()?, self.seat.as_ref()?);
            let keymap = us_keymap()?;
            let (fd, size) = keymap_fd(&keymap)?;
            let vk = manager.create_virtual_keyboard(seat, qh, crate::Noop);
            vk.keymap(xkb::KEYMAP_FORMAT_TEXT_V1, fd.as_fd(), size);
            self.vk = Some((vk, keymap));
        }
        self.vk.as_ref()
    }
}

/// Starts a shell command fully detached, so no zombie is left behind.
fn launch(exec: &str) -> Result<(), String> {
    Command::new("setsid")
        .args(["-f", "sh", "-c", exec])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|_| ())
        .map_err(|e| format!("launch {exec}: {e}"))
}

fn us_keymap() -> Option<xkb::Keymap> {
    let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    xkb::Keymap::new_from_names(&ctx, "", "", "us", "", None, xkb::KEYMAP_COMPILE_NO_FLAGS)
}

/// The keymap as a NUL-terminated string in a memfd, as wl_keyboard expects.
fn keymap_fd(keymap: &xkb::Keymap) -> Option<(OwnedFd, u32)> {
    let mut text = keymap
        .get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1)
        .into_bytes();
    text.push(0);
    // SAFETY: memfd_create returns a new descriptor that nothing else owns.
    let fd = unsafe { libc::memfd_create(c"keydock-keymap".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return None;
    }
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    file.write_all(&text).ok()?;
    Some((file.into(), text.len() as u32))
}

/// Parses a chord like "super+ctrl+alt+shift+p" into an evdev key code and a
/// modifier mask for `keymap`.
fn parse_chord(keymap: &xkb::Keymap, chord: &str) -> Option<(u32, u32)> {
    let (mut mask, mut key) = (0u32, None);
    for part in chord.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        let modifier = match part.to_lowercase().as_str() {
            "super" | "logo" | "win" | "mod4" => Some(xkb::MOD_NAME_LOGO),
            "ctrl" | "control" => Some(xkb::MOD_NAME_CTRL),
            "alt" | "mod1" => Some(xkb::MOD_NAME_ALT),
            "shift" => Some(xkb::MOD_NAME_SHIFT),
            _ => None,
        };
        match modifier {
            Some(name) => mask |= 1 << keymap.mod_get_index(name),
            None if key.is_none() => key = Some(part),
            None => return None,
        }
    }
    let sym = xkb::keysym_from_name(key?, xkb::KEYSYM_CASE_INSENSITIVE);
    if sym.raw() == 0 {
        return None;
    }
    // The key producing the symbol at the base level; evdev codes are xkb codes - 8.
    (keymap.min_keycode().raw()..=keymap.max_keycode().raw())
        .find(|&code| {
            keymap
                .key_get_syms_by_level(code.into(), 0, 0)
                .contains(&sym)
        })
        .map(|code| (code - 8, mask))
}

fn id_of(handle: &ZwlrForeignToplevelHandleV1) -> Id {
    handle.id().protocol_id()
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
            let id = id_of(&toplevel);
            state.windows.tracker.add(id);
            state.windows.handles.insert(id, toplevel);
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
        handle: &ZwlrForeignToplevelHandleV1,
        event: handle::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        let (id, windows) = (id_of(handle), &mut state.windows);
        match event {
            handle::Event::Title { title } => windows.tracker.set_title(id, title),
            handle::Event::AppId { app_id } => windows.tracker.set_app_id(id, app_id),
            handle::Event::State { state: raw } => {
                let has =
                    |s: handle::State| raw.as_chunks::<4>().0.contains(&(s as u32).to_ne_bytes());
                windows.tracker.set_state(
                    id,
                    has(handle::State::Activated),
                    has(handle::State::Minimized),
                );
            }
            handle::Event::Closed => {
                windows.tracker.remove(id);
                windows.handles.remove(&id);
                handle.destroy();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_chords() {
        let keymap = us_keymap().unwrap();
        let bit = |name| 1 << keymap.mod_get_index(name);
        let all = bit(xkb::MOD_NAME_LOGO)
            | bit(xkb::MOD_NAME_CTRL)
            | bit(xkb::MOD_NAME_ALT)
            | bit(xkb::MOD_NAME_SHIFT);

        const KEY_P: u32 = 25;
        const KEY_F12: u32 = 88;
        assert_eq!(
            parse_chord(&keymap, "super+ctrl+alt+shift+p"),
            Some((KEY_P, all))
        );
        assert_eq!(
            parse_chord(&keymap, "Alt + Super + F12"),
            Some((KEY_F12, bit(xkb::MOD_NAME_ALT) | bit(xkb::MOD_NAME_LOGO)))
        );
        assert_eq!(parse_chord(&keymap, "super+p+q"), None);
        assert_eq!(parse_chord(&keymap, "super+nosuchkey"), None);
        assert_eq!(parse_chord(&keymap, "super"), None);
    }
}

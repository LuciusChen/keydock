//! Window bookkeeping and the show/hide decisions.
//!
//! Nothing here touches Wayland: methods return the [`Action`]s to perform,
//! and `windows.rs` sends them to the compositor. keydock keeps its own
//! most-recently-used order so the behaviour does not depend on how a
//! compositor stacks windows.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use crate::config::{Hide, app_id_matches};

/// Protocol id of a foreign-toplevel handle.
pub type Id = u32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Activate(Id),
    Unminimize(Id),
    Minimize(Id),
    /// Restore the app from its tray icon, or run the shell command to start it.
    Launch {
        app_id: String,
        exec: String,
    },
    /// Ask the compositor to move the pointer to the focused window.
    Warp,
}

pub struct Window {
    pub id: Id,
    pub app_id: String,
    pub title: String,
    pub activated: bool,
    pub minimized: bool,
}

/// Windows an automatic hide tried to minimize; see [`Tracker::check_minimized`].
pub struct MinimizeCheck {
    pub app_id: String,
    pub windows: Vec<Id>,
}

/// Ignore repeated presses for an app that is still starting.
const LAUNCH_GRACE: Duration = Duration::from_secs(5);

#[derive(Default)]
pub struct Tracker {
    /// Most recently used first; windows never focused sit at the end.
    pub list: Vec<Window>,
    /// Windows minimized by hiding a whole app; showing the app restores them.
    hidden: HashSet<Id>,
    launching: Vec<(String, Instant)>,
    /// Learned on the first automatic hide: tiling compositors ignore minimize.
    pub minimize_works: Option<bool>,
}

impl Tracker {
    pub fn add(&mut self, id: Id) {
        self.list.push(Window {
            id,
            app_id: String::new(),
            title: String::new(),
            activated: false,
            minimized: false,
        });
    }

    pub fn remove(&mut self, id: Id) {
        self.list.retain(|w| w.id != id);
        self.hidden.remove(&id);
    }

    pub fn set_title(&mut self, id: Id, title: String) {
        if let Some(w) = self.get_mut(id) {
            w.title = title;
        }
    }

    pub fn set_app_id(&mut self, id: Id, app_id: String) {
        self.launching.retain(|(a, _)| !app_id_matches(a, &app_id));
        if let Some(w) = self.get_mut(id) {
            w.app_id = app_id;
        }
    }

    /// Records the compositor's state; a newly focused window moves to the front.
    pub fn set_state(&mut self, id: Id, activated: bool, minimized: bool) {
        let Some(i) = self.list.iter().position(|w| w.id == id) else {
            return;
        };
        let was_activated = self.list[i].activated;
        self.list[i].activated = activated;
        self.list[i].minimized = minimized;
        if !minimized {
            self.hidden.remove(&id);
        }
        if activated && !was_activated {
            let w = self.list.remove(i);
            self.list.insert(0, w);
        }
    }

    fn get_mut(&mut self, id: Id) -> Option<&mut Window> {
        self.list.iter_mut().find(|w| w.id == id)
    }

    pub fn active(&self) -> Option<&Window> {
        self.list.iter().find(|w| w.activated)
    }

    /// Windows of an app, most recently used first.
    pub fn of_app<'a>(&'a self, app_id: &'a str) -> impl Iterator<Item = &'a Window> + 'a {
        self.list
            .iter()
            .filter(move |w| app_id_matches(app_id, &w.app_id))
    }

    /// Focuses the app, hides it when it is already focused, or launches it.
    /// A returned check must be passed to [`Tracker::check_minimized`] shortly after.
    pub fn toggle(
        &mut self,
        app_id: &str,
        exec: Option<&str>,
        hide: Hide,
    ) -> Result<(Vec<Action>, Option<MinimizeCheck>), String> {
        if self
            .active()
            .is_some_and(|w| app_id_matches(app_id, &w.app_id))
        {
            Ok(self.hide(app_id, hide))
        } else {
            Ok((self.show(app_id, exec)?, None))
        }
    }

    /// Brings all of an app's windows forward with the most recent on top,
    /// or launches it when it has none.
    pub fn show(&mut self, app_id: &str, exec: Option<&str>) -> Result<Vec<Action>, String> {
        let windows: Vec<(Id, bool)> = self.of_app(app_id).map(|w| (w.id, w.minimized)).collect();
        if windows.is_empty() {
            return self.launch(app_id, exec);
        }
        // Windows the user minimized one by one stay minimized, unless all
        // are minimized; then the most recent one comes back.
        let mut targets: Vec<(Id, bool)> = windows
            .iter()
            .copied()
            .filter(|(id, minimized)| !minimized || self.hidden.contains(id))
            .collect();
        if targets.is_empty() {
            targets.push(windows[0]);
        }
        let mut actions = Vec::new();
        // Least recent first, so the most recent ends up on top.
        for &(id, minimized) in targets.iter().rev() {
            if minimized {
                actions.push(Action::Unminimize(id));
            }
            actions.push(Action::Activate(id));
            self.hidden.remove(&id);
        }
        actions.push(Action::Warp);
        Ok(actions)
    }

    /// Minimizes the app's windows, or focuses the previous app where
    /// minimizing is unsupported or not wanted.
    pub fn hide(&mut self, app_id: &str, mode: Hide) -> (Vec<Action>, Option<MinimizeCheck>) {
        let minimize = match mode {
            Hide::Minimize => true,
            Hide::Previous => false,
            Hide::Auto => self.minimize_works != Some(false),
        };
        if !minimize {
            return (self.focus_previous(app_id), None);
        }
        // The focused window goes last, so focus moves to another app
        // instead of a sibling window.
        let mut windows: Vec<(Id, bool)> = self
            .of_app(app_id)
            .filter(|w| !w.minimized)
            .map(|w| (w.id, w.activated))
            .collect();
        windows.sort_by_key(|&(_, active)| active);
        let ids: Vec<Id> = windows.into_iter().map(|(id, _)| id).collect();
        self.hidden.extend(&ids);
        let actions = ids.iter().map(|&id| Action::Minimize(id)).collect();
        let check =
            (mode == Hide::Auto && self.minimize_works.is_none() && !ids.is_empty()).then(|| {
                MinimizeCheck {
                    app_id: app_id.to_string(),
                    windows: ids,
                }
            });
        (actions, check)
    }

    /// Decides whether the compositor honoured an automatic hide. If none of
    /// the windows became minimized, remembers that and focuses the previous app.
    pub fn check_minimized(&mut self, check: &MinimizeCheck) -> Vec<Action> {
        if self.minimize_works.is_some() {
            return Vec::new();
        }
        let tracked: Vec<&Window> = self
            .list
            .iter()
            .filter(|w| check.windows.contains(&w.id))
            .collect();
        if tracked.is_empty() {
            return Vec::new(); // closed meanwhile; decide on the next hide
        }
        if tracked.iter().any(|w| w.minimized) {
            self.minimize_works = Some(true);
            return Vec::new();
        }
        self.minimize_works = Some(false);
        for id in &check.windows {
            self.hidden.remove(id);
        }
        self.focus_previous(&check.app_id)
    }

    /// Focuses the most recently used visible window of another app.
    fn focus_previous(&self, app_id: &str) -> Vec<Action> {
        self.list
            .iter()
            .find(|w| !w.minimized && !w.app_id.is_empty() && !app_id_matches(app_id, &w.app_id))
            .map(|w| vec![Action::Activate(w.id), Action::Warp])
            .unwrap_or_default()
    }

    fn launch(&mut self, app_id: &str, exec: Option<&str>) -> Result<Vec<Action>, String> {
        let exec =
            exec.ok_or_else(|| format!("{app_id} is not running and has no exec command"))?;
        self.launching
            .retain(|(_, started)| started.elapsed() < LAUNCH_GRACE);
        if self.launching.iter().any(|(a, _)| a == app_id) {
            return Ok(Vec::new());
        }
        self.launching.push((app_id.to_string(), Instant::now()));
        Ok(vec![Action::Launch {
            app_id: app_id.to_string(),
            exec: exec.to_string(),
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::Action::*;
    use super::*;

    /// Builds a tracker from (id, app_id, minimized) in most-recent-first
    /// order; the first window is focused unless it is minimized.
    fn tracker(windows: &[(Id, &str, bool)]) -> Tracker {
        let mut t = Tracker::default();
        for &(id, app, minimized) in windows.iter().rev() {
            t.add(id);
            t.set_app_id(id, app.into());
            t.set_state(id, true, false);
            t.set_state(id, false, minimized);
        }
        if let Some(&(id, _, false)) = windows.first() {
            t.set_state(id, true, false);
        }
        t
    }

    fn order(t: &Tracker) -> Vec<Id> {
        t.list.iter().map(|w| w.id).collect()
    }

    #[test]
    fn focus_moves_window_to_front() {
        let mut t = tracker(&[(1, "a", false), (2, "b", false), (3, "c", false)]);
        t.set_state(1, false, false);
        t.set_state(3, true, false);
        assert_eq!(order(&t), [3, 1, 2]);
    }

    #[test]
    fn show_activates_least_recent_first() {
        let mut t = tracker(&[(1, "term", false), (2, "web", false), (3, "web", false)]);
        let actions = t.show("web", None).unwrap();
        assert_eq!(actions, [Activate(3), Activate(2), Warp]);
    }

    #[test]
    fn show_leaves_user_minimized_windows_alone() {
        let mut t = tracker(&[(1, "term", false), (2, "web", true), (3, "web", false)]);
        assert_eq!(t.show("web", None).unwrap(), [Activate(3), Warp]);
    }

    #[test]
    fn show_restores_most_recent_when_all_minimized() {
        let mut t = tracker(&[(1, "term", false), (2, "web", true), (3, "web", true)]);
        assert_eq!(
            t.show("web", None).unwrap(),
            [Unminimize(2), Activate(2), Warp]
        );
    }

    #[test]
    fn show_launches_missing_app_once() {
        let mut t = tracker(&[(1, "term", false)]);
        assert_eq!(
            t.show("web", Some("firefox")).unwrap(),
            [Launch {
                app_id: "web".into(),
                exec: "firefox".into()
            }]
        );
        assert_eq!(t.show("web", Some("firefox")).unwrap(), []);
        assert!(t.show("chat", None).is_err());
    }

    #[test]
    fn launch_grace_ends_when_window_appears() {
        let mut t = tracker(&[(1, "term", false)]);
        t.show("web", Some("firefox")).unwrap();
        t.add(2);
        t.set_app_id(2, "web".into());
        t.remove(2);
        assert_eq!(
            t.show("web", Some("firefox")).unwrap(),
            [Launch {
                app_id: "web".into(),
                exec: "firefox".into()
            }]
        );
    }

    #[test]
    fn toggle_hides_focused_app_with_focused_window_last() {
        let mut t = tracker(&[(1, "web", false), (2, "web", false), (3, "term", false)]);
        let (actions, check) = t.toggle("web", None, Hide::Minimize).unwrap();
        assert_eq!(actions, [Minimize(2), Minimize(1)]);
        assert!(check.is_none());
    }

    #[test]
    fn hidden_windows_come_back_together() {
        let mut t = tracker(&[(1, "web", false), (2, "web", false), (3, "term", false)]);
        t.hide("web", Hide::Minimize);
        t.set_state(2, false, true);
        t.set_state(1, false, true);
        t.set_state(3, true, false);
        let actions = t.show("web", None).unwrap();
        assert_eq!(
            actions,
            [Unminimize(2), Activate(2), Unminimize(1), Activate(1), Warp]
        );
    }

    #[test]
    fn hide_previous_focuses_other_app() {
        let mut t = tracker(&[(1, "web", false), (2, "web", false), (3, "term", false)]);
        let (actions, _) = t.hide("web", Hide::Previous);
        assert_eq!(actions, [Activate(3), Warp]);
    }

    #[test]
    fn auto_hide_learns_minimize_works() {
        let mut t = tracker(&[(1, "web", false), (2, "term", false)]);
        let (_, check) = t.hide("web", Hide::Auto);
        t.set_state(1, false, true);
        assert_eq!(t.check_minimized(&check.unwrap()), []);
        assert_eq!(t.minimize_works, Some(true));
        assert!(t.hide("web", Hide::Auto).1.is_none());
    }

    #[test]
    fn auto_hide_falls_back_when_minimize_is_ignored() {
        let mut t = tracker(&[(1, "web", false), (2, "term", false)]);
        let (actions, check) = t.hide("web", Hide::Auto);
        assert_eq!(actions, [Minimize(1)]);
        assert_eq!(t.check_minimized(&check.unwrap()), [Activate(2), Warp]);
        assert_eq!(t.minimize_works, Some(false));
        assert_eq!(t.hide("web", Hide::Auto).0, [Activate(2), Warp]);
    }

    #[test]
    fn closed_windows_are_forgotten() {
        let mut t = tracker(&[(1, "web", false), (2, "term", false)]);
        t.remove(1);
        assert_eq!(order(&t), [2]);
        assert!(t.active().is_none());
    }
}

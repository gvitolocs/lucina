//! The shell's table of app surfaces: the manifest id it launched, the [`AppId`] the window manager's rules run on, and the
//! [`SurfaceId`] the compositor holds.
//!
//! Section 5a of `docs/design/juno-window-manager.md` gives every running app one surface, owned by the shell: the app paints
//! into a buffer it handed over, the compositor samples it, and the row of spaces decides where it goes. Three names for that
//! one thing are in play — `apps.rs` and `app_groups.rs` know the manifest id string, `lucina-wm-rules` knows only `AppId`,
//! `lucina-app::compose` knows only `SurfaceId`. This module keeps the three together so a caller never has to remember a
//! number across a call: `wm_scene::scene_ops` takes a `space -> surface` map, and slice W builds it from
//! [`Spaces::space_of`](lucina_wm_rules::spaces::Spaces::space_of) over this table.
//!
//! Bounded like the rules it serves, in both directions. [`MAX_APPS`] slots in a fixed array that never grows, so a table cannot
//! be pushed past what a row of spaces can show; and the [`AppId`] values come from a counter that turns over the whole
//! 1..=u16::MAX pool and skips the values that are live, so two apps that run at once never share an id and a closed app's id
//! does not come back to name a new one until the pool has turned over (65 535 opens). The slot behind an id is reused at once,
//! so the table holds `MAX_APPS` apps however many come and go.
//!
//! Why a counter and not a generation packed into the id: [`AppId`] is a bare u16 that the rules compare only for equality, so
//! one could split it into a slot and a per-slot generation (4 bits and 12, say). That window is four times narrower than the
//! counter's, and the counter's uniqueness among the live apps is a checked fact rather than a bit layout, so this is the one
//! that is stated in the tests below. What a caller gets from either is the same rule: an `AppId` you no longer hold in the
//! table answers for nothing, never for whoever took the slot.
//!
//! Nothing here waits, panics or uses `unsafe`: the failures are the compositor's own ([`std::io::Error`] from `add_surface`) and
//! the table's full/no-such-app cases, and a call that fails leaves the table exactly as it was.

#![allow(dead_code)] // slice W wires this in; it is the surface owner that the Scene -> ops mapping of wm_scene waits for

use std::array;
use std::io;

use lucina_app::compose::{Compositor, SurfaceId};
use lucina_wm_rules::spaces::MAX_SPACES;
use lucina_wm_rules::switcher::AppId;

/// How many apps the table holds at most: one space per app in a [`Spaces`](lucina_wm_rules::spaces::Spaces) row of
/// [`MAX_SPACES`], Home taking the first of them. [`switcher::MAX_APPS`](lucina_wm_rules::switcher::MAX_APPS) is one larger
/// because the most-recently-used order counts apps and not the spaces they sit in; staying inside the row's own capacity is what
/// lets the shell drive the two side by side and never open an app the row has no room to show.
pub const MAX_APPS: usize = MAX_SPACES - 1;

/// One live app: the three names for it.
#[derive(Debug)]
struct Entry {
    /// The shell's manifest id, the one `apps.rs` launched.
    id: String,
    /// The app as the window manager's rules know it.
    app: AppId,
    /// Its surface in the compositor it was opened on.
    surface: SurfaceId,
}

/// The live apps, their [`AppId`] and their surface. See the module's text.
#[derive(Debug)]
pub struct SurfaceTable {
    slots: [Option<Entry>; MAX_APPS],
    /// How many are live, kept so [`SurfaceTable::len`] does not count.
    len: usize,
    /// The next [`AppId`] value to offer. It only moves forward, wrapping to 1 — `AppId(0)` stays the filler the rules' arrays
    /// use, never an app.
    next_app: u16,
}

impl Default for SurfaceTable {
    fn default() -> SurfaceTable {
        SurfaceTable::new()
    }
}

impl SurfaceTable {
    /// The table of a shell with no app running.
    pub fn new() -> SurfaceTable {
        SurfaceTable {
            slots: array::from_fn(|_| None),
            len: 0,
            next_app: 1,
        }
    }

    /// How many apps are live. Never more than [`MAX_APPS`].
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether no app runs.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The [`AppId`] of the live app `id`, None when that app does not run.
    pub fn app_of(&self, id: &str) -> Option<AppId> {
        self.entry_of_id(id).map(|entry| entry.app)
    }

    /// The manifest id of the live app `app`, None when that id is not one of the live apps. An id of a closed app is not here,
    /// and never answers for the app that took its slot.
    pub fn id_of(&self, app: AppId) -> Option<&str> {
        self.entry_of_app(app).map(|entry| entry.id.as_str())
    }

    /// The surface of the live app `app`, None when that id is not one of the live apps. This is the map `scene_ops` calls.
    pub fn surface_of(&self, app: AppId) -> Option<SurfaceId> {
        self.entry_of_app(app).map(|entry| entry.surface)
    }

    /// The live apps, in slot order: manifest id, [`AppId`], surface. One per live app, so [`MAX_APPS`] at most.
    pub fn iter(&self) -> impl Iterator<Item = (&str, AppId, SurfaceId)> + '_ {
        self.slots
            .iter()
            .flatten()
            .map(|entry| (entry.id.as_str(), entry.app, entry.surface))
    }

    /// The app `id` starts: it gets an [`AppId`] and, on a fresh open, a surface of `size` from `comp`.
    ///
    /// An app that is live already is left exactly as it is — the same [`AppId`] and the same surface, and no second surface is
    /// asked for — because the shell may call this again for an app it already shows (a relaunch that found it running, a
    /// `configure` that comes twice). A table that is full answers with an error and takes nothing; a compositor that fails to
    /// make the surface leaves the table as it was, with the [`AppId`] it had offered simply not used.
    pub fn open(
        &mut self,
        id: &str,
        comp: &mut dyn Compositor,
        size: (u32, u32),
    ) -> io::Result<AppId> {
        if let Some(entry) = self.entry_of_id(id) {
            return Ok(entry.app);
        }
        let Some(slot) = self.slots.iter().position(Option::is_none) else {
            return Err(io::Error::other(format!(
                "surface_table: no room for {id}, {MAX_APPS} apps are open"
            )));
        };
        let app = self.take_app_id();
        let surface = comp.add_surface(size.0, size.1)?;
        self.slots[slot] = Some(Entry {
            id: id.to_string(),
            app,
            surface,
        });
        self.len += 1;
        Ok(app)
    }

    /// The app `id` ended: its surface goes back to `comp` and its slot is free for the next app. None when the app does not
    /// run, which is what a second `bye` for a process already reaped looks like; then nothing is removed.
    pub fn close(&mut self, id: &str, comp: &mut dyn Compositor) -> Option<AppId> {
        let slot = self.index_of_id(id)?;
        let entry = self.slots[slot].take()?;
        self.len -= 1;
        comp.remove_surface(entry.surface);
        Some(entry.app)
    }

    /// The app `id` gets a surface of `size` (a tile, or a screen that changed), and the old one goes back to `comp`. Its
    /// [`AppId`] and its manifest id do not change, so the row of spaces and the switcher order need nothing.
    ///
    /// Ok(false) when the app does not run, and nothing added. The new surface is asked for before the old one is dropped, so a
    /// failure leaves the app on the surface it had rather than on none.
    pub fn resize(
        &mut self,
        id: &str,
        comp: &mut dyn Compositor,
        size: (u32, u32),
    ) -> io::Result<bool> {
        let Some(entry) = self.entry_mut_of_id(id) else {
            return Ok(false);
        };
        let surface = comp.add_surface(size.0, size.1)?;
        comp.remove_surface(entry.surface);
        entry.surface = surface;
        Ok(true)
    }

    fn entry_of_id(&self, id: &str) -> Option<&Entry> {
        self.slots.iter().flatten().find(|entry| entry.id == id)
    }

    fn entry_mut_of_id(&mut self, id: &str) -> Option<&mut Entry> {
        self.slots.iter_mut().flatten().find(|entry| entry.id == id)
    }

    fn entry_of_app(&self, app: AppId) -> Option<&Entry> {
        self.slots.iter().flatten().find(|entry| entry.app == app)
    }

    fn index_of_id(&self, id: &str) -> Option<usize> {
        self.slots
            .iter()
            .position(|slot| slot.as_ref().is_some_and(|entry| entry.id == id))
    }

    /// The next [`AppId`] no live app holds. At most [`MAX_APPS`] of the 65 535 values are live, so a free one turns up within
    /// [`MAX_APPS`] steps plus one.
    fn take_app_id(&mut self) -> AppId {
        loop {
            let value = self.next_app;
            self.next_app = if value == u16::MAX { 1 } else { value + 1 };
            if self.entry_of_app(AppId(value)).is_none() {
                return AppId(value);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_app::compose::{CpuCompositor, Op, Shown};
    use lucina_app::gfx::{Frame, Rect};
    use lucina_wm_rules::spaces::Spaces;

    /// A [`CpuCompositor`] that counts the surfaces it was asked to make and, from `fail_at` on, refuses one of them. The count
    /// is how a test sees that an idempotent open added nothing; `fail_at` (1-based, 0 for never) is how it sees a compositor that
    /// fails.
    struct Counting {
        inner: CpuCompositor,
        added: usize,
        fail_at: usize,
    }

    impl Counting {
        fn new() -> Counting {
            Counting {
                inner: CpuCompositor::default(),
                added: 0,
                fail_at: 0,
            }
        }
    }

    impl Compositor for Counting {
        fn name(&self) -> &'static str {
            "counting"
        }
        fn add_surface(&mut self, w: u32, h: u32) -> io::Result<SurfaceId> {
            self.added += 1;
            if self.fail_at != 0 && self.added == self.fail_at {
                return Err(io::Error::other("no buffer"));
            }
            self.inner.add_surface(w, h)
        }
        fn remove_surface(&mut self, id: SurfaceId) {
            self.inner.remove_surface(id);
        }
        fn surface_size(&self, id: SurfaceId) -> Option<(u32, u32)> {
            self.inner.surface_size(id)
        }
        fn surface_mut(&mut self, id: SurfaceId) -> &mut [u32] {
            self.inner.surface_mut(id)
        }
        fn changed(&mut self, id: SurfaceId, r: Rect) {
            self.inner.changed(id, r);
        }
        fn show(&mut self, ops: &[Op], vblank: bool, frame: &mut Frame) -> io::Result<Shown> {
            self.inner.show(ops, vblank, frame)
        }
    }

    const SIZE: (u32, u32) = (8, 6);

    /// The apps of the tests, in the order they open.
    const APPS: [&str; 4] = ["files", "gallery", "camera", "office"];

    /// A table with `APPS` open on `comp`, all on [`SIZE`].
    fn table_with(comp: &mut Counting) -> SurfaceTable {
        let mut t = SurfaceTable::new();
        for id in APPS {
            t.open(id, comp, SIZE).expect("room for four");
        }
        t
    }

    #[test]
    fn open_makes_one_surface_and_its_appid_stays() {
        let mut c = Counting::new();
        let mut t = SurfaceTable::new();
        let app = t.open("gallery", &mut c, SIZE).expect("a first app");
        assert_eq!(t.len(), 1);
        assert!(!t.is_empty());
        assert_eq!(c.added, 1, "one surface asked for");
        assert_eq!(t.app_of("gallery"), Some(app));
        assert_eq!(t.id_of(app), Some("gallery"));
        let surface = t.surface_of(app).expect("the app has a surface");
        assert_eq!(c.surface_size(surface), Some(SIZE));
    }

    #[test]
    fn an_open_of_a_live_app_is_idempotent() {
        let mut c = Counting::new();
        let mut t = table_with(&mut c);
        let app = t.app_of("gallery").expect("live");
        let surface = t.surface_of(app).expect("its surface");
        let again = t.open("gallery", &mut c, (4, 4)).expect("a live app");
        assert_eq!(again, app, "the same AppId");
        assert_eq!(t.surface_of(app), Some(surface), "the same surface");
        assert_eq!(c.surface_size(surface), Some(SIZE), "and its size");
        assert_eq!(c.added, 4, "no surface was asked for");
        assert_eq!(t.len(), 4);
    }

    #[test]
    fn close_removes_the_surface_and_frees_the_slot() {
        let mut c = Counting::new();
        let mut t = table_with(&mut c);
        let app = t.app_of("gallery").expect("live");
        let surface = t.surface_of(app).expect("its surface");
        assert_eq!(t.close("gallery", &mut c), Some(app));
        assert_eq!(c.surface_size(surface), None, "the compositor dropped it");
        assert_eq!(t.len(), 3);
        assert_eq!(t.app_of("gallery"), None);
        assert_eq!(t.id_of(app), None);
        assert_eq!(t.surface_of(app), None);
        // The other apps are untouched.
        for id in ["files", "camera", "office"] {
            let other = t.app_of(id).expect("still live");
            assert!(
                c.surface_size(t.surface_of(other).expect("its surface"))
                    .is_some()
            );
        }
    }

    #[test]
    fn the_open_past_the_cap_fails_and_leaves_the_table_as_it_was() {
        let mut c = Counting::new();
        let mut t = SurfaceTable::new();
        let mut opened = Vec::new();
        for i in 0..MAX_APPS {
            let id = format!("app{i}");
            opened.push(t.open(&id, &mut c, SIZE).expect("room up to the cap"));
        }
        assert_eq!(t.len(), MAX_APPS);
        assert_eq!(c.added, MAX_APPS);
        let err = t
            .open("one-too-many", &mut c, SIZE)
            .expect_err("a full table says so");
        assert_eq!(err.kind(), io::ErrorKind::Other);
        assert_eq!(t.len(), MAX_APPS, "nothing was taken");
        assert_eq!(c.added, MAX_APPS, "not even a surface was asked for");
        assert_eq!(t.app_of("one-too-many"), None);
        for (i, app) in opened.iter().enumerate() {
            assert_eq!(t.app_of(&format!("app{i}")), Some(*app));
            assert_eq!(
                c.surface_size(t.surface_of(*app).expect("its surface")),
                Some(SIZE)
            );
        }
        // Reopening a live app still answers at the cap: it needs no room.
        assert_eq!(t.open("app3", &mut c, SIZE).expect("a live app"), opened[3]);
        assert_eq!(c.added, MAX_APPS, "and still no surface");
    }

    #[test]
    fn a_full_table_opens_anew_once_an_app_closes() {
        let mut c = Counting::new();
        let mut t = SurfaceTable::new();
        for i in 0..MAX_APPS {
            t.open(&format!("app{i}"), &mut c, SIZE)
                .expect("room up to the cap");
        }
        t.close("app0", &mut c).expect("live");
        assert_eq!(t.len(), MAX_APPS - 1, "the slot is free");
        let last = t.open("latecomer", &mut c, SIZE).expect("the freed slot");
        assert_eq!(t.len(), MAX_APPS);
        assert_eq!(t.id_of(last), Some("latecomer"));
        assert!(
            c.surface_size(t.surface_of(last).expect("its surface"))
                .is_some()
        );
        assert!(
            t.open(&format!("app{}", MAX_APPS), &mut c, SIZE).is_err(),
            "the cap holds however many come and go"
        );
    }

    #[test]
    fn resize_keeps_the_appid_and_changes_the_size() {
        let mut c = Counting::new();
        let mut t = table_with(&mut c);
        let app = t.app_of("gallery").expect("live");
        let old = t.surface_of(app).expect("its surface");
        assert!(t.resize("gallery", &mut c, (4, 4)).expect("a live app"));
        assert_eq!(t.app_of("gallery"), Some(app), "the AppId is kept");
        assert_eq!(t.id_of(app), Some("gallery"));
        let fresh = t.surface_of(app).expect("a surface after the resize");
        assert_ne!(fresh, old, "the old one went back");
        assert_eq!(c.surface_size(old), None);
        assert_eq!(c.surface_size(fresh), Some((4, 4)));
        assert_eq!(c.surface_mut(fresh).len(), 4 * 4, "and it is paintable");
        assert_eq!(c.added, 5, "one new surface");
        assert_eq!(t.len(), 4);
    }

    #[test]
    fn a_resize_of_an_unknown_app_adds_nothing() {
        let mut c = Counting::new();
        let mut t = table_with(&mut c);
        assert!(!t.resize("nobody", &mut c, (4, 4)).expect("not an error"));
        assert_eq!(c.added, 4);
        assert_eq!(t.len(), 4);
    }

    #[test]
    fn lookups_of_an_unknown_answer_none() {
        let mut c = Counting::new();
        let mut t = table_with(&mut c);
        assert_eq!(t.app_of("nobody"), None);
        assert_eq!(t.id_of(AppId(999)), None);
        assert_eq!(t.surface_of(AppId(999)), None);
        assert_eq!(t.close("nobody", &mut c), None);
        assert_eq!(t.surface_of(AppId(0)), None, "the filler is never an app");
    }

    #[test]
    fn a_close_of_an_unknown_app_removes_no_surface() {
        let mut c = Counting::new();
        let mut t = table_with(&mut c);
        let app = t.app_of("files").expect("live");
        let surface = t.surface_of(app).expect("its surface");
        assert_eq!(t.close("nobody", &mut c), None);
        assert_eq!(c.surface_size(surface), Some(SIZE), "still there");
    }

    #[test]
    fn a_reopened_app_gets_a_working_surface_and_a_fresh_appid() {
        let mut c = Counting::new();
        let mut t = table_with(&mut c);
        let gone = t.app_of("gallery").expect("live");
        t.close("gallery", &mut c).expect("it was live");
        let fresh = t.open("gallery", &mut c, (4, 4)).expect("room again");
        assert_eq!(t.len(), 4);
        assert_ne!(fresh, gone, "a stale AppId cannot name the new app");
        assert_eq!(t.app_of("gallery"), Some(fresh));
        assert_eq!(t.id_of(gone), None);
        // A closed app maps to no surface at all, so nothing reaches an op for it. The SurfaceId itself may be one the
        // compositor handed out again (CpuCompositor fills its first empty slot), which the table never relies on either way.
        assert_eq!(t.surface_of(gone), None);
        let surface = t.surface_of(fresh).expect("its surface");
        assert_eq!(c.surface_size(surface), Some((4, 4)), "a working surface");
    }

    #[test]
    fn no_two_live_apps_share_an_appid_and_ids_do_not_come_back() {
        let mut c = Counting::new();
        let mut t = SurfaceTable::new();
        let mut seen = Vec::new();
        for round in 0..200 {
            let app = t
                .open("the-app", &mut c, SIZE)
                .expect("a table of one always has room");
            assert!(
                !seen.contains(&app),
                "round {round}: id {app:?} was handed out before"
            );
            assert_eq!(t.id_of(app), Some("the-app"));
            seen.push(app);
            assert_eq!(t.len(), 1);
            t.close("the-app", &mut c).expect("live");
            assert_eq!(t.len(), 0);
        }
        assert_eq!(t.len(), 0, "the table did not grow with the rounds");
    }

    #[test]
    fn iter_gives_each_live_app_once() {
        let mut c = Counting::new();
        let t = table_with(&mut c);
        let listed: Vec<(&str, AppId, SurfaceId)> = t.iter().collect();
        assert_eq!(listed.len(), t.len());
        for (id, app, surface) in &listed {
            assert_eq!(t.app_of(id), Some(*app), "{id} lists what it holds");
            assert_eq!(*surface, t.surface_of(*app).expect("its surface"));
            assert_eq!(id, &t.id_of(*app).expect("the id of the app"));
        }
        let mut ids: Vec<&str> = listed.iter().map(|(id, _, _)| *id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 4, "no app twice");
        for id in APPS {
            assert!(ids.contains(&id), "{id} is listed");
        }
    }

    #[test]
    fn a_failed_surface_leaves_the_table_as_it_was() {
        let mut c = Counting::new();
        let mut t = table_with(&mut c);
        c.fail_at = 5;
        assert_eq!(t.len(), 4);
        let err = t
            .open("gallery-two", &mut c, SIZE)
            .expect_err("the compositor says no");
        assert_eq!(err.kind(), io::ErrorKind::Other);
        assert_eq!(t.len(), 4, "the app was not entered");
        assert_eq!(t.app_of("gallery-two"), None);
        for id in APPS {
            let app = t.app_of(id).expect("still live");
            assert!(
                c.surface_size(t.surface_of(app).expect("its surface"))
                    .is_some()
            );
        }
        // The next app opens on the same table, with an id the failed attempt did not burn in a way that clashes.
        c.fail_at = 0;
        let fresh = t.open("gallery-two", &mut c, SIZE).expect("room");
        assert_eq!(t.len(), 5);
        assert_eq!(t.id_of(fresh), Some("gallery-two"));
    }

    #[test]
    fn a_failed_resize_keeps_the_surface_the_app_had() {
        let mut c = Counting::new();
        c.fail_at = 5;
        let mut t = table_with(&mut c);
        let app = t.app_of("gallery").expect("live");
        let surface = t.surface_of(app).expect("its surface");
        assert!(
            t.resize("gallery", &mut c, (4, 4))
                .expect_err("the compositor says no")
                .kind()
                == io::ErrorKind::Other
        );
        assert_eq!(t.surface_of(app), Some(surface), "the old surface stays");
        assert_eq!(c.surface_size(surface), Some(SIZE));
        assert_eq!(t.len(), 4);
    }

    #[test]
    fn the_table_and_a_spaces_row_agree_on_which_apps_are_live() {
        let mut c = Counting::new();
        let mut t = SurfaceTable::new();
        let mut spaces = Spaces::new();
        let mut row: Vec<(&str, AppId)> = Vec::new();
        for (i, id) in APPS.iter().enumerate() {
            let app = t.open(id, &mut c, SIZE).expect("room");
            assert!(
                spaces.open_app(app).is_some(),
                "the row has a space for {id}"
            );
            row.push((id, app));
            assert_eq!(spaces.len(), i + 2, "Home plus the {} apps opened", i + 1);
        }
        for (id, app) in row.iter() {
            assert_eq!(
                spaces.space_of(*app).is_some(),
                t.app_of(id).is_some(),
                "{id} is live in both or in neither"
            );
        }
        // The app in the middle leaves both.
        t.close("gallery", &mut c).expect("live");
        spaces.close_app(row[1].1);
        assert_eq!(t.app_of("gallery"), None);
        assert_eq!(spaces.space_of(row[1].1), None);
        for (id, app) in row.iter() {
            assert_eq!(
                spaces.space_of(*app).is_some(),
                t.app_of(id).is_some(),
                "{id} is live in both or in neither after the close"
            );
            if *id == "gallery" {
                continue;
            }
            assert!(
                (0..spaces.len()).any(|i| spaces.get(i).is_some_and(|s| s.contains(*app))),
                "the row still shows {id}"
            );
        }
        // The cap is the same on both sides: the row keeps a space for every app the table holds, and both stop there.
        let mut t = SurfaceTable::new();
        let mut spaces = Spaces::new();
        for i in 0..MAX_APPS {
            let id = format!("cap{i}");
            let app = t.open(&id, &mut c, SIZE).expect("room up to the cap");
            assert_eq!(
                spaces.open_app(app),
                Some(i + 1),
                "the row has a space for {id}"
            );
        }
        assert_eq!(t.len(), MAX_APPS);
        assert!(
            t.open("one-too-many", &mut c, SIZE).is_err(),
            "the table stops at the cap"
        );
        assert_eq!(
            spaces.open_app(AppId(500)),
            None,
            "and the row stops at the same count"
        );
    }

    #[test]
    fn a_fresh_table_is_empty_and_holds_nothing() {
        let t = SurfaceTable::default();
        assert!(t.is_empty());
        assert_eq!(t.len(), 0);
        assert_eq!(t.iter().count(), 0);
        assert_eq!(t.app_of("files"), None);
    }
}

//! Drawing motion: the clock every window shares, what asks gpui for frames
//! (only a box that moves does), the pointer probe of boxes that react to
//! hover, and the figures of actors.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use gpui_kit::*;

use super::actor::{ActorDef, LookKind, Pt, Run};
use super::motion::{NodeMotion, Registry};
use super::picture;

/// The motion of every window: its state, its clock, and the settings that
/// say how much to move.
#[derive(Clone)]
pub struct Motion {
    reg: Arc<Mutex<Registry>>,
    epoch: Instant,
    /// `ui.reduce_motion`, or the system's.
    pub reduce: bool,
    /// `ui.decorations`.
    pub decorations: bool,
}

impl Global for Motion {}

/// Puts the motion settings in effect. The state of boxes and actors stays.
pub fn set_motion(reduce: bool, decorations: bool, cx: &mut App) {
    let (reg, epoch) = match cx.try_global::<Motion>() {
        Some(m) => (m.reg.clone(), m.epoch),
        None => (Arc::default(), Instant::now()),
    };
    cx.set_global(Motion { reg, epoch, reduce, decorations });
}

impl Motion {
    /// Seconds since the app started.
    pub fn now(&self) -> f64 {
        self.epoch.elapsed().as_secs_f64()
    }

    pub fn reg(&self) -> MutexGuard<'_, Registry> {
        self.reg.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Notes that the templates were compiled again.
    pub fn sync(&self, generation: u64) {
        let mut reg = self.reg();
        let before = reg.generation();
        reg.sync(generation);
        if before != generation {
            picture::clear();
        }
    }
}

/// The key of a box: its window, drawing, place in the template and in its list.
pub fn key(win: u64, scope: &str, path: &str, index: Option<usize>) -> String {
    format!("{win}|{scope}|{path}|{}", index.map_or(String::new(), |i| i.to_string()))
}

/// What a box with `key` and everything below it forgets when it is hidden: it
/// starts again when it shows.
pub fn forget(motion: &Motion, win: u64, scope: &str, path: &str) {
    let mut reg = motion.reg();
    reg.forget(&format!("{win}|{scope}|{path}|"));
    reg.forget(&format!("{win}|{scope}|{path}."));
}

/// An element that asks for the next frame each time it is painted: put in a
/// box only while the box moves.
pub fn frame_request() -> AnyElement {
    canvas(|_, _, _| {}, |_, _, window, _| window.request_animation_frame()).absolute().inset_0().into_any_element()
}

/// An element that tells `key` when the pointer enters and leaves the box it
/// is in. It takes no click or hover from anything.
pub fn hover_probe(key: String, m: Arc<NodeMotion>, motion: Motion) -> AnyElement {
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |_, hitbox, window, _| {
            let (key, m, motion) = (key.clone(), m.clone(), motion.clone());
            window.on_mouse_event(move |_: &MouseMoveEvent, _, window, _| {
                let over = hitbox.is_hovered(window);
                let changed = motion.reg().set_hover(&key, over, &m, motion.now());
                if changed {
                    window.refresh();
                }
            });
        },
    )
    .absolute()
    .inset_0()
    .into_any_element()
}

fn seed(key: &str, now: f64) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut h);
    now.to_bits().hash(&mut h);
    h.finish()
}

/// An actor on the stage that is the box it is in: drawn at where it is now,
/// with a click that makes it react.
pub fn actor(motion: &Motion, def: &Arc<ActorDef>, key: String) -> AnyElement {
    let now = motion.now();
    let (pose, wake, moving) = {
        let mut reg = motion.reg();
        let Some(stage) = reg.stages.get(&key).copied() else {
            drop(reg);
            return stage_probe(motion, key, None, true);
        };
        if reg.actors.get(&key).is_none_or(|r| *r.def != **def) {
            reg.actors.insert(key.clone(), Run::new(def.clone(), stage, now, seed(&key, now)));
        }
        let Some(run) = reg.actors.get_mut(&key) else { return div().into_any_element() };
        run.resize(stage);
        let pose = if motion.reduce { run.still() } else { run.advance(now) };
        let (moving, wake) = (pose.moving, pose.wake);
        (pose, wake, moving)
    };
    let (w, h) = def.size;
    let look = &def.looks[pose.look.min(def.looks.len() - 1)];
    let mut interval = None;
    let content: AnyElement = match &look.kind {
        LookKind::Image(path) if picture::is_svg(path) => img(path.clone()).size_full().object_fit(ObjectFit::Contain).into_any_element(),
        kind => {
            let (path, sprite, fps, looping) = match kind {
                LookKind::Image(p) => (p, None, 12., true),
                LookKind::Sprite { path, frames, fps, looping } => (path, Some(*frames), *fps, *looping),
            };
            match picture::load(path, sprite) {
                Some(pic) => {
                    let secs = if looping { now as f32 } else { pose.age };
                    let frame = pic.frame_at(secs, fps, looping);
                    interval = pic.interval(fps).filter(|_| !motion.reduce);
                    let mirrored = pose.mirrored;
                    canvas(|_, _, _| {}, move |bounds, _, window, _| pic.paint(window, bounds, frame, mirrored))
                        .size_full()
                        .into_any_element()
                },
                None => div().into_any_element(),
            }
        },
    };
    let click_key = key.clone();
    let click_motion = motion.clone();
    let figure = div()
        .absolute()
        .left(px(pose.at.x - w / 2.))
        .top(px(pose.at.y - h))
        .w(px(w))
        .h(px(h))
        .id(ElementId::Name(format!("actor:{key}").into()))
        .cursor_pointer()
        .on_click(move |e, window, _| {
            let mut reg = click_motion.reg();
            let dx = reg.centers.get(&click_key).map_or(0., |c| f32::from(e.position().x) - c);
            if let Some(run) = reg.actors.get_mut(&click_key) {
                run.click(dx);
            }
            drop(reg);
            window.refresh();
        })
        .child(content)
        .child(center_probe(motion.clone(), key.clone()));
    // Frames while it moves; while it stands, a wake-up when something changes.
    let wake = match (moving, wake, interval) {
        (true, _, _) => None,
        (false, w, i) => w.into_iter().chain(i).reduce(f32::min),
    };
    let probe = stage_probe(motion, key, wake, moving);
    div().absolute().inset_0().child(probe).child(figure).into_any_element()
}

/// The stage: it fills its parent, takes no mouse event, measures itself for the
/// actor and wakes it. `moving` asks for a frame after each one.
fn stage_probe(motion: &Motion, key: String, wake: Option<f32>, moving: bool) -> AnyElement {
    let motion = motion.clone();
    canvas(
        |bounds, _, _| bounds,
        move |bounds, _, window, cx| {
            let stage = Pt { x: f32::from(bounds.size.width), y: f32::from(bounds.size.height) };
            let changed = {
                let mut reg = motion.reg();
                let changed = reg.stages.get(&key) != Some(&stage);
                reg.stages.insert(key.clone(), stage);
                changed
            };
            if changed || moving {
                window.request_animation_frame();
            }
            let Some(secs) = wake else { return };
            if motion.reg().pending.insert(key.clone()) {
                let (motion, key) = (motion.clone(), key.clone());
                let wait = Duration::from_secs_f32(secs.max(0.016));
                window
                    .spawn(cx, async move |cx| {
                        cx.background_executor().timer(wait).await;
                        motion.reg().pending.remove(&key);
                        cx.update(|window, _| window.refresh()).ok();
                    })
                    .detach();
            }
        },
    )
    .absolute()
    .inset_0()
    .into_any_element()
}

/// Notes where the figure was painted, so a click knows which side of it it is on.
fn center_probe(motion: Motion, key: String) -> AnyElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, _, _| {
            motion.reg().centers.insert(key.clone(), f32::from(bounds.center().x));
        },
    )
    .absolute()
    .inset_0()
    .into_any_element()
}

/// The motion in effect, ready for the templates now compiled.
pub fn current(cx: &App) -> Option<Motion> {
    let m = cx.try_global::<Motion>()?.clone();
    if let Some(t) = cx.try_global::<super::Templates>() {
        m.sync(t.generation());
    }
    Some(m)
}

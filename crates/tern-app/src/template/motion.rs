//! Motion in templates, as in CSS: keyframe `animations` run by a box's
//! `animation`, `transition`s that ease a property to its new value, and `on`
//! events that run a keyframe set once.
//!
//! Everything here is plain math over seconds, so it is tested without a
//! window. The drawing code asks [`Registry::frame`] what a box looks like at
//! a time and whether it still moves; only a box that moves asks gpui for
//! another frame.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use gpui_kit::*;

use super::Cond;
use crate::style::Val;

/// What an animation changes: a style property, or a transform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Chan {
    Prop(&'static str),
    /// Pixels.
    Tx,
    Ty,
    Scale,
    /// Degrees.
    Rotate,
}

/// The transform channels a keyframe may name, and what they are by default.
pub const TRANSFORMS: [(&str, Chan, f32); 4] =
    [("translate_x", Chan::Tx, 0.), ("translate_y", Chan::Ty, 0.), ("scale", Chan::Scale, 1.), ("rotate", Chan::Rotate, 0.)];

/// What a box is moved by: its transform channels at a moment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    pub tx: f32,
    pub ty: f32,
    pub scale: f32,
    pub rotate: f32,
}

impl Default for Transform {
    fn default() -> Self {
        Transform { tx: 0., ty: 0., scale: 1., rotate: 0. }
    }
}

impl Transform {
    #[cfg(test)]
    pub fn is_identity(&self) -> bool {
        *self == Transform::default()
    }

    /// The transform the channels in `chans` say.
    pub fn of(chans: &HashMap<Chan, Val>) -> Transform {
        let get = |c: Chan, d: f32| match chans.get(&c) {
            Some(Val::Num(n)) => *n,
            _ => d,
        };
        Transform { tx: get(Chan::Tx, 0.), ty: get(Chan::Ty, 0.), scale: get(Chan::Scale, 1.), rotate: get(Chan::Rotate, 0.) }
    }
}

// ---- easing ----

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Easing {
    Linear,
    Bezier(f32, f32, f32, f32),
    /// `steps(n)`: n jumps, at the end of each step.
    Steps(u32),
}

impl Easing {
    pub const EASE: Easing = Easing::Bezier(0.25, 0.1, 0.25, 1.);

    pub fn parse(text: &str) -> Result<Easing, String> {
        let t = text.trim();
        Ok(match t {
            "linear" => Easing::Linear,
            "ease" => Easing::EASE,
            "ease-in" => Easing::Bezier(0.42, 0., 1., 1.),
            "ease-out" => Easing::Bezier(0., 0., 0.58, 1.),
            "ease-in-out" => Easing::Bezier(0.42, 0., 0.58, 1.),
            _ => {
                let args = |name: &str| t.strip_prefix(name).and_then(|r| r.trim().strip_prefix('(')).and_then(|r| r.strip_suffix(')'));
                if let Some(a) = args("steps") {
                    let n: u32 =
                        a.split(',').next().unwrap_or("").trim().parse().map_err(|_| format!("\"{t}\": steps needs a whole number"))?;
                    if n == 0 {
                        return Err(format!("\"{t}\": steps needs at least 1"));
                    }
                    Easing::Steps(n)
                } else if let Some(a) = args("cubic-bezier") {
                    let v: Vec<f32> = a.split(',').filter_map(|p| p.trim().parse().ok()).collect();
                    match v[..] {
                        [x1, y1, x2, y2] if (0. ..=1.).contains(&x1) && (0. ..=1.).contains(&x2) => Easing::Bezier(x1, y1, x2, y2),
                        _ => return Err(format!("\"{t}\": cubic-bezier takes four numbers, the x ones between 0 and 1")),
                    }
                } else {
                    return Err(format!(
                        "\"{t}\" is not an easing (linear, ease, ease-in, ease-out, ease-in-out, steps(N), cubic-bezier(a,b,c,d))"
                    ));
                }
            },
        })
    }

    /// The eased value of progress `t` (0 to 1).
    pub fn apply(&self, t: f32) -> f32 {
        let t = t.clamp(0., 1.);
        match *self {
            Easing::Linear => t,
            Easing::Steps(n) => {
                if t >= 1. {
                    1.
                } else {
                    (t * n as f32).floor() / n as f32
                }
            },
            Easing::Bezier(x1, y1, x2, y2) => {
                let b = |a: f32, b: f32, s: f32| 3. * a * (1. - s) * (1. - s) * s + 3. * b * (1. - s) * s * s + s * s * s;
                // Find the curve's parameter whose x is t.
                let (mut lo, mut hi) = (0f32, 1f32);
                for _ in 0..24 {
                    let mid = (lo + hi) / 2.;
                    if b(x1, x2, mid) < t {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                b(y1, y2, (lo + hi) / 2.)
            },
        }
    }
}

// ---- values ----

fn lerp_f(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// `a` to `b` at `t`, when the two are of a kind that has between.
pub fn lerp(a: &Val, b: &Val, t: f32) -> Option<Val> {
    match (a, b) {
        (Val::Num(a), Val::Num(b)) => Some(Val::Num(lerp_f(*a, *b, t))),
        (Val::Len(AbsoluteLength::Pixels(a)), Val::Len(AbsoluteLength::Pixels(b))) => {
            Some(Val::Len(px(lerp_f(f32::from(*a), f32::from(*b), t)).into()))
        },
        (Val::Len(AbsoluteLength::Rems(a)), Val::Len(AbsoluteLength::Rems(b))) => Some(Val::Len(rems(lerp_f(a.0, b.0, t)).into())),
        (Val::Color(a), Val::Color(b)) => {
            // From or to nothing: the color fades in or out, it doesn't go through black.
            let (a, b) = (*a, *b);
            let (a, b) = (if a.a == 0. { Hsla { a: 0., ..b } } else { a }, if b.a == 0. { Hsla { a: 0., ..a } } else { b });
            let (a, b) = (Rgba::from(a), Rgba::from(b));
            Some(Val::Color(Hsla::from(Rgba {
                r: lerp_f(a.r, b.r, t),
                g: lerp_f(a.g, b.g, t),
                b: lerp_f(a.b, b.b, t),
                a: lerp_f(a.a, b.a, t),
            })))
        },
        _ => None,
    }
}

/// `a` to `b` at `t`; kinds without a between jump at the end.
fn mix(a: &Val, b: &Val, t: f32) -> Val {
    lerp(a, b, t).unwrap_or_else(|| if t >= 1. { b.clone() } else { a.clone() })
}

// ---- keyframes ----

/// A keyframe set: the values at offsets 0 to 1.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Keyframes {
    stops: Vec<(f32, HashMap<Chan, Val>)>,
}

impl Keyframes {
    pub fn new(mut stops: Vec<(f32, HashMap<Chan, Val>)>) -> Keyframes {
        stops.sort_by(|a, b| a.0.total_cmp(&b.0));
        Keyframes { stops }
    }

    /// Where `key` of a keyframe set (`from`, `to`, `40%`) is: 0 to 1.
    pub fn offset(key: &str) -> Result<f32, String> {
        match key.trim() {
            "from" => Ok(0.),
            "to" => Ok(1.),
            k => match k.strip_suffix('%').and_then(|n| n.trim().parse::<f32>().ok()) {
                Some(n) if (0. ..=100.).contains(&n) => Ok(n / 100.),
                _ => Err(format!("\"{k}\" is not from, to or a percent from 0% to 100%")),
            },
        }
    }

    /// The values at progress `t`. A channel a set leaves out at 0 or 1 starts
    /// or ends at what `base` says it is (else it holds its nearest value).
    pub fn sample(&self, t: f32, base: &dyn Fn(Chan) -> Option<Val>) -> HashMap<Chan, Val> {
        let mut chans: Vec<Chan> = vec![];
        for (_, m) in &self.stops {
            for c in m.keys() {
                if !chans.contains(c) {
                    chans.push(*c);
                }
            }
        }
        let t = t.clamp(0., 1.);
        let mut out = HashMap::new();
        for c in chans {
            let before = self.stops.iter().rev().find(|(at, m)| *at <= t && m.contains_key(&c)).map(|(at, m)| (*at, m[&c].clone()));
            let after = self.stops.iter().find(|(at, m)| *at >= t && m.contains_key(&c)).map(|(at, m)| (*at, m[&c].clone()));
            let value = match (before, after) {
                (Some((a, va)), Some((b, vb))) => {
                    if b <= a {
                        va
                    } else {
                        mix(&va, &vb, (t - a) / (b - a))
                    }
                },
                (Some((a, va)), None) => match base(c) {
                    Some(vb) if a < 1. => mix(&va, &vb, (t - a) / (1. - a)),
                    _ => va,
                },
                (None, Some((b, vb))) => match base(c) {
                    Some(va) if b > 0. => mix(&va, &vb, t / b),
                    _ => vb,
                },
                (None, None) => continue,
            };
            out.insert(c, value);
        }
        out
    }
}

// ---- animations ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Normal,
    Reverse,
    Alternate,
    AlternateReverse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fill {
    None,
    Forwards,
    Backwards,
    Both,
}

/// How far an animation is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Phase {
    /// Waiting out its delay.
    Before,
    /// Running, at this progress of the keyframes (easing not applied).
    Active(f32),
    /// Over, at this progress.
    After(f32),
}

/// A keyframe set run with the timing of an `animation`.
#[derive(Debug, Clone)]
pub struct Anim {
    pub kf: Arc<Keyframes>,
    /// Seconds.
    pub duration: f64,
    pub delay: f64,
    /// How many times; infinite is `f64::INFINITY`.
    pub iterations: f64,
    pub direction: Direction,
    pub easing: Easing,
    pub fill: Fill,
    /// It plays while this holds.
    pub play: Option<Cond>,
}

impl Anim {
    fn directed(&self, index: f64, p: f32) -> f32 {
        let even = (index as u64).is_multiple_of(2);
        match self.direction {
            Direction::Normal => p,
            Direction::Reverse => 1. - p,
            Direction::Alternate => {
                if even {
                    p
                } else {
                    1. - p
                }
            },
            Direction::AlternateReverse => {
                if even {
                    1. - p
                } else {
                    p
                }
            },
        }
    }

    pub fn is_infinite(&self) -> bool {
        self.iterations.is_infinite()
    }

    /// Where it is `elapsed` seconds after it started (its delay included).
    pub fn phase(&self, elapsed: f64) -> Phase {
        let t = elapsed - self.delay;
        if t < 0. {
            return Phase::Before;
        }
        let over = |s: &Anim| {
            let n = s.iterations;
            if n.fract() == 0. { s.directed((n - 1.).max(0.), 1.) } else { s.directed(n.floor(), n.fract() as f32) }
        };
        if self.duration <= 0. {
            return Phase::After(over(self));
        }
        let iters = t / self.duration;
        if iters >= self.iterations {
            return Phase::After(over(self));
        }
        Phase::Active(self.directed(iters.floor(), iters.fract() as f32))
    }

    /// The channels at `phase`, with what the fill says of before and after.
    pub fn at(&self, phase: Phase, base: &dyn Fn(Chan) -> Option<Val>) -> HashMap<Chan, Val> {
        let p = match phase {
            Phase::Active(p) => p,
            Phase::Before if matches!(self.fill, Fill::Backwards | Fill::Both) => self.directed(0., 0.),
            Phase::After(p) if matches!(self.fill, Fill::Forwards | Fill::Both) => p,
            _ => return HashMap::new(),
        };
        self.kf.sample(self.easing.apply(p), base)
    }
}

/// A property that eases to a new value.
#[derive(Debug, Clone)]
pub struct Trans {
    /// `None`: every property.
    pub prop: Option<&'static str>,
    pub duration: f64,
    pub delay: f64,
    pub easing: Easing,
}

/// What a node does over time.
#[derive(Debug, Clone, Default)]
pub struct NodeMotion {
    pub anims: Vec<Anim>,
    pub trans: Vec<Trans>,
    /// Run once when the box appears, when it is clicked, when the pointer enters it.
    pub appear: Vec<Arc<Anim>>,
    pub click: Vec<Arc<Anim>>,
    pub hover: Vec<Arc<Anim>>,
}

impl NodeMotion {
    /// Whether it needs to know when the pointer is over the box.
    pub fn tracks_hover(&self) -> bool {
        !self.trans.is_empty() || !self.hover.is_empty() || self.anims.iter().any(|a| a.play.is_some())
    }

    pub fn is_empty(&self) -> bool {
        self.anims.is_empty() && self.trans.is_empty() && self.appear.is_empty() && self.click.is_empty() && self.hover.is_empty()
    }
}

// ---- state ----

#[derive(Debug, Default)]
struct AnimRun {
    elapsed: f64,
    /// Whether it played at the last frame: time passes only then.
    was_playing: bool,
}

#[derive(Debug)]
struct TransRun {
    from: Val,
    to: Val,
    start: f64,
    duration: f64,
    delay: f64,
    easing: Easing,
}

impl TransRun {
    fn progress(&self, now: f64) -> f32 {
        if self.duration <= 0. { 1. } else { (((now - self.start - self.delay) / self.duration).clamp(0., 1.)) as f32 }
    }

    fn value(&self, now: f64) -> Val {
        mix(&self.from, &self.to, self.easing.apply(self.progress(now)))
    }

    fn over(&self, now: f64) -> bool {
        now - self.start - self.delay >= self.duration
    }
}

/// One box's motion state.
#[derive(Default)]
struct BoxState {
    last: f64,
    runs: Vec<AnimRun>,
    once: Vec<(Arc<Anim>, f64)>,
    target: HashMap<&'static str, Val>,
    trans: HashMap<&'static str, TransRun>,
    hover: bool,
}

/// A box at a moment: what its animations and transitions say, and whether
/// any of it still moves (and so needs another frame).
#[derive(Debug, Default)]
pub struct Frame {
    pub chans: HashMap<Chan, Val>,
    pub running: bool,
}

/// The longest step one frame advances: a window that wasn't drawn for a while
/// picks up where it was, not at the end.
const MAX_STEP: f64 = 0.25;

/// Every box's motion state of every window, by key, and the actors' (see
/// `actor`).
#[derive(Default)]
pub struct Registry {
    boxes: HashMap<String, BoxState>,
    pub actors: HashMap<String, super::actor::Run>,
    /// The size of each actor's stage, as its last paint measured it.
    pub stages: HashMap<String, super::actor::Pt>,
    /// Where each actor's figure was painted: the x of its middle.
    pub centers: HashMap<String, f32>,
    /// Actors with a wake-up timer on its way.
    pub pending: std::collections::HashSet<String>,
    /// Which compile of the templates the state belongs to.
    generation: u64,
}

impl Registry {
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Drops everything when the templates changed.
    pub fn sync(&mut self, generation: u64) {
        if self.generation != generation {
            self.boxes.clear();
            self.generation = generation;
        }
    }

    /// Forgets the boxes whose key starts with `prefix` (a box that was hidden
    /// appears again when it comes back).
    pub fn forget(&mut self, prefix: &str) {
        if !self.boxes.is_empty() {
            self.boxes.retain(|k, _| !k.starts_with(prefix));
        }
        if !self.actors.is_empty() {
            self.actors.retain(|k, _| !k.starts_with(prefix));
            self.stages.retain(|k, _| !k.starts_with(prefix));
        }
    }

    pub fn hover(&self, key: &str) -> bool {
        self.boxes.get(key).is_some_and(|b| b.hover)
    }

    /// The pointer entered or left the box; true when that changed.
    pub fn set_hover(&mut self, key: &str, over: bool, m: &NodeMotion, now: f64) -> bool {
        let st = self.boxes.entry(key.to_owned()).or_default();
        if st.hover == over {
            return false;
        }
        st.hover = over;
        if over {
            for a in &m.hover {
                st.once.retain(|(o, _)| !Arc::ptr_eq(o, a));
                st.once.push((a.clone(), 0.));
            }
        }
        st.last = st.last.max(now);
        true
    }

    /// Runs `anim` once, from its start.
    pub fn trigger(&mut self, key: &str, anim: &Arc<Anim>) {
        let st = self.boxes.entry(key.to_owned()).or_default();
        st.once.retain(|(o, _)| !Arc::ptr_eq(o, anim));
        st.once.push((anim.clone(), 0.));
    }

    /// The box under `key` at `now` (seconds): its `target` properties (what its
    /// styles say now) eased from what they were, its animations sampled.
    /// `holds` reads a `play` condition; `base` says what a channel is when
    /// nothing animates it. With `reduce`, nothing moves: animations that end
    /// show their end, the others and transitions don't show.
    #[allow(clippy::too_many_arguments)]
    pub fn frame(
        &mut self,
        key: &str,
        m: &NodeMotion,
        now: f64,
        reduce: bool,
        target: &HashMap<&'static str, Val>,
        holds: &dyn Fn(&Cond) -> bool,
        base: &dyn Fn(Chan) -> Option<Val>,
    ) -> Frame {
        let fresh = !self.boxes.contains_key(key);
        let st = self.boxes.entry(key.to_owned()).or_default();
        if fresh {
            st.last = now;
            for a in &m.appear {
                st.once.push((a.clone(), 0.));
            }
        }
        if st.runs.len() != m.anims.len() {
            st.runs = m.anims.iter().map(|_| AnimRun::default()).collect();
        }
        let dt = (now - st.last).clamp(0., MAX_STEP);
        st.last = now;
        let mut out = Frame::default();

        // Transitions: a property whose target changed eases from where it was.
        for (&prop, to) in target {
            let Some(t) = m.trans.iter().rev().find(|t| t.prop.is_none_or(|p| p == prop)) else { continue };
            if reduce {
                st.target.insert(prop, to.clone());
                continue;
            }
            if let Some(prev) = st.target.get(prop)
                && prev != to
                && t.duration + t.delay > 0.
                && lerp(prev, to, 0.5).is_some()
            {
                let from = st.trans.get(prop).map_or_else(|| prev.clone(), |r| r.value(now));
                st.trans
                    .insert(prop, TransRun { from, to: to.clone(), start: now, duration: t.duration, delay: t.delay, easing: t.easing });
            }
            st.target.insert(prop, to.clone());
            if let Some(r) = st.trans.get(prop) {
                if r.over(now) {
                    st.trans.remove(prop);
                } else {
                    out.chans.insert(Chan::Prop(prop), r.value(now));
                    out.running = true;
                }
            }
        }

        // Animations: later ones win over earlier ones.
        for (a, run) in m.anims.iter().zip(&mut st.runs) {
            let playing = a.play.as_ref().is_none_or(holds);
            if reduce {
                if !a.is_infinite() {
                    out.chans.extend(a.at(Phase::After(a.directed(a.iterations.max(1.) - 1., 1.)), base));
                }
                continue;
            }
            if run.was_playing {
                run.elapsed += dt;
            }
            run.was_playing = playing;
            let phase = a.phase(run.elapsed);
            out.chans.extend(a.at(phase, base));
            if playing && !matches!(phase, Phase::After(_)) {
                out.running = true;
            }
        }
        for (a, elapsed) in &mut st.once {
            if reduce {
                continue;
            }
            *elapsed += dt;
            let phase = a.phase(*elapsed);
            out.chans.extend(a.at(phase, base));
            if !matches!(phase, Phase::After(_)) {
                out.running = true;
            }
        }
        if reduce {
            st.once.clear();
        } else {
            // A finished one that holds nothing goes.
            st.once.retain(|(a, e)| !(matches!(a.phase(*e), Phase::After(_)) && matches!(a.fill, Fill::None | Fill::Backwards)));
        }
        out
    }
}

/// The animations of a theme or settings file: name, then each stop's
/// properties.
pub type RawAnimations = BTreeMap<String, BTreeMap<String, BTreeMap<String, serde_json::Value>>>;

#[cfg(test)]
mod tests {
    use gpui_kit::{hsla, px};

    use super::{
        Anim, Arc, Chan, Cond, Direction, Easing, Fill, Frame, HashMap, Keyframes, MAX_STEP, NodeMotion, Phase, Registry, Trans, Transform,
        Val, lerp,
    };

    fn kf(from: f32, to: f32) -> Arc<Keyframes> {
        let stop = |v| HashMap::from([(Chan::Prop("opacity"), Val::Num(v))]);
        Arc::new(Keyframes::new(vec![(0., stop(from)), (1., stop(to))]))
    }

    fn anim(duration: f64) -> Anim {
        Anim {
            kf: kf(0., 1.),
            duration,
            delay: 0.,
            iterations: 1.,
            direction: Direction::Normal,
            easing: Easing::Linear,
            fill: Fill::None,
            play: None,
        }
    }

    fn num(f: &HashMap<Chan, Val>, p: &'static str) -> Option<f32> {
        match f.get(&Chan::Prop(p)) {
            Some(Val::Num(n)) => Some(*n),
            _ => None,
        }
    }

    fn no_base(_: Chan) -> Option<Val> {
        None
    }

    #[test]
    fn easings_hit_their_ends_and_steps_jump() {
        for e in ["linear", "ease", "ease-in", "ease-out", "ease-in-out", "cubic-bezier(0.1, 0.7, 1, 0.1)", "steps(4)"] {
            let e = Easing::parse(e).unwrap();
            assert!(e.apply(0.).abs() < 1e-3 && (e.apply(1.) - 1.).abs() < 1e-3, "{e:?}");
        }
        let steps = Easing::parse("steps(4)").unwrap();
        assert_eq!(steps.apply(0.3), 0.25);
        assert_eq!(steps.apply(0.99), 0.75);
        assert!(Easing::parse("ease-in").unwrap().apply(0.5) < 0.5);
        assert!(Easing::parse("ease-out").unwrap().apply(0.5) > 0.5);
        assert!(Easing::parse("steps(0)").is_err());
        assert!(Easing::parse("bouncy").is_err());
        assert!(Easing::parse("cubic-bezier(2,0,1,1)").is_err());
    }

    #[test]
    fn keyframes_interpolate_and_fall_back_to_the_base() {
        let stop = |v| HashMap::from([(Chan::Prop("opacity"), Val::Num(v))]);
        let k = Keyframes::new(vec![(0.5, stop(1.)), (1., stop(0.))]);
        // Not set at 0: it starts from the base.
        let base = |_: Chan| Some(Val::Num(0.5));
        assert_eq!(num(&k.sample(0.25, &base), "opacity"), Some(0.75));
        assert_eq!(num(&k.sample(0.75, &base), "opacity"), Some(0.5));
        // No base: it holds its nearest value.
        assert_eq!(num(&k.sample(0.25, &no_base), "opacity"), Some(1.));
        assert_eq!(Keyframes::offset("from"), Ok(0.));
        assert_eq!(Keyframes::offset("40%"), Ok(0.4));
        assert!(Keyframes::offset("140%").is_err());
    }

    #[test]
    fn colors_and_lengths_interpolate() {
        let a = Val::Color(hsla(0., 0., 0., 1.));
        let b = Val::Color(hsla(0., 0., 1., 1.));
        let Some(Val::Color(c)) = lerp(&a, &b, 0.5) else { panic!() };
        assert!((c.l - 0.5).abs() < 0.05, "{c:?}");
        // Fading in from nothing keeps the color.
        let Some(Val::Color(c)) = lerp(&Val::Color(hsla(0., 0., 0., 0.)), &Val::Color(hsla(0.5, 1., 0.5, 1.)), 0.5) else { panic!() };
        assert!((c.a - 0.5).abs() < 1e-3 && (c.h - 0.5).abs() < 0.02, "{c:?}");
        assert_eq!(lerp(&Val::Len(px(10.).into()), &Val::Len(px(20.).into()), 0.5), Some(Val::Len(px(15.).into())));
        assert_eq!(lerp(&Val::Text("a".into()), &Val::Text("b".into()), 0.5), None);
    }

    #[test]
    fn phases_follow_delay_iterations_and_direction() {
        let mut a = anim(2.);
        a.delay = 1.;
        a.iterations = 2.;
        assert_eq!(a.phase(0.5), Phase::Before);
        assert_eq!(a.phase(2.), Phase::Active(0.5));
        assert_eq!(a.phase(4.), Phase::Active(0.5));
        assert_eq!(a.phase(5.), Phase::After(1.));
        a.direction = Direction::Alternate;
        // The second run goes back.
        assert_eq!(a.phase(4.), Phase::Active(0.5));
        assert_eq!(a.phase(4.5), Phase::Active(0.25));
        assert_eq!(a.phase(6.), Phase::After(0.));
        a.direction = Direction::Reverse;
        assert_eq!(a.phase(1.5), Phase::Active(0.75));
        a.iterations = f64::INFINITY;
        assert_eq!(a.phase(1000.), Phase::Active(1. - (999.0f64 / 2.0).fract() as f32));
        a.iterations = 1.5;
        a.direction = Direction::Normal;
        assert_eq!(a.phase(100.), Phase::After(0.5));
    }

    #[test]
    fn fill_holds_the_ends() {
        let mut a = anim(1.);
        a.delay = 1.;
        a.kf = kf(0.2, 0.8);
        assert!(a.at(Phase::Before, &no_base).is_empty());
        assert!(a.at(Phase::After(1.), &no_base).is_empty());
        a.fill = Fill::Both;
        assert_eq!(num(&a.at(Phase::Before, &no_base), "opacity"), Some(0.2));
        assert_eq!(num(&a.at(Phase::After(1.), &no_base), "opacity"), Some(0.8));
        a.fill = Fill::Forwards;
        assert!(a.at(Phase::Before, &no_base).is_empty());
        assert!(!a.at(Phase::After(1.), &no_base).is_empty());
    }

    fn motion(a: Anim) -> NodeMotion {
        NodeMotion { anims: vec![a], ..Default::default() }
    }

    /// The box at `now`, drawn every 0.1 s from the last time it was.
    fn run(reg: &mut Registry, m: &NodeMotion, now: f64) -> Frame {
        let mut at = reg.boxes.get("k").map_or(now, |b| b.last);
        loop {
            at = (at + 0.1).min(now);
            let f = reg.frame("k", m, at, false, &HashMap::new(), &|_| true, &no_base);
            if at >= now {
                return f;
            }
        }
    }

    #[test]
    fn a_box_animates_and_then_asks_for_no_frames() {
        let m = motion(anim(1.));
        let mut reg = Registry::default();
        let f = run(&mut reg, &m, 10.);
        assert!(f.running);
        assert_eq!(num(&f.chans, "opacity"), Some(0.));
        let f = run(&mut reg, &m, 10.5);
        assert_eq!(num(&f.chans, "opacity"), Some(0.5));
        assert!(f.running);
        // Over: nothing set, nothing to draw again.
        let f = run(&mut reg, &m, 11.2);
        assert!(!f.running);
        assert!(f.chans.is_empty());
        // And it stays idle.
        let f = run(&mut reg, &m, 500.);
        assert!(!f.running && f.chans.is_empty());
    }

    #[test]
    fn a_box_without_motion_requests_nothing() {
        let mut reg = Registry::default();
        let f = run(&mut reg, &NodeMotion::default(), 1.);
        assert!(!f.running && f.chans.is_empty());
        assert!(NodeMotion::default().is_empty() && !NodeMotion::default().tracks_hover());
    }

    #[test]
    fn a_long_gap_does_not_skip_a_running_animation_to_its_end() {
        let m = motion(anim(10.));
        let mut reg = Registry::default();
        run(&mut reg, &m, 0.);
        let f = reg.frame("k", &m, 3600., false, &HashMap::new(), &|_| true, &no_base);
        assert!(f.running);
        assert_eq!(num(&f.chans, "opacity"), Some((MAX_STEP / 10.) as f32));
    }

    #[test]
    fn play_pauses_while_its_condition_fails() {
        let mut a = anim(10.);
        a.play = Some(Cond::parse("hover").unwrap());
        let m = motion(a);
        let mut reg = Registry::default();
        let hold = std::cell::Cell::new(false);
        let at = |reg: &mut Registry, now| reg.frame("k", &m, now, false, &HashMap::new(), &|_| hold.get(), &no_base);
        assert!(!at(&mut reg, 0.).running);
        assert!(!at(&mut reg, 0.2).running);
        hold.set(true);
        at(&mut reg, 0.4);
        let f = at(&mut reg, 0.6);
        assert!(f.running);
        assert_eq!(num(&f.chans, "opacity"), Some(0.02));
        hold.set(false);
        at(&mut reg, 0.8);
        let f = at(&mut reg, 5.);
        assert!(!f.running);
        assert_eq!(num(&f.chans, "opacity"), Some(0.04));
    }

    #[test]
    fn a_transition_eases_a_changed_target() {
        let m = NodeMotion { trans: vec![Trans { prop: None, duration: 1., delay: 0., easing: Easing::Linear }], ..Default::default() };
        let mut reg = Registry::default();
        let at = |reg: &mut Registry, now, v: f32| {
            reg.frame("k", &m, now, false, &HashMap::from([("opacity", Val::Num(v))]), &|_| true, &no_base)
        };
        // The first value is shown as it is.
        let f = at(&mut reg, 0., 0.);
        assert!(!f.running && f.chans.is_empty());
        let f = at(&mut reg, 1., 1.);
        assert!(f.running);
        assert_eq!(num(&f.chans, "opacity"), Some(0.));
        let f = at(&mut reg, 1.5, 1.);
        assert_eq!(num(&f.chans, "opacity"), Some(0.5));
        // Turned back half way: it goes from where it is.
        let f = at(&mut reg, 1.5, 0.);
        assert_eq!(num(&f.chans, "opacity"), Some(0.5));
        let f = at(&mut reg, 2., 0.);
        assert_eq!(num(&f.chans, "opacity"), Some(0.25));
        let f = at(&mut reg, 3., 0.);
        assert!(!f.running && f.chans.is_empty());
    }

    #[test]
    fn reduce_motion_jumps_to_ends() {
        let mut a = anim(1.);
        a.fill = Fill::Forwards;
        let mut inf = anim(1.);
        inf.iterations = f64::INFINITY;
        let m = NodeMotion {
            anims: vec![a, inf],
            trans: vec![Trans { prop: None, duration: 1., delay: 0., easing: Easing::Linear }],
            ..Default::default()
        };
        let mut reg = Registry::default();
        let at = |reg: &mut Registry, now, v: f32| {
            reg.frame("k", &m, now, true, &HashMap::from([("opacity", Val::Num(v))]), &|_| true, &no_base)
        };
        at(&mut reg, 0., 0.);
        let f = at(&mut reg, 0.1, 1.);
        assert!(!f.running);
        assert_eq!(num(&f.chans, "opacity"), Some(1.));
    }

    #[test]
    fn events_run_once_and_forgetting_makes_a_box_appear_again() {
        let once = Arc::new(anim(1.));
        let m = NodeMotion { click: vec![once.clone()], appear: vec![Arc::new(anim(1.))], ..Default::default() };
        let mut reg = Registry::default();
        // Appearing runs the appear animation.
        assert!(run(&mut reg, &m, 0.).running);
        assert!(!run(&mut reg, &m, 2.).running);
        reg.trigger("k", &once);
        assert!(run(&mut reg, &m, 2.1).running);
        assert!(!run(&mut reg, &m, 3.5).running);
        reg.forget("k");
        assert!(run(&mut reg, &m, 4.).running);
    }

    #[test]
    fn hovering_runs_the_hover_animation_and_is_tracked() {
        let m = NodeMotion { hover: vec![Arc::new(anim(1.))], ..Default::default() };
        let mut reg = Registry::default();
        assert!(m.tracks_hover());
        assert!(!reg.hover("k"));
        assert!(reg.set_hover("k", true, &m, 0.));
        assert!(!reg.set_hover("k", true, &m, 0.1));
        assert!(reg.hover("k"));
        assert!(run(&mut reg, &m, 0.2).running);
        assert!(reg.set_hover("k", false, &m, 0.3));
    }

    #[test]
    fn transform_reads_its_channels() {
        let chans = HashMap::from([(Chan::Tx, Val::Num(4.)), (Chan::Scale, Val::Num(2.))]);
        assert_eq!(Transform::of(&chans), Transform { tx: 4., ty: 0., scale: 2., rotate: 0. });
        assert!(Transform::default().is_identity());
    }
}

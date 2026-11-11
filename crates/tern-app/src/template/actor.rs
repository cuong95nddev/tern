//! Actors: small figures that move over a stage (see the `actors` setting).
//!
//! This is the pure part: reading an actor from its description, and the
//! motion of its moves as a function of time. Nothing here knows of gpui, so
//! it is tested without a window.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value as Json;

/// An actor as written in `actors`.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ActorSpec {
    /// The figure's box in pixels: `[width, height]`.
    #[schemars(with = "Option<Vec<f32>>")]
    pub size: Option<[f32; 2]>,
    /// What it shows in each state, by name: `{"image": "x.gif"}` (a PNG, SVG
    /// or GIF, an animated GIF plays by itself) or `{"sprite": "x.png",
    /// "frames": 6, "fps": 12}` (a sheet of equal frames side by side;
    /// `"loop": false` plays it once).
    pub looks: BTreeMap<String, LookSpec>,
    /// The way the pictures face: `right` (the default) or `left`. They are
    /// drawn mirrored when the figure moves the other way.
    pub faces: Option<String>,
    /// What it does, one after another, forever: `rest`, `wander`, `patrol`,
    /// `visit` and `hop` (see the config README).
    pub moves: Vec<MoveSpec>,
    /// `list` (the default) or `random`.
    pub order: Option<String>,
    /// What a click on the figure does: a move, or `flee`, and `then` it goes
    /// back to its place (`rest`) or goes on with `moves` (`continue`).
    pub click: Option<MoveSpec>,
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct LookSpec {
    /// A picture: PNG, SVG or GIF, relative to the config folder (or absolute, or `~/…`).
    pub image: Option<String>,
    /// A sheet of `frames` equal frames side by side, played at `fps`.
    pub sprite: Option<String>,
    pub frames: Option<u32>,
    pub fps: Option<f32>,
    /// Whether a sprite plays again and again (default true).
    #[serde(rename = "loop")]
    pub looping: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct MoveSpec {
    /// `rest`, `wander`, `patrol`, `visit`, `hop`; in `click` also `flee`.
    #[serde(rename = "do")]
    pub do_: String,
    /// What it shows while it moves (default: the actor's first look).
    pub look: Option<String>,
    /// What it shows while it stands still (default: the actor's first look).
    pub rest_look: Option<String>,
    /// `rest`: where it stays, `[x, y]` in the stage: 0 to 1 is a fraction of
    /// the stage, more (or `"40px"`) is pixels; `y` is where the feet are.
    #[schemars(with = "Option<Vec<serde_json::Value>>")]
    pub at: Option<Json>,
    /// `rest`: seconds, a number or `[min, max]`.
    #[serde(rename = "for")]
    #[schemars(with = "Option<serde_json::Value>")]
    pub for_: Option<Json>,
    /// `wander`: seconds it stands between runs, a number or `[min, max]`.
    #[schemars(with = "Option<serde_json::Value>")]
    pub pause: Option<Json>,
    /// `wander`, `patrol`: how many runs, a number or `[min, max]`.
    #[schemars(with = "Option<serde_json::Value>")]
    pub times: Option<Json>,
    /// Pixels per second.
    pub speed: Option<f32>,
    /// `patrol`: one end.
    #[schemars(with = "Option<Vec<serde_json::Value>>")]
    pub from: Option<Json>,
    /// `patrol`, `hop`: the other end.
    #[schemars(with = "Option<Vec<serde_json::Value>>")]
    pub to: Option<Json>,
    /// `visit`: `"across"` (out one side of the stage, in the other) or a list of points.
    #[schemars(with = "Option<serde_json::Value>")]
    pub path: Option<Json>,
    /// `hop`: how high the jump goes, in pixels.
    pub height: Option<f32>,
    /// `click`: `rest` or `continue`.
    pub then: Option<String>,
}

// ---- compiled ----

/// One coordinate of a point: a share of the stage, or pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Coord {
    Frac(f32),
    Px(f32),
}

impl Coord {
    fn at(self, extent: f32) -> f32 {
        match self {
            Coord::Frac(f) => f * extent,
            Coord::Px(p) => p,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point(pub Coord, pub Coord);

/// A point in the stage, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Pt {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub fn at(&self, stage: Pt) -> Pt {
        Pt { x: self.0.at(stage.x), y: self.1.at(stage.y) }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum LookKind {
    Image(PathBuf),
    Sprite { path: PathBuf, frames: u32, fps: f32, looping: bool },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Look {
    pub name: String,
    pub kind: LookKind,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Range {
    pub min: f32,
    pub max: f32,
}

impl Range {
    fn one(v: f32) -> Range {
        Range { min: v, max: v }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Route {
    Across,
    Points(Vec<Point>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Rest {
        at: Option<Point>,
        secs: Range,
    },
    Wander {
        pause: Range,
        times: Range,
    },
    Patrol {
        from: Point,
        to: Point,
        times: Range,
    },
    Visit {
        route: Route,
    },
    Hop {
        to: Point,
        height: f32,
    },
    /// A click's: run away from the pointer.
    Flee,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Move {
    pub kind: Kind,
    /// Indexes into the actor's looks.
    pub look: usize,
    pub rest_look: usize,
    pub speed: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Then {
    Rest,
    Continue,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Click {
    pub mv: Move,
    pub then: Then,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActorDef {
    pub size: (f32, f32),
    pub looks: Vec<Look>,
    /// The pictures face left.
    pub faces_left: bool,
    pub moves: Vec<Move>,
    pub random: bool,
    pub click: Option<Click>,
}

/// How fast it walks to a place where it has no `speed` to go by.
const WALK_SPEED: f32 = 120.;
/// The speed of a `hop`.
const HOP_SPEED: f32 = 150.;
const DEFAULT_SIZE: (f32, f32) = (32., 32.);

fn path_of(dir: &Path, text: &str) -> PathBuf {
    if let Some(rest) = text.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
    {
        return PathBuf::from(home).join(rest);
    }
    dir.join(text)
}

struct Reader<'a> {
    name: &'a str,
    problems: &'a mut Vec<String>,
}

impl Reader<'_> {
    fn bad(&mut self, at: &str, msg: &str) {
        self.problems.push(format!("actors.{}.{at}: {msg}", self.name));
    }

    fn coord(&mut self, v: &Json, at: &str) -> Option<Coord> {
        match v {
            Json::Number(n) => {
                let n = n.as_f64()? as f32;
                Some(if (0. ..=1.).contains(&n) { Coord::Frac(n) } else { Coord::Px(n) })
            },
            Json::String(s) => {
                let s = s.trim();
                if let Some(n) = s.strip_suffix("px").and_then(|n| n.trim().parse::<f32>().ok()) {
                    Some(Coord::Px(n))
                } else if let Some(n) = s.strip_suffix('%').and_then(|n| n.trim().parse::<f32>().ok()) {
                    Some(Coord::Frac(n / 100.))
                } else {
                    self.bad(at, &format!("\"{s}\" is not a number, \"40px\" or \"50%\""));
                    None
                }
            },
            _ => {
                self.bad(at, "a point's x and y are numbers or \"40px\"");
                None
            },
        }
    }

    fn point(&mut self, v: &Json, at: &str) -> Option<Point> {
        match v.as_array().map(Vec::as_slice) {
            Some([x, y]) => Some(Point(self.coord(x, at)?, self.coord(y, at)?)),
            _ => {
                self.bad(at, "a point is [x, y]");
                None
            },
        }
    }

    fn range(&mut self, v: Option<&Json>, default: Range, at: &str) -> Range {
        let n = |v: &Json| v.as_f64().filter(|n| n.is_finite() && *n >= 0.).map(|n| n as f32);
        match v {
            None => default,
            Some(v) => match (n(v), v.as_array().map(Vec::as_slice)) {
                (Some(n), _) => Range::one(n),
                (None, Some([a, b])) => match (n(a), n(b)) {
                    (Some(a), Some(b)) => Range { min: a.min(b), max: a.max(b) },
                    _ => {
                        self.bad(at, "a number from 0 or [min, max]");
                        default
                    },
                },
                _ => {
                    self.bad(at, "a number from 0 or [min, max]");
                    default
                },
            },
        }
    }

    fn look(&mut self, looks: &[Look], name: Option<&String>, at: &str) -> usize {
        match name {
            None => 0,
            Some(n) => looks.iter().position(|l| l.name == *n).unwrap_or_else(|| {
                let known: Vec<&str> = looks.iter().map(|l| l.name.as_str()).collect();
                self.bad(at, &format!("no look \"{n}\" (there are: {})", known.join(", ")));
                0
            }),
        }
    }

    fn mv(&mut self, m: &MoveSpec, looks: &[Look], at: &str, in_click: bool) -> Option<Move> {
        let kind = match m.do_.as_str() {
            "rest" => {
                let at_pt = m.at.as_ref().and_then(|v| self.point(v, &format!("{at}.at")));
                Kind::Rest { at: at_pt, secs: self.range(m.for_.as_ref(), Range { min: 5., max: 5. }, &format!("{at}.for")) }
            },
            "wander" => Kind::Wander {
                pause: self.range(m.pause.as_ref(), Range { min: 1., max: 3. }, &format!("{at}.pause")),
                times: self.range(m.times.as_ref(), Range::one(3.), &format!("{at}.times")),
            },
            "patrol" => {
                let from = m.from.as_ref().and_then(|p| self.point(p, &format!("{at}.from")));
                let to = m.to.as_ref().and_then(|p| self.point(p, &format!("{at}.to")));
                let (Some(from), Some(to)) = (from, to) else {
                    self.bad(at, "a patrol needs from and to");
                    return None;
                };
                Kind::Patrol { from, to, times: self.range(m.times.as_ref(), Range::one(1.), &format!("{at}.times")) }
            },
            "visit" => {
                let route = match m.path.as_ref() {
                    None => Route::Across,
                    Some(Json::String(s)) if s == "across" => Route::Across,
                    Some(Json::Array(points)) => Route::Points(
                        points.iter().enumerate().filter_map(|(i, p)| self.point(p, &format!("{at}.path.{i}"))).collect::<Vec<_>>(),
                    ),
                    Some(_) => {
                        self.bad(&format!("{at}.path"), "\"across\" or a list of points");
                        Route::Across
                    },
                };
                Kind::Visit { route }
            },
            "hop" => match m.to.as_ref().and_then(|t| self.point(t, &format!("{at}.to"))) {
                Some(to) => Kind::Hop { to, height: m.height.filter(|h| *h >= 0.).unwrap_or(16.) },
                None => {
                    self.bad(at, "a hop needs a `to`");
                    return None;
                },
            },
            "flee" if in_click => Kind::Flee,
            other => {
                let more = if in_click { ", flee" } else { "" };
                self.bad(&format!("{at}.do"), &format!("\"{other}\" is not one of rest, wander, patrol, visit, hop{more}"));
                return None;
            },
        };
        let speed = match m.speed {
            Some(s) if s > 0. && s.is_finite() => s,
            Some(_) => {
                self.bad(&format!("{at}.speed"), "pixels per second, more than 0");
                WALK_SPEED
            },
            None if matches!(kind, Kind::Hop { .. }) => HOP_SPEED,
            None => WALK_SPEED,
        };
        let look = self.look(looks, m.look.as_ref(), &format!("{at}.look"));
        let rest_look = self.look(looks, m.rest_look.as_ref(), &format!("{at}.rest_look"));
        Some(Move { kind, look, rest_look, speed })
    }
}

/// Reads the `actors` of the settings: what's wrong goes to `problems`, the
/// rest is kept. Pictures are looked for in `dir`.
pub fn compile(raw: &BTreeMap<String, ActorSpec>, dir: &Path, problems: &mut Vec<String>) -> BTreeMap<String, Arc<ActorDef>> {
    let mut out = BTreeMap::new();
    for (name, spec) in raw {
        let mut r = Reader { name, problems };
        let mut looks = vec![];
        for (look, l) in &spec.looks {
            let at = format!("looks.{look}");
            let file = |r: &mut Reader, text: &str, field: &str| {
                let p = path_of(dir, text);
                if !p.is_file() {
                    r.bad(&format!("{at}.{field}"), &format!("no file {}", p.display()));
                }
                p
            };
            let kind = match (&l.image, &l.sprite) {
                (Some(i), None) => LookKind::Image(file(&mut r, i, "image")),
                (None, Some(s)) => {
                    let frames = l.frames.unwrap_or(1);
                    if frames == 0 {
                        r.bad(&format!("{at}.frames"), "at least 1");
                    }
                    let fps = l.fps.unwrap_or(12.);
                    if fps <= 0. || !fps.is_finite() {
                        r.bad(&format!("{at}.fps"), "more than 0");
                    }
                    LookKind::Sprite {
                        path: file(&mut r, s, "sprite"),
                        frames: frames.max(1),
                        fps: if fps > 0. && fps.is_finite() { fps } else { 12. },
                        looping: l.looping.unwrap_or(true),
                    }
                },
                _ => {
                    r.bad(&at, "a look is an `image` or a `sprite`");
                    continue;
                },
            };
            looks.push(Look { name: look.clone(), kind });
        }
        if looks.is_empty() {
            r.bad("looks", "an actor needs at least one look");
            continue;
        }
        let size = match spec.size {
            Some([w, h]) if w > 0. && h > 0. => (w, h),
            Some(_) => {
                r.bad("size", "[width, height], more than 0");
                DEFAULT_SIZE
            },
            None => DEFAULT_SIZE,
        };
        let faces_left = match spec.faces.as_deref() {
            None | Some("right") => false,
            Some("left") => true,
            Some(f) => {
                r.bad("faces", &format!("\"{f}\" is not left or right"));
                false
            },
        };
        let random = match spec.order.as_deref() {
            None | Some("list") => false,
            Some("random") => true,
            Some(o) => {
                r.bad("order", &format!("\"{o}\" is not list or random"));
                false
            },
        };
        let moves: Vec<Move> = spec.moves.iter().enumerate().filter_map(|(i, m)| r.mv(m, &looks, &format!("moves.{i}"), false)).collect();
        let click = spec.click.as_ref().and_then(|c| {
            let mv = r.mv(c, &looks, "click", true)?;
            let then = match c.then.as_deref() {
                None | Some("rest") => Then::Rest,
                Some("continue") => Then::Continue,
                Some(t) => {
                    r.bad("click.then", &format!("\"{t}\" is not rest or continue"));
                    Then::Rest
                },
            };
            Some(Click { mv, then })
        });
        out.insert(name.clone(), Arc::new(ActorDef { size, looks, faces_left, moves, random, click }));
    }
    out
}

// ---- motion ----

/// A part of a move: the figure goes `from` to `to` in `dur` seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct Seg {
    pub dur: f32,
    pub from: Pt,
    pub to: Pt,
    /// How high a jump goes in the middle; 0 for a run.
    pub arc: f32,
    pub look: usize,
    pub moving: bool,
    /// It appears at `to` at once.
    pub jump: bool,
}

impl Seg {
    /// Where the feet are `t` seconds in.
    pub fn at(&self, t: f32) -> Pt {
        if self.jump {
            return self.to;
        }
        let u = if self.dur <= 0. { 1. } else { (t / self.dur).clamp(0., 1.) };
        Pt { x: self.from.x + (self.to.x - self.from.x) * u, y: self.from.y + (self.to.y - self.from.y) * u - self.arc * 4. * u * (1. - u) }
    }
}

/// A small, fast random source: motion doesn't need more.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// 0 up to 1.
    pub fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }

    fn between(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }

    fn range(&mut self, r: &Range) -> f32 {
        self.between(r.min, r.max)
    }

    fn count(&mut self, r: &Range) -> usize {
        (self.between(r.min.round(), r.max.round() + 0.999).floor() as usize).max(1)
    }
}

/// A gap this long between two frames is a sleep: the moves missed aren't replayed.
pub const SKIP_AFTER: f64 = 2.;

/// What to draw.
#[derive(Debug, Clone, PartialEq)]
pub struct Pose {
    /// Where the feet are, in the stage.
    pub at: Pt,
    pub look: usize,
    pub moving: bool,
    /// The pictures are drawn mirrored.
    pub mirrored: bool,
    /// Seconds until what it does changes, when it stands still.
    pub wake: Option<f32>,
    /// Seconds since it took this look (a sprite that doesn't loop plays by it).
    pub age: f32,
}

/// An actor's state on a stage: where it is and what it is doing.
pub struct Run {
    pub def: Arc<ActorDef>,
    stage: Pt,
    pos: Pt,
    home: Pt,
    left: bool,
    queue: VecDeque<Seg>,
    cur: Option<(Seg, f32)>,
    next: usize,
    /// After the queue: the move list goes on from the first `rest`.
    to_rest: bool,
    last: f64,
    rng: Rng,
}

impl Run {
    pub fn new(def: Arc<ActorDef>, stage: Pt, now: f64, seed: u64) -> Run {
        let mut rng = Rng::new(seed);
        let rest = def.moves.iter().find_map(|m| match &m.kind {
            Kind::Rest { at, .. } => *at,
            _ => None,
        });
        let home = rest.map_or(Pt { x: stage.x / 2., y: stage.y }, |p| p.at(stage));
        let next = if def.random && !def.moves.is_empty() { (rng.unit() * def.moves.len() as f32) as usize % def.moves.len() } else { 0 };
        let mut run =
            Run { def, stage, pos: home, home, left: false, queue: VecDeque::new(), cur: None, next, to_rest: false, last: now, rng };
        // Its place is somewhere the figure fits.
        run.home = run.clamp(home);
        run.pos = run.home;
        run
    }

    #[cfg(test)]
    pub fn stage(&self) -> Pt {
        self.stage
    }

    /// The stage changed size: it keeps its place in it, and plans again.
    pub fn resize(&mut self, stage: Pt) {
        if self.stage == stage {
            return;
        }
        let (sx, sy) = (stage.x / self.stage.x.max(1.), stage.y / self.stage.y.max(1.));
        self.pos = Pt { x: self.pos.x * sx, y: self.pos.y * sy };
        self.home = Pt { x: self.home.x * sx, y: self.home.y * sy };
        self.stage = stage;
        self.queue.clear();
        self.cur = None;
    }

    fn fig(&self) -> (f32, f32) {
        self.def.size
    }

    /// A point of the stage the figure can stand on.
    fn clamp(&self, p: Pt) -> Pt {
        let (w, h) = self.fig();
        let x = if self.stage.x > w { p.x.clamp(w / 2., self.stage.x - w / 2.) } else { self.stage.x / 2. };
        Pt { x, y: p.y.clamp(h.min(self.stage.y), self.stage.y.max(0.)) }
    }

    fn walk_look(&self) -> usize {
        self.def.moves.iter().find(|m| !matches!(m.kind, Kind::Rest { .. })).map_or(0, |m| m.look)
    }

    fn seg_to(&self, from: Pt, to: Pt, speed: f32, look: usize) -> Seg {
        let dist = ((to.x - from.x).powi(2) + (to.y - from.y).powi(2)).sqrt();
        Seg { dur: dist / speed, from, to, arc: 0., look, moving: true, jump: false }
    }

    fn stand(&self, at: Pt, secs: f32, look: usize) -> Seg {
        Seg { dur: secs, from: at, to: at, arc: 0., look, moving: false, jump: false }
    }

    /// The segments of `m` from `from`; the last ends where it stands.
    pub fn plan(&mut self, m: &Move, from: Pt, pointer_dx: f32) -> Vec<Seg> {
        let mut segs = vec![];
        let mut at = from;
        let near = |a: Pt, b: Pt| (a.x - b.x).abs() < 1. && (a.y - b.y).abs() < 1.;
        match &m.kind {
            Kind::Rest { at: place, secs } => {
                let place = place.map_or(self.home, |p| self.clamp(p.at(self.stage)));
                if !near(at, place) {
                    segs.push(self.seg_to(at, place, m.speed, self.walk_look()));
                }
                let secs = self.rng.range(secs).max(0.05);
                segs.push(self.stand(place, secs, m.look));
            },
            Kind::Wander { pause, times } => {
                for _ in 0..self.rng.count(times) {
                    let (rx, ry) = (self.rng.between(0., self.stage.x), self.rng.between(0., self.stage.y));
                    let to = self.clamp(Pt { x: rx, y: ry });
                    segs.push(self.seg_to(at, to, m.speed, m.look));
                    let secs = self.rng.range(pause).max(0.05);
                    segs.push(self.stand(to, secs, m.rest_look));
                    at = to;
                }
            },
            Kind::Patrol { from: a, to: b, times } => {
                let (a, b) = (self.clamp(a.at(self.stage)), self.clamp(b.at(self.stage)));
                if !near(at, a) {
                    segs.push(self.seg_to(at, a, m.speed, m.look));
                }
                for _ in 0..self.rng.count(times) {
                    segs.push(self.seg_to(a, b, m.speed, m.look));
                    segs.push(self.seg_to(b, a, m.speed, m.look));
                }
            },
            Kind::Visit { route } => match route {
                Route::Across => {
                    let (w, _) = self.fig();
                    let left_first = self.rng.unit() < 0.5;
                    let (out, back_in) = if left_first { (-w, self.stage.x + w) } else { (self.stage.x + w, -w) };
                    segs.push(self.seg_to(at, Pt { x: out, y: at.y }, m.speed, m.look));
                    segs.push(Seg {
                        dur: 0.,
                        from: Pt { x: out, y: at.y },
                        to: Pt { x: back_in, y: at.y },
                        arc: 0.,
                        look: m.look,
                        moving: true,
                        jump: true,
                    });
                    segs.push(self.seg_to(Pt { x: back_in, y: at.y }, from, m.speed, m.look));
                },
                Route::Points(points) => {
                    for p in points {
                        let to = self.clamp(p.at(self.stage));
                        segs.push(self.seg_to(at, to, m.speed, m.look));
                        at = to;
                    }
                    segs.push(self.seg_to(at, from, m.speed, m.look));
                },
            },
            Kind::Hop { to, height } => {
                let to = self.clamp(to.at(self.stage));
                let mut s = self.seg_to(at, to, m.speed, m.look);
                s.dur = s.dur.max(0.25);
                s.arc = *height;
                segs.push(s);
            },
            Kind::Flee => {
                // Away from the pointer; the other way when a wall is there.
                let (w, _) = self.fig();
                let room = |dir: f32| if dir > 0. { self.stage.x - w / 2. - at.x } else { at.x - w / 2. };
                let dir = if pointer_dx < 0. { 1. } else { -1. };
                let dir = if room(dir) < 20. && room(-dir) > room(dir) { -dir } else { dir };
                let far = room(dir).max(0.);
                let x = at.x + dir * self.rng.between(far * 0.5, far);
                let y = self.rng.between(self.stage.y * 0.5, self.stage.y);
                segs.push(self.seg_to(at, self.clamp(Pt { x, y }), m.speed, m.look));
            },
        }
        segs
    }

    fn plan_next(&mut self) {
        let n = self.def.moves.len();
        if n == 0 {
            // Nothing to do: it stands at home.
            let s = self.stand(self.pos, 1., 0);
            self.queue.push_back(s);
            return;
        }
        let ix = if self.to_rest {
            self.to_rest = false;
            self.def.moves.iter().position(|m| matches!(m.kind, Kind::Rest { .. })).unwrap_or(self.next % n)
        } else if self.def.random {
            // Not the same one twice, when there is a choice.
            let mut ix = (self.rng.unit() * n as f32) as usize % n;
            if n > 1 && ix == self.next % n {
                ix = (ix + 1) % n;
            }
            ix
        } else {
            self.next % n
        };
        self.next = if self.def.random { ix } else { ix + 1 };
        let m = self.def.moves[ix].clone();
        let segs = self.plan(&m, self.pos, 0.);
        self.queue.extend(segs);
    }

    /// A click: the figure reacts from where it is. `pointer_dx` is the
    /// pointer's side of the figure's middle (negative: left of it).
    pub fn click(&mut self, pointer_dx: f32) {
        let Some(c) = self.def.click.clone() else { return };
        self.queue.clear();
        self.cur = None;
        let segs = self.plan(&c.mv, self.pos, pointer_dx);
        self.queue.extend(segs);
        match c.then {
            Then::Rest => {
                // Back to its place, and it goes on from there.
                let end = self.queue.back().map_or(self.pos, |s| s.to);
                if (end.x - self.home.x).abs() > 1. || (end.y - self.home.y).abs() > 1. {
                    let s = self.seg_to(end, self.home, c.mv.speed, c.mv.look);
                    self.queue.push_back(s);
                }
                self.to_rest = true;
            },
            Then::Continue => {},
        }
    }

    /// The figure at its place, not moving (for those who asked for less motion).
    pub fn still(&self) -> Pose {
        let look = self.def.moves.iter().find_map(|m| matches!(m.kind, Kind::Rest { .. }).then_some(m.look)).unwrap_or(0);
        Pose { at: self.home, look, moving: false, mirrored: self.left != self.def.faces_left, wake: None, age: 0. }
    }

    /// Everything planned is over: the figure stands where the last part ends.
    fn skip(&mut self) {
        let end = self.queue.back().map(|s| s.to).or_else(|| self.cur.as_ref().map(|(s, _)| s.to));
        if let Some(end) = end {
            self.pos = end;
        }
        self.queue.clear();
        self.cur = None;
    }

    /// Moves time on to `now` (seconds) and says where the figure is.
    pub fn advance(&mut self, now: f64) -> Pose {
        let mut dt = (now - self.last).max(0.);
        self.last = now;
        if dt > SKIP_AFTER {
            self.skip();
            dt = 0.;
        }
        let mut dt = dt as f32;
        for _ in 0..256 {
            if self.cur.is_none() {
                if self.queue.is_empty() {
                    self.plan_next();
                }
                let Some(seg) = self.queue.pop_front() else { break };
                if seg.moving && !seg.jump && (seg.to.x - seg.from.x).abs() > 0.5 {
                    self.left = seg.to.x < seg.from.x;
                }
                self.cur = Some((seg, 0.));
            }
            let Some((seg, elapsed)) = self.cur.as_mut() else { break };
            *elapsed += dt;
            if *elapsed >= seg.dur {
                dt = *elapsed - seg.dur;
                self.pos = seg.to;
                self.cur = None;
                continue;
            }
            self.pos = seg.at(*elapsed);
            break;
        }
        let (look, moving, wake, age) = match &self.cur {
            Some((s, e)) => (s.look, s.moving, (!s.moving).then(|| s.dur - e), *e),
            None => (0, false, Some(0.), 0.),
        };
        Pose { at: self.pos, look, moving, mirrored: self.left != self.def.faces_left, wake, age }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(json: &str) -> BTreeMap<String, ActorSpec> {
        serde_json::from_str(json).unwrap()
    }

    /// An actor over a folder where `files` exist.
    fn compiled(json: &str, files: &[&str]) -> (BTreeMap<String, Arc<ActorDef>>, Vec<String>) {
        let dir = std::env::temp_dir().join(format!("tern-actor-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for f in files {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        let mut problems = vec![];
        let out = compile(&spec(json), &dir, &mut problems);
        (out, problems)
    }

    const RABBIT: &str = r#"{"rabbit": {
        "size": [40, 40],
        "looks": {"sit": {"image": "sit.png"}, "run": {"sprite": "run.png", "frames": 6, "fps": 12}},
        "faces": "right",
        "moves": [
            {"do": "rest", "look": "sit", "at": [0.1, 1], "for": [20, 60]},
            {"do": "wander", "look": "run", "rest_look": "sit", "speed": 120, "pause": [2, 6], "times": [2, 5]},
            {"do": "visit", "look": "run", "path": "across", "speed": 220},
            {"do": "patrol", "look": "run", "from": [0, 1], "to": [1, 1], "speed": 90, "times": 2},
            {"do": "hop", "look": "run", "to": [0.5, 1], "height": 20}],
        "order": "random",
        "click": {"do": "flee", "look": "run", "speed": 400, "then": "rest"}}}"#;

    fn rabbit() -> Arc<ActorDef> {
        let (out, problems) = compiled(RABBIT, &["sit.png", "run.png"]);
        assert_eq!(problems, Vec::<String>::new());
        out["rabbit"].clone()
    }

    #[test]
    fn an_actor_is_read() {
        let a = rabbit();
        assert_eq!(a.size, (40., 40.));
        assert_eq!(a.looks.len(), 2);
        assert!(a.random && !a.faces_left);
        assert_eq!(a.moves.len(), 5);
        assert!(matches!(a.moves[0].kind, Kind::Rest { at: Some(Point(Coord::Frac(f), Coord::Frac(g))), .. } if f == 0.1 && g == 1.));
        assert!(matches!(&a.click, Some(Click { then: Then::Rest, mv: Move { kind: Kind::Flee, speed, .. } }) if *speed == 400.));
    }

    #[test]
    fn mistakes_are_reported_and_the_rest_is_kept() {
        let (out, problems) = compiled(
            r#"{"a": {"looks": {"sit": {"image": "gone.png"}, "x": {}, "run": {"sprite": "run.png", "frames": 0}},
                "moves": [{"do": "rest", "look": "nap", "at": [0.5, "far"]}, {"do": "dance"},
                          {"do": "hop"}, {"do": "wander", "speed": -1, "pause": [1]}],
                "faces": "up", "order": "shuffle", "size": [0, 4],
                "click": {"do": "rest", "then": "later"}},
               "b": {"looks": {}}}"#,
            &["run.png"],
        );
        for want in [
            "actors.a.looks.sit.image: no file",
            "actors.a.looks.x: a look is an `image` or a `sprite`",
            "actors.a.looks.run.frames: at least 1",
            "actors.a.moves.0.look: no look \"nap\" (there are: run, sit)",
            "actors.a.moves.0.at: \"far\" is not a number",
            "actors.a.moves.1.do: \"dance\" is not one of rest, wander, patrol, visit, hop",
            "actors.a.moves.2: a hop needs a `to`",
            "actors.a.moves.3.speed: pixels per second",
            "actors.a.moves.3.pause: a number from 0 or [min, max]",
            "actors.a.faces: \"up\" is not left or right",
            "actors.a.order: \"shuffle\" is not list or random",
            "actors.a.size: [width, height]",
            "actors.a.click.then: \"later\" is not rest or continue",
            "actors.b.looks: an actor needs at least one look",
        ] {
            assert!(problems.iter().any(|p| p.contains(want)), "{want}\n{problems:#?}");
        }
        // The actor with a look stays; the one with none is gone.
        assert!(out.contains_key("a") && !out.contains_key("b"));
        // A move that couldn't be read is left out; the others stay.
        assert_eq!(out["a"].moves.len(), 2);
    }

    #[test]
    fn points_are_fractions_or_pixels() {
        let mut problems = vec![];
        let mut r = Reader { name: "a", problems: &mut problems };
        let p = r.point(&serde_json::json!([0.5, 30]), "p").unwrap();
        assert_eq!(p.at(Pt { x: 200., y: 100. }), Pt { x: 100., y: 30. });
        let p = r.point(&serde_json::json!(["25%", "40px"]), "p").unwrap();
        assert_eq!(p.at(Pt { x: 200., y: 100. }), Pt { x: 50., y: 40. });
        assert!(r.point(&serde_json::json!([1]), "p").is_none());
        assert!(r.point(&serde_json::json!([true, 1]), "p").is_none());
    }

    #[test]
    fn a_segment_runs_straight_and_a_hop_arcs() {
        let run = Seg { dur: 2., from: Pt { x: 0., y: 100. }, to: Pt { x: 100., y: 100. }, arc: 0., look: 0, moving: true, jump: false };
        assert_eq!(run.at(0.), Pt { x: 0., y: 100. });
        assert_eq!(run.at(1.), Pt { x: 50., y: 100. });
        assert_eq!(run.at(9.), Pt { x: 100., y: 100. });
        let hop = Seg { arc: 20., ..run.clone() };
        assert_eq!(hop.at(1.), Pt { x: 50., y: 80. });
        assert_eq!(hop.at(2.), Pt { x: 100., y: 100. });
        let jump = Seg { jump: true, ..run };
        assert_eq!(jump.at(0.), Pt { x: 100., y: 100. });
    }

    const STAGE: Pt = Pt { x: 400., y: 100. };

    fn only(json_moves: &str) -> Arc<ActorDef> {
        let json = format!(
            r#"{{"a": {{"size": [40, 40], "looks": {{"a": {{"image": "a.png"}}, "b": {{"image": "a.png"}}}}, "moves": {json_moves},
                 "click": {{"do": "flee", "look": "b", "speed": 400, "then": "rest"}}}}}}"#
        );
        let (out, problems) = compiled(&json, &["a.png"]);
        assert_eq!(problems, Vec::<String>::new());
        out["a"].clone()
    }

    #[test]
    fn it_starts_at_its_place_and_rests_there() {
        let def = only(r#"[{"do": "rest", "at": [0.25, 1], "for": 5}]"#);
        let mut run = Run::new(def, STAGE, 0., 1);
        let p = run.advance(0.);
        assert_eq!(p.at, Pt { x: 100., y: 100. });
        assert!(!p.moving);
        assert_eq!(p.wake, Some(5.));
        let p = run.advance(2.);
        assert_eq!(p.wake, Some(3.));
    }

    #[test]
    fn a_patrol_goes_and_comes_back_and_faces_its_way() {
        let def = only(
            r#"[{"do": "rest", "at": [0, 1], "for": 1}, {"do": "patrol", "from": [0.1, 1], "to": [0.9, 1], "speed": 100, "times": 1}]"#,
        );
        let mut run = Run::new(def, STAGE, 0., 1);
        // The place is x = 0 clamped to the figure's half width: 20.
        run.advance(0.);
        // After the rest: it runs to the left end (x 40), then right: 360, then back.
        let p = run.advance(1.0);
        assert!(p.moving || p.wake.is_some());
        let p = run.advance(1.0 + 0.2 + 1.0);
        assert!(p.moving);
        assert!((p.at.x - 140.).abs() < 1.0, "{p:?}");
        assert!(!p.mirrored, "running right, facing right");
        // Back: past the far end at 1.2 + 3.2 s.
        run.advance(3.5);
        let p = run.advance(1.2 + 3.2 + 1.0);
        assert!(p.moving);
        assert!(p.mirrored, "running left against the way it faces");
        assert!((p.at.x - 260.).abs() < 1.0, "{p:?}");
    }

    #[test]
    fn faces_left_flips_the_mirroring() {
        let (out, _) = compiled(
            r#"{"a": {"size": [40, 40], "looks": {"a": {"image": "a.png"}}, "faces": "left",
                "moves": [{"do": "patrol", "from": [0.1, 1], "to": [0.9, 1], "speed": 100}]}}"#,
            &["a.png"],
        );
        let mut run = Run::new(out["a"].clone(), STAGE, 0., 1);
        run.advance(0.);
        // First it runs left to the start of the patrol, then right.
        let p = run.advance(2.5);
        assert!(p.moving);
        assert!(p.mirrored, "moving right, drawn facing left");
    }

    #[test]
    fn a_hop_arcs_up_to_its_target() {
        let def = only(r#"[{"do": "hop", "to": [0.5, 1], "height": 30, "speed": 100}]"#);
        let mut run = Run::new(def, STAGE, 0., 1);
        let start = run.advance(0.).at;
        assert_eq!(start, Pt { x: 200., y: 100. });
        // Home is the middle bottom, the hop target the same: it still takes its minimum time and arcs.
        let mid = run.advance(0.125).at;
        assert!(mid.y < 100., "{mid:?}");
        let end = run.advance(0.3).at;
        assert!((end.y - 100.).abs() < 20., "{end:?}");
    }

    #[test]
    fn a_visit_leaves_one_side_and_returns_from_the_other() {
        let def = only(r#"[{"do": "rest", "at": [0.5, 1], "for": 1}, {"do": "visit", "speed": 200}]"#);
        let mut run = Run::new(def, STAGE, 0., 7);
        run.advance(0.);
        let (mut min, mut max, mut jumped) = (f32::MAX, f32::MIN, false);
        let mut last = 200.;
        let mut t = 0.;
        while t < 4. {
            t += 0.05;
            let p = run.advance(t);
            min = min.min(p.at.x);
            max = max.max(p.at.x);
            // The jump across the stage.
            jumped |= (p.at.x - last).abs() > 300.;
            last = p.at.x;
        }
        assert!(min <= -40. || max >= 440., "it leaves the stage ({min}, {max})");
        assert!(jumped, "it comes in from the other side");
        assert!((last - 200.).abs() < 20., "and ends at its place: {last}");
    }

    #[test]
    fn wander_stays_in_the_stage_and_pauses() {
        let def = only(r#"[{"do": "wander", "look": "b", "speed": 300, "pause": [1, 1], "times": 3}]"#);
        let mut run = Run::new(def, STAGE, 0., 3);
        let mut paused = false;
        let mut t = 0.;
        while t < 20. {
            t += 0.05;
            let p = run.advance(t);
            assert!(p.at.x >= 20. - 0.01 && p.at.x <= 380. + 0.01 && p.at.y >= 40. - 0.01 && p.at.y <= 100. + 0.01, "{p:?}");
            paused |= !p.moving && p.wake.is_some_and(|w| w > 0.2);
        }
        assert!(paused);
    }

    #[test]
    fn random_order_picks_each_move_without_repeating_in_a_row() {
        let def = only(r#"[{"do": "rest", "for": 0.5, "at": [0.5, 1]}, {"do": "hop", "to": [0.5, 1], "height": 5}]"#);
        let mut run = Run::new(def, STAGE, 0., 11);
        let mut seen = std::collections::HashSet::new();
        let mut t = 0.;
        while t < 30. {
            t += 0.05;
            run.advance(t);
            seen.insert(run.next);
        }
        assert_eq!(seen.len(), 2);
    }

    #[test]
    fn a_click_flees_away_from_the_pointer_then_goes_home() {
        let def = only(r#"[{"do": "rest", "at": [0.5, 1], "for": 100}]"#);
        let mut run = Run::new(def, STAGE, 0., 5);
        run.advance(0.);
        // The pointer is left of the figure: it runs right.
        run.click(-10.);
        let p = run.advance(0.2);
        assert!(p.moving && p.at.x > 200., "{p:?}");
        assert!(!p.mirrored);
        // Later it is back at its place and rests.
        let p = run.advance(1.5);
        let p = if p.moving { run.advance(2.8) } else { p };
        assert!(!p.moving, "{p:?}");
        assert!((p.at.x - 200.).abs() < 1. && (p.at.y - 100.).abs() < 1., "{p:?}");
        // A click from the other side runs the other way, and a click while it reacts starts over.
        run.click(10.);
        let p = run.advance(1.6);
        assert!(p.at.x < 200., "{p:?}");
        run.click(10.);
        assert!(run.advance(1.7).moving);
    }

    #[test]
    fn a_click_without_room_flees_the_other_way() {
        let def = only(r#"[{"do": "rest", "at": [1, 1], "for": 100}]"#);
        let mut run = Run::new(def, STAGE, 0., 5);
        run.advance(0.);
        // At the right wall, with the pointer on its left: it can't go right.
        run.click(-5.);
        let p = run.advance(0.1);
        assert!(p.at.x < 380., "{p:?}");
    }

    #[test]
    fn a_long_gap_skips_ahead() {
        let def = only(r#"[{"do": "wander", "look": "b", "speed": 50, "pause": [30, 30], "times": 4}]"#);
        let mut run = Run::new(def, STAGE, 0., 3);
        run.advance(0.);
        let before = run.next;
        // A sleep: no replaying of the minutes missed.
        let p = run.advance(3600.);
        assert!(p.at.x.is_finite());
        // It went on to the next move and is at its start.
        assert!(run.next != before || run.cur.is_some());
        let p2 = run.advance(3600.1);
        assert!((p2.at.x - p.at.x).abs() < 20., "{p:?} {p2:?}");
    }

    #[test]
    fn a_resized_stage_keeps_the_place() {
        let def = only(r#"[{"do": "rest", "at": [0.5, 1], "for": 100}]"#);
        let mut run = Run::new(def, STAGE, 0., 5);
        run.advance(0.);
        run.resize(Pt { x: 800., y: 200. });
        let p = run.advance(0.1);
        assert_eq!(p.at, Pt { x: 400., y: 200. });
        assert_eq!(run.stage(), Pt { x: 800., y: 200. });
    }

    #[test]
    fn an_actor_without_moves_just_stands() {
        let def = only("[]");
        let mut run = Run::new(def, STAGE, 0., 5);
        let p = run.advance(1.);
        assert!(!p.moving);
        assert_eq!(p.at, Pt { x: 200., y: 100. });
    }
}

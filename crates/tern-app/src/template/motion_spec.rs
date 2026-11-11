//! Reading what a template says about motion: `animation`, `transition`, `on`
//! and the `animations` they name (see `motion`).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use serde_json::Value as Json;

use super::motion::{Anim, Chan, Direction, Easing, Fill, Keyframes, NodeMotion, RawAnimations, TRANSFORMS, Trans};
use super::{Compiler, Spec};
use crate::style::{Props, Val};

/// The default length of an animation, in seconds.
const ANIMATION_SECONDS: f64 = 1.;
/// The default length of a transition, in seconds.
const TRANSITION_SECONDS: f64 = 0.2;

/// Reads the keyframe sets of `animations`.
pub fn compile_animations(raw: &RawAnimations, props: &Props, problems: &mut Vec<String>) -> HashMap<String, Arc<Keyframes>> {
    let mut out = HashMap::new();
    for (name, stops) in raw {
        let mut list = vec![];
        for (key, values) in stops {
            let at = format!("animations.{name}.{key}");
            // A key may name several offsets: "20%, 60%".
            let offsets: Vec<f32> = key
                .split(',')
                .filter_map(|k| Keyframes::offset(k).map_err(|e| problems.push(format!("animations.{name}: {e}"))).ok())
                .collect();
            let mut plain = BTreeMap::new();
            let mut chans = HashMap::new();
            for (prop, v) in values {
                match TRANSFORMS.iter().find(|t| t.0 == prop) {
                    Some(&(_, chan, _)) => match v.as_f64() {
                        Some(n) => _ = chans.insert(chan, Val::Num(n as f32)),
                        None => problems.push(format!("{at}.{prop}: a number")),
                    },
                    None => _ = plain.insert(prop.clone(), v.clone()),
                }
            }
            for (prop, v) in props.compile(None, &plain, &at, problems) {
                chans.insert(Chan::Prop(prop), v);
            }
            list.extend(offsets.into_iter().map(|o| (o, chans.clone())));
        }
        if list.is_empty() {
            problems.push(format!("animations.{name}: no keyframes"));
        }
        out.insert(name.clone(), Arc::new(Keyframes::new(list)));
    }
    out
}

/// `text` cut at spaces (or `sep`) outside parentheses.
fn split_outside(text: &str, sep: impl Fn(char) -> bool) -> Vec<&str> {
    let (mut out, mut depth, mut from) = (vec![], 0i32, 0);
    for (i, c) in text.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            c if depth == 0 && sep(c) => {
                if i > from {
                    out.push(&text[from..i]);
                }
                from = i + c.len_utf8();
            },
            _ => {},
        }
    }
    if from < text.len() {
        out.push(&text[from..]);
    }
    out
}

/// `1.2s` or `200ms` as seconds.
fn seconds(t: &str) -> Option<f64> {
    let (n, scale) = match t.strip_suffix("ms") {
        Some(n) => (n, 0.001),
        None => (t.strip_suffix('s')?, 1.),
    };
    n.parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.).map(|v| v * scale)
}

fn is_easing(t: &str) -> bool {
    matches!(t, "linear" | "ease" | "ease-in" | "ease-out" | "ease-in-out") || t.starts_with("steps(") || t.starts_with("cubic-bezier(")
}

/// What an `animation` object or shorthand says, before its name is looked up.
struct Timing {
    name: Option<String>,
    duration: Option<f64>,
    delay: f64,
    iterations: f64,
    direction: Direction,
    easing: Easing,
    fill: Fill,
    play: Option<String>,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            name: None,
            duration: None,
            delay: 0.,
            iterations: 1.,
            direction: Direction::Normal,
            easing: Easing::EASE,
            fill: Fill::None,
            play: None,
        }
    }
}

fn direction(t: &str) -> Option<Direction> {
    Some(match t {
        "normal" => Direction::Normal,
        "reverse" => Direction::Reverse,
        "alternate" => Direction::Alternate,
        "alternate-reverse" => Direction::AlternateReverse,
        _ => return None,
    })
}

fn fill(t: &str) -> Option<Fill> {
    Some(match t {
        "none" => Fill::None,
        "forwards" => Fill::Forwards,
        "backwards" => Fill::Backwards,
        "both" => Fill::Both,
        _ => return None,
    })
}

impl Compiler<'_, '_> {
    /// `"pulse 1.2s infinite ease-in-out"`.
    fn timing_text(&mut self, text: &str, at: &str) -> Timing {
        let mut t = Timing::default();
        let mut times = 0;
        for word in split_outside(text, char::is_whitespace) {
            if let Some(s) = seconds(word) {
                if times == 0 {
                    t.duration = Some(s);
                } else {
                    t.delay = s;
                }
                times += 1;
            } else if word == "infinite" {
                t.iterations = f64::INFINITY;
            } else if let Some(n) = word.parse::<f64>().ok().filter(|n| *n >= 0. && n.is_finite()) {
                t.iterations = n;
            } else if let Some(d) = direction(word) {
                t.direction = d;
            } else if let Some(f) = fill(word) {
                t.fill = f;
            } else if is_easing(word) {
                match Easing::parse(word) {
                    Ok(e) => t.easing = e,
                    Err(e) => self.problems.push(format!("{at}: {e}")),
                }
            } else if t.name.is_none() {
                t.name = Some(word.to_owned());
            } else {
                self.problems.push(format!("{at}: \"{word}\" is not part of an animation"));
            }
        }
        t
    }

    fn seconds_field(&mut self, o: &serde_json::Map<String, Json>, key: &str, at: &str) -> Option<f64> {
        let v = o.get(key)?;
        match v.as_f64().filter(|n| *n >= 0.) {
            Some(n) => Some(n),
            None => {
                self.problems.push(format!("{at}.{key}: seconds, a number from 0"));
                None
            },
        }
    }

    fn timing_object(&mut self, o: &serde_json::Map<String, Json>, at: &str) -> Timing {
        let mut t = Timing::default();
        for key in o.keys() {
            if !["name", "duration", "delay", "iteration", "direction", "easing", "fill", "play"].contains(&key.as_str()) {
                self.problems
                    .push(format!("{at}.{key}: an animation takes name, duration, delay, iteration, direction, easing, fill and play"));
            }
        }
        t.name = o.get("name").and_then(Json::as_str).map(str::to_owned);
        t.duration = self.seconds_field(o, "duration", at);
        t.delay = self.seconds_field(o, "delay", at).unwrap_or(0.);
        match o.get("iteration") {
            None => {},
            Some(Json::String(s)) if s == "infinite" => t.iterations = f64::INFINITY,
            Some(v) => match v.as_f64().filter(|n| *n >= 0.) {
                Some(n) => t.iterations = n,
                None => self.problems.push(format!("{at}.iteration: a count or \"infinite\"")),
            },
        }
        if let Some(d) = o.get("direction") {
            match d.as_str().and_then(direction) {
                Some(d) => t.direction = d,
                None => self.problems.push(format!("{at}.direction: one of normal, reverse, alternate, alternate-reverse")),
            }
        }
        if let Some(e) = o.get("easing") {
            match e.as_str().map(Easing::parse) {
                Some(Ok(e)) => t.easing = e,
                Some(Err(e)) => self.problems.push(format!("{at}.easing: {e}")),
                None => self.problems.push(format!("{at}.easing: text")),
            }
        }
        if let Some(f) = o.get("fill") {
            match f.as_str().and_then(fill) {
                Some(f) => t.fill = f,
                None => self.problems.push(format!("{at}.fill: one of none, forwards, backwards, both")),
            }
        }
        t.play = o.get("play").and_then(Json::as_str).map(str::to_owned);
        t
    }

    /// The animations `v` (a shorthand, an object, or a list of them) runs.
    pub(super) fn animations(&mut self, v: &Json, at: &str) -> Vec<Anim> {
        let timings: Vec<Timing> = match v {
            Json::String(s) => vec![self.timing_text(s, at)],
            Json::Object(o) => vec![self.timing_object(o, at)],
            Json::Array(list) => list
                .iter()
                .enumerate()
                .filter_map(|(i, e)| match e {
                    Json::String(s) => Some(self.timing_text(s, &format!("{at}.{i}"))),
                    Json::Object(o) => Some(self.timing_object(o, &format!("{at}.{i}"))),
                    _ => {
                        self.problems.push(format!("{at}.{i}: a shorthand or an object"));
                        None
                    },
                })
                .collect(),
            _ => {
                self.problems.push(format!("{at}: a shorthand, an object or a list"));
                vec![]
            },
        };
        let mut out = vec![];
        for t in timings {
            let Some(name) = t.name else {
                self.problems.push(format!("{at}: an animation needs a name"));
                continue;
            };
            let Some(kf) = self.animations.get(&name).cloned() else {
                let mut known: Vec<&str> = self.animations.keys().map(String::as_str).collect();
                known.sort_unstable();
                self.problems.push(format!("{at}: no animation \"{name}\" (there are: {})", known.join(", ")));
                continue;
            };
            let play = t.play.as_deref().and_then(|p| self.cond(p, &format!("{at}.play")));
            out.push(Anim {
                kf,
                duration: t.duration.unwrap_or(ANIMATION_SECONDS),
                delay: t.delay,
                iterations: t.iterations,
                direction: t.direction,
                easing: t.easing,
                fill: t.fill,
                play,
            });
        }
        out
    }

    /// A property name a transition takes: `all` is every one.
    fn transition_prop(&mut self, name: &str, at: &str) -> Option<Option<&'static str>> {
        if name == "all" {
            return Some(None);
        }
        match crate::style::box_prop(name) {
            Some(p) => Some(Some(p.0)),
            None => {
                self.problems.push(format!("{at}: no property \"{name}\""));
                None
            },
        }
    }

    fn transition_text(&mut self, text: &str, at: &str) -> Vec<Trans> {
        let mut out = vec![];
        for item in split_outside(text, |c| c == ',') {
            let words = split_outside(item, char::is_whitespace);
            let Some(prop) = words.first().and_then(|p| self.transition_prop(p, at)) else { continue };
            let (mut duration, mut delay, mut easing, mut times) = (None, 0., Easing::EASE, 0);
            for w in &words[1..] {
                if let Some(s) = seconds(w) {
                    if times == 0 {
                        duration = Some(s);
                    } else {
                        delay = s;
                    }
                    times += 1;
                } else {
                    match Easing::parse(w) {
                        Ok(e) => easing = e,
                        Err(e) => self.problems.push(format!("{at}: {e}")),
                    }
                }
            }
            out.push(Trans { prop, duration: duration.unwrap_or(TRANSITION_SECONDS), delay, easing });
        }
        out
    }

    fn transition_object(&mut self, o: &serde_json::Map<String, Json>, at: &str) -> Option<Trans> {
        for key in o.keys() {
            if !["property", "duration", "delay", "easing"].contains(&key.as_str()) {
                self.problems.push(format!("{at}.{key}: a transition takes property, duration, delay and easing"));
            }
        }
        let prop = self.transition_prop(o.get("property").and_then(Json::as_str).unwrap_or("all"), &format!("{at}.property"))?;
        let easing = match o.get("easing").and_then(Json::as_str).map(Easing::parse) {
            Some(Ok(e)) => e,
            Some(Err(e)) => {
                self.problems.push(format!("{at}.easing: {e}"));
                Easing::EASE
            },
            None => Easing::EASE,
        };
        Some(Trans {
            prop,
            duration: self.seconds_field(o, "duration", at).unwrap_or(TRANSITION_SECONDS),
            delay: self.seconds_field(o, "delay", at).unwrap_or(0.),
            easing,
        })
    }

    /// The transitions `v` (a string, an object, or a list of them) says.
    fn transitions(&mut self, v: &Json, at: &str) -> Vec<Trans> {
        match v {
            Json::String(s) => self.transition_text(s, at),
            Json::Object(o) => self.transition_object(o, at).into_iter().collect(),
            Json::Array(list) => list
                .iter()
                .enumerate()
                .flat_map(|(i, e)| match e {
                    Json::String(s) => self.transition_text(s, &format!("{at}.{i}")),
                    Json::Object(o) => self.transition_object(o, &format!("{at}.{i}")).into_iter().collect(),
                    _ => {
                        self.problems.push(format!("{at}.{i}: a string or an object"));
                        vec![]
                    },
                })
                .collect(),
            _ => {
                self.problems.push(format!("{at}: a string, an object or a list"));
                vec![]
            },
        }
    }

    /// What `s` says about motion; None when it says none.
    pub(super) fn motion(&mut self, s: &Spec, at: &str) -> Option<Arc<NodeMotion>> {
        let mut m = NodeMotion::default();
        if let Some(a) = &s.animation {
            m.anims = self.animations(a, &format!("{at}.animation"));
        }
        if let Some(t) = &s.transition {
            m.trans = self.transitions(t, &format!("{at}.transition"));
        }
        for (event, v) in &s.on {
            let here = format!("{at}.on.{event}");
            let list = self.animations(v, &here).into_iter().map(Arc::new).collect();
            match event.as_str() {
                "appear" => m.appear = list,
                "click" => m.click = list,
                "hover" => m.hover = list,
                _ => self.problems.push(format!("{here}: an event is one of appear, click, hover")),
            }
        }
        (!m.is_empty()).then(|| Arc::new(m))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shorthand_words_are_told_apart() {
        assert_eq!(split_outside("a cubic-bezier(0.1, 0.2, 0.3, 1) b", char::is_whitespace), ["a", "cubic-bezier(0.1, 0.2, 0.3, 1)", "b"]);
        assert_eq!(seconds("1.5s"), Some(1.5));
        assert_eq!(seconds("200ms"), Some(0.2));
        assert_eq!(seconds("fast"), None);
        assert!(is_easing("steps(3)") && !is_easing("pulse"));
    }
}

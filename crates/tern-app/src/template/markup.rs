//! Templates in HTML and CSS: reads the files a theme or `settings.json` names
//! (`html`, `css`) into the same JSON-shaped templates `templates` holds, so
//! the one compiler in `mod.rs` checks everything. What the markup says that
//! the template engine has no place for is a problem with its file, line and
//! column; nothing is dropped silently.
//!
//! The mapping is in the config README (`settings.rs`); `unmarkup` writes it
//! the other way, for the built-in templates in `reference/`.

use std::collections::BTreeMap;

use serde_json::{Map, Value as Json};

use super::css::{self, Decl, Dom, Media, Tok};
use super::html::{self, Element, Item, Pos};
use super::{RawAnimations, Spec};
use crate::style::{self, Kind};

/// A file's name (for problems) and what it holds.
pub struct Source {
    pub file: String,
    pub text: String,
}

/// What the markup files make.
#[derive(Default)]
pub struct Markup {
    pub templates: BTreeMap<String, Spec>,
    /// The same templates as JSON, as the compiler reads them.
    pub json: BTreeMap<String, Json>,
    pub animations: RawAnimations,
    /// `:root`'s `--name: value`, as `tokens` would hold them.
    pub tokens: BTreeMap<String, Json>,
    /// Where each node (`templates.name.children.0`) and keyframe set came from.
    origins: Vec<(String, String, Pos)>,
}

impl Markup {
    /// `problem`, as the template compiler wrote it (`templates.x.children.0.style: …`),
    /// with the file, line and column of the markup it is about.
    pub fn locate(&self, problem: &str) -> String {
        let best = self
            .origins
            .iter()
            .filter(|(key, ..)| problem.strip_prefix(key.as_str()).is_some_and(|rest| rest.is_empty() || rest.starts_with(['.', ':'])))
            .max_by_key(|(key, ..)| key.len());
        match best {
            Some((key, file, pos)) => {
                let rest = problem[key.len()..].trim_start_matches('.').trim_start_matches(':').trim_start();
                format!("{file}:{}:{}: {rest}", pos.line, pos.col)
            },
            None => problem.to_owned(),
        }
    }
}

/// The CSS name of a style property, and back.
const CSS_NAMES: &[(&str, &str)] = &[
    ("radius", "border-radius"),
    ("radius_top_left", "border-top-left-radius"),
    ("radius_top_right", "border-top-right-radius"),
    ("radius_bottom_right", "border-bottom-right-radius"),
    ("radius_bottom_left", "border-bottom-left-radius"),
    ("border", "border-color"),
    ("background", "background-color"),
    ("text", "color"),
    ("italic", "font-style"),
    ("line_clamp", "-webkit-line-clamp"),
    ("shadow", "box-shadow"),
    ("background_image", "background-image"),
    ("background_image_fit", "background-size"),
    ("padding_x", "padding-inline"),
    ("padding_y", "padding-block"),
    ("margin_x", "margin-inline"),
    ("margin_y", "margin-block"),
];

/// Style properties CSS spells like Tern does (underscores to hyphens).
const SAME_NAMES: &[&str] = &[
    "width",
    "height",
    "min_width",
    "min_height",
    "max_width",
    "max_height",
    "padding",
    "padding_top",
    "padding_right",
    "padding_bottom",
    "padding_left",
    "margin",
    "margin_top",
    "margin_right",
    "margin_bottom",
    "margin_left",
    "gap",
    "border_width",
    "border_top_width",
    "border_right_width",
    "border_bottom_width",
    "border_left_width",
    "font_family",
    "font_size",
    "font_weight",
    "line_height",
    "opacity",
    "text_transform",
];

/// The style property a CSS property sets.
pub fn style_key(css_name: &str) -> Option<String> {
    if let Some(rest) = css_name.strip_prefix("-tern-") {
        let key = rest.replace('-', "_");
        return (style::box_prop(&key).is_some() || key == "element").then_some(key);
    }
    if let Some((key, _)) = CSS_NAMES.iter().find(|(_, c)| *c == css_name) {
        return Some((*key).to_owned());
    }
    let key = css_name.replace('-', "_");
    SAME_NAMES.contains(&key.as_str()).then_some(key)
}

/// The CSS property that writes style property `key`.
pub fn css_name(key: &str) -> String {
    if let Some((_, c)) = CSS_NAMES.iter().find(|(k, _)| *k == key) {
        return (*c).to_owned();
    }
    if SAME_NAMES.contains(&key) {
        return key.replace('_', "-");
    }
    format!("-tern-{}", key.replace('_', "-"))
}

/// `$agents.tab.gap` as a CSS name: `.` is `--`, `_` is `-`.
pub fn token_ident(path: &str) -> String {
    format!("--{}", path.replace('_', "-").replace('.', "--"))
}

/// The other way: `--agents--tab--gap` is `agents.tab.gap`.
fn token_path(ident: &str) -> String {
    ident.trim_start_matches("--").replace("--", ".").replace('-', "_")
}

/// Whether `path` survives being written as a CSS name and read back.
pub fn token_path_is_lossless(path: &str) -> bool {
    token_path(&token_ident(path)) == path
}

pub fn jnum(n: f64) -> Json {
    if n.fract() == 0. && n.abs() < 1e15 { Json::from(n as i64) } else { Json::from(n) }
}

fn color_hex(args: &[Tok]) -> Result<Json, String> {
    let nums: Vec<f64> = args
        .iter()
        .filter_map(|t| match t {
            Tok::Num(n) => Some(*n),
            Tok::Pct(p) => Some(p * 2.55),
            _ => None,
        })
        .collect();
    if nums.len() < 3 {
        return Err("rgb() takes red, green, blue and optionally an alpha".into());
    }
    let ch = |n: f64| n.round().clamp(0., 255.) as u8;
    let mut out = format!("#{:02x}{:02x}{:02x}", ch(nums[0]), ch(nums[1]), ch(nums[2]));
    if let Some(a) = nums.get(3) {
        // A percentage alpha was scaled by 2.55; a number is 0 to 1.
        let alpha = match args.iter().rev().find(|t| matches!(t, Tok::Num(_) | Tok::Pct(_))) {
            Some(Tok::Pct(p)) => p / 100.,
            _ => *a,
        };
        out.push_str(&format!("{:02x}", ch(alpha * 255.)));
    }
    Ok(out.into())
}

/// A token written the way a JSON value would be, without knowing what property it is for.
fn generic(toks: &[Tok]) -> Result<Json, String> {
    match toks {
        [Tok::Num(n)] => Ok(jnum(*n)),
        [Tok::Dim(n, u)] if u == "px" => Ok(jnum(*n)),
        [Tok::Dim(n, u)] if u == "rem" => Ok(format!("{}rem", num_text(*n)).into()),
        [Tok::Dim(_, u)] => Err(format!("unit \"{u}\" is not supported (px, rem)")),
        [Tok::Pct(n)] => Ok(format!("{}%", num_text(*n)).into()),
        [Tok::Hash(h)] => Ok(format!("#{h}").into()),
        [Tok::Ident(i)] => Ok(i.clone().into()),
        [Tok::Str(s)] => Ok(s.clone().into()),
        [Tok::Hole(n)] => Ok(format!("{{{n}}}").into()),
        [Tok::Func(f, args)] if f == "var" => match &args[..] {
            [Tok::Ident(n)] if n.starts_with("--") => Ok(format!("${}", token_path(n)).into()),
            _ => Err("var(--name) takes one name (a default is not supported)".into()),
        },
        [Tok::Func(f, args)] if f == "rgb" || f == "rgba" => color_hex(args),
        [Tok::Ident(a), Tok::Slash, Tok::Num(n) | Tok::Pct(n)] => Ok(format!("{a}/{}", num_text(*n)).into()),
        [] => Err("no value".into()),
        _ => Err("one value is expected".into()),
    }
}

fn num_text(n: f64) -> String {
    jnum(n).to_string()
}

/// A value for property `key` of kind `kind`.
fn value(key: &str, kind: Kind, toks: &[Tok]) -> Result<Json, String> {
    match (kind, toks) {
        (Kind::Len, [Tok::Num(n)]) if *n == 0. => Ok(0.into()),
        (Kind::Len, [Tok::Num(_)]) => Err("a length needs a unit (px or rem)".into()),
        (Kind::Len, [Tok::Pct(_)]) => Err("% is not supported here (only width and height 100% together, as fill)".into()),
        (Kind::Len | Kind::Color | Kind::Text | Kind::Num, [Tok::Func(f, args)]) if f == "url" && key.starts_with("background_image") => {
            match &args[..] {
                [Tok::Str(s)] => Ok(s.clone().into()),
                _ => Err("url() takes a path".into()),
            }
        },
        (Kind::Num, [Tok::Num(n)]) => Ok(jnum(*n)),
        (Kind::Num, [Tok::Pct(p)]) if key == "opacity" || key == "background_image_opacity" => Ok(jnum(p / 100.)),
        (Kind::Num, [Tok::Ident(i)]) if key == "font_weight" => match i.as_str() {
            "normal" => Ok(400.into()),
            "bold" => Ok(700.into()),
            _ => Err(format!("font-weight is a number from 100 to 900, normal or bold, not {i}")),
        },
        (Kind::Num, [Tok::Ident(i)]) if key == "italic" => match i.as_str() {
            "italic" | "oblique" => Ok(1.into()),
            "normal" => Ok(0.into()),
            _ => Err("font-style is normal or italic".into()),
        },
        (Kind::Num, [Tok::Dim(..)]) => Err("a number without a unit is expected".into()),
        (Kind::Text, [Tok::Ident(i)]) if key == "background_image_fit" => Ok(match i.as_str() {
            "scale-down" => "scale_down".into(),
            other => other.to_owned().into(),
        }),
        (Kind::Text, [Tok::Ident(i)]) => Ok(i.clone().into()),
        (Kind::Text, [Tok::Str(s)]) => Ok(s.clone().into()),
        (Kind::Text, toks) if key == "font_family" => {
            if toks.len() > 1 {
                return Err("one font family only (Tern has no fallback list); quote a name with spaces".into());
            }
            generic(toks)
        },
        (Kind::Text, [t]) if key == "shadow" => Err(format!("{t:?} is not a shadow size (none, sm, md, lg, xl)")),
        (_, toks) => generic(toks),
    }
}

/// A shadow: one of Tern's sizes, or the blur of a CSS shadow mapped to one.
fn shadow(toks: &[Tok]) -> Result<Json, String> {
    if let [Tok::Ident(i)] = toks
        && ["none", "sm", "md", "lg", "xl"].contains(&i.as_str())
    {
        return Ok(i.clone().into());
    }
    let lens: Vec<f64> = toks
        .iter()
        .map_while(|t| match t {
            Tok::Dim(n, u) if u == "px" => Some(*n),
            Tok::Num(n) => Some(*n),
            _ => None,
        })
        .collect();
    if lens.len() < 2 {
        return Err("a shadow is none, sm, md, lg, xl, or offsets and a blur in px".into());
    }
    let blur = lens.get(2).copied().unwrap_or(0.);
    Ok(match blur {
        b if b <= 0. => "none",
        b if b <= 3. => "sm",
        b if b <= 8. => "md",
        b if b <= 16. => "lg",
        _ => "xl",
    }
    .into())
}

/// A shorthand's one to four lengths.
fn lens(key: &str, toks: &[Tok]) -> Result<Vec<Json>, String> {
    let list: Vec<&[Tok]> = toks.chunks(1).collect();
    if list.is_empty() || list.len() > 4 {
        return Err("one to four lengths".into());
    }
    list.iter().map(|t| value(key, Kind::Len, t)).collect()
}

/// What a rule's declarations make of one box (or of one of its states).
#[derive(Default)]
struct Target {
    style: Map<String, Json>,
    /// The box's own properties: layout, position, flags.
    node: Map<String, Json>,
    dir: Option<String>,
    wrap: Option<bool>,
    fill: [Option<(Pos, String)>; 2],
}

const BORDER_SIDES: [&str; 4] = ["top", "right", "bottom", "left"];

fn clear(t: &mut Target, keys: &[&str]) {
    keys.iter().for_each(|k| _ = t.style.shift_remove(*k));
}

/// Applies one declaration. `base` is a box's own rule, not one of its states.
fn apply(d: &Decl, file: &str, base: bool, t: &mut Target, problems: &mut Vec<String>) {
    let mut say = |msg: String| problems.push(format!("{file}:{}:{}: {msg}", d.pos.line, d.pos.col));
    let name = d.name.as_str();
    let node_only = |say: &mut dyn FnMut(String)| {
        if !base {
            say(format!("{name} can't change in a state (:hover, :state()), only a box's own rule sets it"));
        }
        !base
    };
    let word = |toks: &[Tok]| match toks {
        [Tok::Ident(i)] => Some(i.clone()),
        _ => None,
    };
    match name {
        "display" => match word(&d.value).as_deref() {
            Some("flex") => {},
            Some("none") => _ = t.style.insert("visible".into(), 0.into()),
            _ => say("display is flex or none".into()),
        },
        "flex-direction" => {
            if node_only(&mut say) {
                return;
            }
            match word(&d.value).as_deref() {
                Some(w @ ("row" | "column")) => t.dir = Some(w.to_owned()),
                _ => say("flex-direction is row or column".into()),
            }
        },
        "flex-wrap" => {
            if node_only(&mut say) {
                return;
            }
            match word(&d.value).as_deref() {
                Some("wrap") => t.wrap = Some(true),
                Some("nowrap") => t.wrap = Some(false),
                _ => say("flex-wrap is wrap or nowrap".into()),
            }
        },
        "align-items" => {
            if node_only(&mut say) {
                return;
            }
            match word(&d.value).as_deref() {
                Some("flex-start" | "start") => _ = t.node.insert("align".into(), "start".into()),
                Some("flex-end" | "end") => _ = t.node.insert("align".into(), "end".into()),
                Some(w @ ("center" | "stretch")) => _ = t.node.insert("align".into(), w.into()),
                _ => say("align-items is flex-start, center, flex-end or stretch".into()),
            }
        },
        "justify-content" => {
            if node_only(&mut say) {
                return;
            }
            let v = match word(&d.value).as_deref() {
                Some("flex-start" | "start") => "start",
                Some("flex-end" | "end") => "end",
                Some("center") => "center",
                Some("space-between") => "between",
                Some("space-around") => "around",
                _ => return say("justify-content is flex-start, center, flex-end, space-between or space-around".into()),
            };
            t.node.insert("justify".into(), v.into());
        },
        "flex-grow" | "flex-shrink" => {
            if node_only(&mut say) {
                return;
            }
            match &d.value[..] {
                [Tok::Num(n)] if *n >= 0. => _ = t.node.insert(if name == "flex-grow" { "grow" } else { "shrink" }.into(), jnum(*n)),
                _ => say(format!("{name} is a number from 0")),
            }
        },
        "position" => {
            if node_only(&mut say) {
                return;
            }
            match word(&d.value).as_deref() {
                Some("absolute") => _ = t.node.entry("position").or_insert_with(|| Json::Object(Map::new())),
                Some("relative") => {},
                _ => say("position is absolute or relative".into()),
            }
        },
        "top" | "right" | "bottom" | "left" => {
            if node_only(&mut say) {
                return;
            }
            match value(name, Kind::Len, &d.value) {
                Ok(v) => {
                    if let Json::Object(m) = t.node.entry("position").or_insert_with(|| Json::Object(Map::new())) {
                        m.insert(name.to_owned(), v);
                    }
                },
                Err(e) => say(format!("{name}: {e}")),
            }
        },
        "z-index" => {
            if node_only(&mut say) {
                return;
            }
            match &d.value[..] {
                [Tok::Num(n)] if n.fract() == 0. => _ = t.node.insert("z".into(), jnum(*n)),
                _ => say("z-index is a whole number".into()),
            }
        },
        "overflow" => {
            if node_only(&mut say) {
                return;
            }
            match word(&d.value).as_deref() {
                Some("hidden") => _ = t.node.insert("clip".into(), true.into()),
                Some("visible") => _ = t.node.shift_remove("clip"),
                _ => say("overflow is hidden or visible".into()),
            }
        },
        "white-space" => {
            if node_only(&mut say) {
                return;
            }
            match word(&d.value).as_deref() {
                Some("nowrap") => {
                    // With an ellipsis it is a truncated line, whichever comes first.
                    if !t.node.contains_key("truncate") {
                        t.node.insert("nowrap".into(), true.into());
                    }
                },
                Some("normal" | "pre-wrap") => _ = t.node.insert("wrap".into(), true.into()),
                _ => say("white-space is nowrap or normal".into()),
            }
        },
        "text-overflow" => {
            if node_only(&mut say) {
                return;
            }
            match word(&d.value).as_deref() {
                Some("ellipsis") => {
                    t.node.shift_remove("nowrap");
                    t.node.insert("truncate".into(), true.into());
                },
                _ => say("text-overflow is ellipsis".into()),
            }
        },
        "transition" => {
            if node_only(&mut say) {
                return;
            }
            match transition_text(&d.value) {
                Ok(Some(text)) => _ = t.node.insert("transition".into(), text.into()),
                Ok(None) => _ = t.node.shift_remove("transition"),
                Err(e) => say(format!("transition: {e}")),
            }
        },
        "animation" => {
            if node_only(&mut say) {
                return;
            }
            match animation_items(&d.value) {
                Ok(items) if items.is_empty() => _ = t.node.shift_remove("animation"),
                Ok(mut items) if items.len() == 1 => _ = t.node.insert("animation".into(), items.remove(0).into()),
                Ok(items) => _ = t.node.insert("animation".into(), Json::Array(items.into_iter().map(Json::from).collect())),
                Err(e) => say(format!("animation: {e}")),
            }
        },
        "-tern-animation-play" => {
            if node_only(&mut say) {
                return;
            }
            match &d.value[..] {
                [Tok::Str(s)] => _ = t.node.insert("-play".into(), s.clone().into()),
                [Tok::Ident(s)] => _ = t.node.insert("-play".into(), s.clone().into()),
                _ => say("-tern-animation-play is a condition: hover, or \"hover && !x\"".into()),
            }
        },
        "transition-property"
        | "transition-duration"
        | "transition-delay"
        | "transition-timing-function"
        | "animation-name"
        | "animation-duration"
        | "animation-delay"
        | "animation-iteration-count"
        | "animation-direction"
        | "animation-timing-function"
        | "animation-fill-mode"
        | "animation-play-state" => say(format!("use the {} shorthand, not {name}", name.split('-').next().unwrap_or(name))),
        "transform" => say("transform works inside @keyframes only".into()),
        "float" | "grid" | "grid-template-columns" | "grid-template-rows" | "table-layout" => say(format!("no property \"{name}\"")),
        "box-shadow" => match shadow(&d.value) {
            Ok(v) => _ = t.style.insert("shadow".into(), v),
            Err(e) => say(format!("box-shadow: {e}")),
        },
        "-tern-element" => match &d.value[..] {
            [Tok::Str(s)] => _ = t.style.insert("element".into(), s.clone().into()),
            _ => say("-tern-element is an element's name in quotes".into()),
        },
        "padding" | "margin" => {
            let sides = match lens(name, &d.value) {
                Ok(s) => s,
                Err(e) => return say(format!("{name}: {e}")),
            };
            let keys: Vec<String> = ["", "_x", "_y", "_top", "_right", "_bottom", "_left"].iter().map(|s| format!("{name}{s}")).collect();
            clear(t, &keys.iter().map(String::as_str).collect::<Vec<_>>());
            let k = |s: &str| format!("{name}{s}");
            let set: Vec<(String, Json)> = match &sides[..] {
                [a] => vec![(name.to_owned(), a.clone())],
                [a, b] => vec![(k("_y"), a.clone()), (k("_x"), b.clone())],
                [a, b, c] => vec![(k("_top"), a.clone()), (k("_x"), b.clone()), (k("_bottom"), c.clone())],
                [a, b, c, e] => vec![(k("_top"), a.clone()), (k("_right"), b.clone()), (k("_bottom"), c.clone()), (k("_left"), e.clone())],
                _ => vec![],
            };
            t.style.extend(set);
        },
        "border-width" => {
            let sides = match lens("border_width", &d.value) {
                Ok(s) => s,
                Err(e) => return say(format!("border-width: {e}")),
            };
            clear(t, &["border_width", "border_top_width", "border_right_width", "border_bottom_width", "border_left_width"]);
            match &sides[..] {
                [a] => _ = t.style.insert("border_width".into(), a.clone()),
                _ => {
                    let at = |i: usize| match sides.len() {
                        2 => sides[i % 2].clone(),
                        3 => sides[[0, 1, 2, 1][i]].clone(),
                        _ => sides[i].clone(),
                    };
                    for (i, s) in BORDER_SIDES.iter().enumerate() {
                        t.style.insert(format!("border_{s}_width"), at(i));
                    }
                },
            }
        },
        "border-radius" => {
            let c = match lens("radius", &d.value) {
                Ok(s) => s,
                Err(e) => return say(format!("border-radius: {e}")),
            };
            clear(t, &["radius", "radius_top_left", "radius_top_right", "radius_bottom_right", "radius_bottom_left"]);
            match &c[..] {
                [a] => _ = t.style.insert("radius".into(), a.clone()),
                _ => {
                    let at = |i: usize| match c.len() {
                        2 => c[i % 2].clone(),
                        3 => c[[0, 1, 2, 1][i]].clone(),
                        _ => c[i].clone(),
                    };
                    for (i, k) in ["radius_top_left", "radius_top_right", "radius_bottom_right", "radius_bottom_left"].iter().enumerate() {
                        t.style.insert((*k).to_owned(), at(i));
                    }
                },
            }
        },
        "border" | "border-top" | "border-right" | "border-bottom" | "border-left" => {
            let key = match name.strip_prefix("border-") {
                Some(side) => format!("border_{side}_width"),
                None => "border_width".to_owned(),
            };
            for tok in &d.value {
                match tok {
                    Tok::Dim(..) | Tok::Num(_) => match value("border_width", Kind::Len, std::slice::from_ref(tok)) {
                        Ok(v) => _ = t.style.insert(key.clone(), v),
                        Err(e) => say(format!("{name}: {e}")),
                    },
                    Tok::Ident(i) if i == "solid" => {},
                    Tok::Ident(i) if i == "none" => _ = t.style.insert(key.clone(), 0.into()),
                    other => match value("border", Kind::Color, std::slice::from_ref(other)) {
                        Ok(v) => _ = t.style.insert("border".into(), v),
                        Err(e) => say(format!("{name}: {e}")),
                    },
                }
            }
        },
        "background" => {
            // A color, or a picture.
            for tok in &d.value {
                let (key, kind) = if matches!(tok, Tok::Func(f, _) if f == "url") {
                    ("background_image", Kind::Text)
                } else {
                    ("background", Kind::Color)
                };
                match value(key, kind, std::slice::from_ref(tok)) {
                    Ok(v) => _ = t.style.insert(key.into(), v),
                    Err(e) => say(format!("background: {e}")),
                }
            }
        },
        "width" | "height" if matches!(&d.value[..], [Tok::Pct(p)] if *p == 100.) => {
            if !base {
                return say(format!("{name}: 100% can't change in a state"));
            }
            t.fill[usize::from(name == "height")] = Some((d.pos, file.to_owned()));
            t.style.shift_remove(name);
        },
        _ => {
            let Some(key) = style_key(name) else { return say(format!("no property \"{name}\"")) };
            if key == "element" {
                return say("-tern-element is an element's name in quotes".into());
            }
            let Some(&(_, kind, _)) = style::box_prop(&key) else { return say(format!("no property \"{name}\"")) };
            match value(&key, kind, &d.value) {
                Ok(v) => {
                    if name == "width" || name == "height" {
                        t.fill[usize::from(name == "height")] = None;
                    }
                    t.style.insert(key, v);
                },
                Err(e) => say(format!("{name}: {e}")),
            }
        },
    }
}

/// `animation` items: the shorthand split at commas.
fn animation_items(toks: &[Tok]) -> Result<Vec<String>, String> {
    if matches!(toks, [Tok::Ident(i)] if i == "none") {
        return Ok(vec![]);
    }
    items(toks).map(|v| v.into_iter().map(|words| words.join(" ")).collect())
}

/// `transition`: names mapped to Tern's, joined at commas.
fn transition_text(toks: &[Tok]) -> Result<Option<String>, String> {
    if matches!(toks, [Tok::Ident(i)] if i == "none") {
        return Ok(None);
    }
    let mut out = vec![];
    for mut words in items(toks)? {
        let prop = words.first().cloned().unwrap_or_default();
        if prop != "all" {
            match style_key(&prop) {
                Some(k) if style::box_prop(&k).is_some() => words[0] = k,
                _ => return Err(format!("no property \"{prop}\"")),
            }
        }
        out.push(words.join(" "));
    }
    Ok(Some(out.join(", ")))
}

fn tok_text(t: &Tok) -> Result<String, String> {
    Ok(match t {
        Tok::Num(n) => num_text(*n),
        Tok::Dim(n, u) => format!("{}{u}", num_text(*n)),
        Tok::Pct(n) => format!("{}%", num_text(*n)),
        Tok::Ident(i) => i.clone(),
        Tok::Func(f, args) => {
            let parts: Result<Vec<String>, String> = args.iter().filter(|a| **a != Tok::Comma).map(tok_text).collect();
            format!("{f}({})", parts?.join(", "))
        },
        other => return Err(format!("{other:?} doesn't belong here")),
    })
}

/// A comma list of space-separated words.
fn items(toks: &[Tok]) -> Result<Vec<Vec<String>>, String> {
    let mut out = vec![];
    for part in toks.split(|t| *t == Tok::Comma) {
        let words: Result<Vec<String>, String> = part.iter().map(tok_text).collect();
        let words = words?;
        if words.is_empty() {
            return Err("an empty item".into());
        }
        out.push(words);
    }
    Ok(out)
}

/// A shorthand as the object form (for a `play` condition).
fn animation_object(text: &str, play: &str) -> Json {
    let mut o = Map::new();
    let mut times = 0;
    for w in text.split_whitespace() {
        let secs = |w: &str| match w.strip_suffix("ms") {
            Some(n) => n.parse::<f64>().ok().map(|n| n / 1000.),
            None => w.strip_suffix('s').and_then(|n| n.parse::<f64>().ok()),
        };
        if let Some(s) = secs(w) {
            o.insert(if times == 0 { "duration" } else { "delay" }.into(), jnum(s));
            times += 1;
        } else if w == "infinite" {
            o.insert("iteration".into(), "infinite".into());
        } else if let Ok(n) = w.parse::<f64>() {
            o.insert("iteration".into(), jnum(n));
        } else if ["normal", "reverse", "alternate", "alternate-reverse"].contains(&w) {
            o.insert("direction".into(), w.into());
        } else if ["forwards", "backwards", "both"].contains(&w) {
            o.insert("fill".into(), w.into());
        } else if ["linear", "ease", "ease-in", "ease-out", "ease-in-out"].contains(&w)
            || w.starts_with("steps(")
            || w.starts_with("cubic-bezier(")
        {
            o.insert("easing".into(), w.into());
        } else {
            o.insert("name".into(), w.into());
        }
    }
    o.insert("play".into(), play.into());
    Json::Object(o)
}

/// The elements of one template, for matching selectors.
#[derive(Default)]
struct Arena {
    els: Vec<ArenaEl>,
}

struct ArenaEl {
    tag: String,
    attrs: Vec<(String, String)>,
    parent: Option<usize>,
    /// The element's address, to find it again.
    addr: usize,
}

impl Arena {
    fn add(&mut self, el: &Element, parent: Option<usize>) {
        let ix = self.els.len();
        self.els.push(ArenaEl {
            tag: el.tag.clone(),
            attrs: el.attrs.iter().map(|a| (a.name.clone(), a.value.clone().unwrap_or_default())).collect(),
            parent,
            addr: el as *const Element as usize,
        });
        for c in &el.children {
            if let Item::Element(e) = c {
                self.add(e, Some(ix));
            }
        }
    }

    fn find(&self, el: &Element) -> usize {
        let addr = el as *const Element as usize;
        self.els.iter().position(|e| e.addr == addr).unwrap_or(0)
    }
}

impl Dom for Arena {
    fn tag(&self, el: usize) -> &str {
        &self.els[el].tag
    }

    fn attr(&self, el: usize, name: &str) -> Option<&str> {
        self.els[el].attrs.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    fn parent(&self, el: usize) -> Option<usize> {
        self.els[el].parent
    }
}

/// A rule that selects a box: its specificity, its place in the stylesheets, its condition, its file, the rule.
type Matched<'a> = ((u32, u32, u32), usize, Option<String>, usize, &'a css::Rule);

/// Tags that are plain boxes.
const BOXES: &[&str] = &[
    "div", "span", "section", "article", "aside", "header", "footer", "main", "nav", "ul", "ol", "li", "p", "h1", "h2", "h3", "h4", "h5",
    "h6",
];

/// Tags with their own meaning.
const SPECIAL: &[&str] = &["img", "icon", "slot", "use", "actor", "ring", "button", "switch", "separator", "menu-item", "each", "after"];

struct Compiler<'a> {
    file: &'a str,
    css_files: &'a [String],
    rules: &'a [(usize, css::Rule)],
    problems: &'a mut Vec<String>,
    origins: &'a mut Vec<(String, String, Pos)>,
    arena: Arena,
    template: String,
}

/// A flag attribute: present or `"true"` is on, `"false"` off, anything else a condition.
fn flag(v: &Option<String>) -> Json {
    match v.as_deref() {
        None | Some("") | Some("true") => true.into(),
        Some("false") => false.into(),
        Some(c) => c.to_owned().into(),
    }
}

/// A number, or text.
fn scalar(v: &str) -> Json {
    match v.parse::<f64>() {
        Ok(n)
            if n.is_finite() && {
                let text = jnum(n).to_string();
                text == v
            } =>
        {
            jnum(n)
        },
        _ => v.to_owned().into(),
    }
}

impl Compiler<'_> {
    fn say(&mut self, pos: Pos, msg: String) {
        self.problems.push(format!("{}:{}:{}: {msg}", self.file, pos.line, pos.col));
    }

    fn attr(&mut self, tag: &str, a: &html::Attr, obj: &mut Map<String, Json>) {
        let text = a.value.clone().unwrap_or_default();
        let by_tag = |names: &[&str]| names.contains(&tag);
        let mut set = |key: &str, v: Json| _ = obj.insert(key.to_owned(), v);
        match a.name.as_str() {
            "class" | "id" => {},
            "style" => self.say(a.pos, "style=\"…\" is not supported: give the element a class and style it in a stylesheet".into()),
            "name" if by_tag(&["button", "switch", "slot", "actor", "icon"]) => {
                set(tag, text.into());
            },
            "src" if tag == "img" => set("image", text.into()),
            "template" if tag == "use" => set("use", text.into()),
            "element" if tag == "use" => set("element", text.into()),
            "value" if tag == "ring" => set("ring", text.into()),
            "list" if tag == "each" => set("each", text.into()),
            "icon" if by_tag(&["button", "menu-item"]) => set("icon", text.into()),
            "variant" if tag == "button" => set("variant", text.into()),
            "size" if tag == "button" => set("size", text.into()),
            "tooltip" if by_tag(&["button", "switch", "menu-item"]) => set("tooltip", text.into()),
            "action" if by_tag(&["button", "menu-item"]) => set("action", text.into()),
            "menu" if tag == "button" => set("menu", text.into()),
            "label" if by_tag(&["button", "menu-item"]) => set("label", text.into()),
            "selected" | "disabled" | "loading" if by_tag(&["button", "switch", "menu-item"]) => set(a.name.as_str(), flag(&a.value)),
            "data-when" | "data-frame" | "data-element" | "data-part" | "data-kind" | "data-dock" | "data-strip" | "data-group"
            | "data-empty" | "data-split" => set(&a.name["data-".len()..], text.into()),
            "data-layout" => {
                if text == "fit" {
                    set("layout", "fit".into());
                } else {
                    self.say(a.pos, "data-layout is fit (other layouts are flex-direction and flex-wrap)".into());
                }
            },
            "data-accepts" | "data-zones" => {
                set(&a.name["data-".len()..], Json::Array(text.split_whitespace().map(|s| Json::from(s.to_owned())).collect()))
            },
            "data-size" | "data-min" | "data-max" | "data-part-size" => set(&a.name["data-".len()..].replace('-', "_"), scalar(&text)),
            "data-fixed" | "data-drag" | "data-float" | "data-window" | "data-attach" => set(&a.name["data-".len()..], flag(&a.value)),
            "data-handle" | "data-overlay" | "data-border-under" | "data-grouped" => match flag(&a.value) {
                Json::Bool(b) => set(&a.name["data-".len()..].replace('-', "_"), b.into()),
                _ => self.say(a.pos, format!("{} is on (no value) or true or false", a.name)),
            },
            n if n.starts_with("data-on-") => {
                let event = &n["data-on-".len()..];
                let v = if text.trim_start().starts_with(['{', '[']) {
                    serde_json::from_str(&text).unwrap_or(Json::String(text))
                } else {
                    text.into()
                };
                if !["click", "hover", "appear"].contains(&event) {
                    self.say(a.pos, format!("{n}: an event is click, hover or appear"));
                } else if let Json::Object(m) = obj.entry("on").or_insert_with(|| Json::Object(Map::new())) {
                    m.insert(event.to_owned(), v);
                }
            },
            n => self.say(a.pos, format!("<{tag}> has no attribute \"{n}\"")),
        }
    }

    fn children(&mut self, el: &Element, path: &str, skip_after: bool, texts: bool) -> Vec<Json> {
        let mut out: Vec<Json> = vec![];
        for item in &el.children {
            match item {
                Item::Element(e) if skip_after && e.tag == "after" => {},
                Item::Element(e) => {
                    let p = format!("{path}.children.{}", out.len());
                    if let Some(n) = self.node(e, &p) {
                        out.push(n);
                    }
                },
                Item::Text(..) if !texts => {},
                Item::Text(t, pos) => {
                    let mut m = Map::new();
                    m.insert("text".into(), html::decode(t).into());
                    self.origins.push((format!("templates.{}{path}.children.{}", self.template, out.len()), self.file.to_owned(), *pos));
                    out.push(Json::Object(m));
                },
                Item::Raw(_, pos) => self.say(*pos, "unexpected raw text".into()),
            }
        }
        out
    }

    fn node(&mut self, el: &Element, path: &str) -> Option<Json> {
        let tag = el.tag.as_str();
        if !BOXES.contains(&tag) && !SPECIAL.contains(&tag) {
            let hint = match tag {
                "script" => " (JavaScript is not supported)",
                "table" | "tr" | "td" | "form" | "input" => " (tables and forms are not supported; there is <button>)",
                "template" => " (templates are not nested)",
                _ => "",
            };
            self.say(el.pos, format!("<{tag}> is not an element a template takes{hint}"));
            return None;
        }
        if tag == "after" {
            self.say(el.pos, "<after> goes inside <each>".into());
            return None;
        }
        self.origins.push((format!("templates.{}{path}", self.template), self.file.to_owned(), el.pos));
        let ix = self.arena.find(el);
        let mut obj = Map::new();
        if tag == "separator" {
            obj.insert("separator".into(), true.into());
        }
        for a in &el.attrs {
            self.attr(tag, a, &mut obj);
        }
        match tag {
            "each" => {
                let item = el.children.iter().find_map(|c| match c {
                    Item::Element(e) if e.tag != "after" => Some(e),
                    _ => None,
                });
                match item {
                    Some(e) => {
                        if let Some(n) = self.node(e, &format!("{path}.item")) {
                            obj.insert("item".into(), n);
                        }
                    },
                    None => self.say(el.pos, "<each> needs an element to draw for each entry".into()),
                }
                if el.children.iter().filter(|c| matches!(c, Item::Element(e) if e.tag != "after")).count() > 1 {
                    self.say(el.pos, "<each> has one element to repeat; what comes after the entries goes in <after>".into());
                }
                let mut after = vec![];
                for c in &el.children {
                    if let Item::Element(a) = c
                        && a.tag == "after"
                    {
                        after.extend(self.children(a, path, false, true));
                    }
                }
                if !after.is_empty() {
                    obj.insert("children".into(), Json::Array(after));
                }
            },
            "button" | "menu-item" => {
                let text: Vec<&str> =
                    el.children.iter().filter_map(|c| if let Item::Text(t, _) = c { Some(t.as_str()) } else { None }).collect();
                if !text.is_empty() {
                    obj.insert("label".into(), html::decode(&text.join(" ")).into());
                }
                let kids = self.children(el, path, false, false);
                if !kids.is_empty() {
                    obj.insert("children".into(), Json::Array(kids));
                }
            },
            t if BOXES.contains(&t) => {
                let only_text = !el.children.is_empty() && el.children.iter().all(|c| matches!(c, Item::Text(..)));
                if only_text {
                    let text: Vec<&str> =
                        el.children.iter().filter_map(|c| if let Item::Text(t, _) = c { Some(t.as_str()) } else { None }).collect();
                    obj.insert("text".into(), html::decode(&text.join(" ")).into());
                } else {
                    let kids = self.children(el, path, false, true);
                    if !kids.is_empty() {
                        obj.insert("children".into(), Json::Array(kids));
                    }
                }
            },
            _ => {
                for c in &el.children {
                    if let Item::Text(_, pos) = c {
                        self.say(*pos, format!("<{tag}> takes no text"));
                    }
                }
                let kids = self.children(el, path, false, true);
                if !kids.is_empty() {
                    obj.insert("children".into(), Json::Array(kids));
                }
            },
        }
        // Required names.
        for (t, key) in [
            ("icon", "icon"),
            ("slot", "slot"),
            ("actor", "actor"),
            ("use", "use"),
            ("button", "button"),
            ("switch", "switch"),
            ("img", "image"),
            ("ring", "ring"),
        ] {
            if tag == t && !obj.contains_key(key) {
                self.say(
                    el.pos,
                    format!(
                        "<{tag}> needs {}",
                        if t == "img" {
                            "src"
                        } else if t == "use" {
                            "template"
                        } else if t == "ring" {
                            "value"
                        } else {
                            "name"
                        }
                    ),
                );
            }
        }
        self.style(el, ix, &mut obj);
        Some(Json::Object(obj))
    }

    /// The CSS rules that apply to `el`, as its `style`, `states` and own properties.
    fn style(&mut self, el: &Element, ix: usize, obj: &mut Map<String, Json>) {
        let mut matched: Vec<Matched> = vec![];
        for (ri, (fi, rule)) in self.rules.iter().enumerate() {
            for sel in &rule.selectors {
                if css::matches(sel, &self.arena, ix) {
                    matched.push((sel.specificity, ri, sel.condition(), *fi, rule));
                }
            }
        }
        if matched.is_empty() {
            return;
        }
        matched.sort_by_key(|m| (m.0, m.1));
        let mut base = Target::default();
        let mut states: BTreeMap<String, Target> = BTreeMap::new();
        for (_, _, cond, fi, rule) in matched {
            let (target, is_base) = match cond {
                None => (&mut base, true),
                Some(c) => (states.entry(c).or_default(), false),
            };
            for d in &rule.decls {
                apply(d, &self.css_files[fi], is_base, target, self.problems);
            }
        }
        // Width and height both 100% is `fill`.
        match base.fill {
            [Some(_), Some(_)] => _ = base.node.insert("fill".into(), true.into()),
            [Some((p, f)), None] | [None, Some((p, f))] => {
                let msg = "only width and height 100% together mean fill; % is not supported otherwise";
                self.problems.push(format!("{f}:{}:{}: {msg}", p.line, p.col));
            },
            [None, None] => {},
        }
        let layout = match (base.dir.as_deref(), base.wrap) {
            (_, Some(true)) => Some("wrap"),
            (Some(d), _) => Some(d),
            _ => None,
        };
        if let Some(l) = layout {
            if matches!(obj.get("layout"), Some(Json::String(s)) if s == "fit") {
                self.say(el.pos, "layout is set twice: data-layout=\"fit\" and flex-direction".into());
            } else {
                obj.insert("layout".into(), l.into());
            }
        }
        // `-tern-animation-play` turns the shorthand strings into objects.
        if let Some(play) = base.node.shift_remove("-play").and_then(|p| p.as_str().map(str::to_owned)) {
            match base.node.get("animation").cloned() {
                Some(Json::String(s)) => _ = base.node.insert("animation".into(), animation_object(&s, &play)),
                Some(Json::Array(list)) => {
                    let list = list.iter().filter_map(Json::as_str).map(|s| animation_object(s, &play)).collect();
                    base.node.insert("animation".into(), Json::Array(list));
                },
                _ => self.say(el.pos, "-tern-animation-play needs an animation".into()),
            }
        }
        for (k, v) in base.node {
            obj.insert(k, v);
        }
        if !base.style.is_empty() {
            obj.insert("style".into(), Json::Object(base.style));
        }
        if !states.is_empty() {
            let mut m = Map::new();
            for (cond, t) in states {
                m.insert(cond, Json::Object(t.style));
            }
            obj.insert("states".into(), Json::Object(m));
        }
    }
}

/// A `transform` list in a keyframe, as Tern's channels.
fn transform(toks: &[Tok], out: &mut BTreeMap<String, Json>) -> Result<(), String> {
    let len = |t: &Tok| match t {
        Tok::Dim(n, u) if u == "px" => Ok(jnum(*n)),
        Tok::Num(n) if *n == 0. => Ok(0.into()),
        _ => Err("a translation is in px".to_owned()),
    };
    for t in toks {
        let Tok::Func(f, args) = t else { return Err("transform is translate(), translateX(), translateY(), scale() or rotate()".into()) };
        let args: Vec<&Tok> = args.iter().filter(|a| **a != Tok::Comma).collect();
        match (f.as_str(), &args[..]) {
            ("translate", [x]) => _ = out.insert("translate_x".into(), len(x)?),
            ("translate", [x, y]) => {
                out.insert("translate_x".into(), len(x)?);
                out.insert("translate_y".into(), len(y)?);
            },
            ("translatex", [x]) => _ = out.insert("translate_x".into(), len(x)?),
            ("translatey", [y]) => _ = out.insert("translate_y".into(), len(y)?),
            ("scale", [Tok::Num(n)]) => _ = out.insert("scale".into(), jnum(*n)),
            ("rotate", [Tok::Dim(n, u)]) if u == "deg" => _ = out.insert("rotate".into(), jnum(*n)),
            ("rotate", [Tok::Num(n)]) if *n == 0. => _ = out.insert("rotate".into(), 0.into()),
            (f, _) => return Err(format!("{f}() is not supported here (translate, translateX, translateY, scale(n), rotate(Ndeg))")),
        }
    }
    Ok(())
}

/// Compiles the files. `media` says what `@media (prefers-reduced-motion)` asks.
pub fn compile(html_files: &[Source], css_files: &[Source], media: Media, problems: &mut Vec<String>) -> Markup {
    let mut out = Markup::default();
    let mut names: Vec<String> = css_files.iter().map(|s| s.file.clone()).collect();
    let mut rules: Vec<(usize, css::Rule)> = vec![];
    let mut sheets: Vec<(usize, css::Sheet)> = vec![];
    for (i, s) in css_files.iter().enumerate() {
        sheets.push((i, css::parse(&s.text, &s.file, 0, media, problems)));
    }
    // A `<style>` in an HTML file comes after the stylesheets.
    let mut parsed: Vec<(String, Vec<Item>)> = vec![];
    for s in html_files {
        let items = html::parse(&s.text, &s.file, problems);
        for item in &items {
            if let Item::Element(e) = item
                && e.tag == "style"
            {
                for c in &e.children {
                    if let Item::Raw(text, pos) = c {
                        names.push(s.file.clone());
                        sheets.push((names.len() - 1, css::parse(text, &s.file, pos.line - 1, media, problems)));
                    }
                }
            }
        }
        parsed.push((s.file.clone(), items));
    }
    for (fi, sheet) in sheets {
        rules.extend(sheet.rules.into_iter().map(|r| (fi, r)));
        for t in sheet.tokens {
            match generic(&t.value) {
                Ok(v) => _ = out.tokens.insert(token_path(&t.name), v),
                Err(e) => problems.push(format!("{}:{}:{}: {}: {e}", names[fi], t.pos.line, t.pos.col, t.name)),
            }
        }
        for f in sheet.frames {
            let file = &names[fi];
            let mut stops: BTreeMap<String, BTreeMap<String, Json>> = BTreeMap::new();
            for (key, decls) in &f.stops {
                let stop = stops.entry(key.clone()).or_default();
                for d in decls {
                    if d.name == "transform" {
                        if let Err(e) = transform(&d.value, stop) {
                            problems.push(format!("{file}:{}:{}: {e}", d.pos.line, d.pos.col));
                        }
                    } else {
                        let mut t = Target::default();
                        apply(d, file, true, &mut t, problems);
                        if !t.node.is_empty() || t.fill.iter().any(Option::is_some) {
                            problems.push(format!("{file}:{}:{}: {} can't be animated", d.pos.line, d.pos.col, d.name));
                        }
                        stop.extend(t.style);
                    }
                }
            }
            out.origins.push((format!("animations.{}", f.name), file.clone(), f.pos));
            if out.animations.insert(f.name.clone(), stops).is_some() {
                problems.push(format!("{file}:{}:{}: @keyframes {} twice", f.pos.line, f.pos.col, f.name));
            }
        }
    }
    let mut seen: BTreeMap<String, (String, Pos)> = BTreeMap::new();
    for (file, items) in &parsed {
        for item in items {
            match item {
                Item::Element(e) if e.tag == "style" => {},
                Item::Element(e) if e.tag == "template" => {
                    let Some(name) = e.attr("name").and_then(|a| a.value.clone()).filter(|n| !n.is_empty()) else {
                        problems.push(format!("{file}:{}:{}: <template> needs a name", e.pos.line, e.pos.col));
                        continue;
                    };
                    if let Some((f, p)) = seen.get(&name) {
                        problems.push(format!(
                            "{file}:{}:{}: template \"{name}\" is also defined at {f}:{}:{}",
                            e.pos.line, e.pos.col, p.line, p.col
                        ));
                    }
                    seen.insert(name.clone(), (file.clone(), e.pos));
                    for a in &e.attrs {
                        if a.name != "name" {
                            problems.push(format!("{file}:{}:{}: <template> has no attribute \"{}\"", a.pos.line, a.pos.col, a.name));
                        }
                    }
                    let roots: Vec<&Element> =
                        e.children.iter().filter_map(|c| if let Item::Element(r) = c { Some(r) } else { None }).collect();
                    if roots.len() != 1 || e.children.iter().any(|c| matches!(c, Item::Text(..))) {
                        problems.push(format!("{file}:{}:{}: template \"{name}\" is one element", e.pos.line, e.pos.col));
                        continue;
                    }
                    let mut arena = Arena::default();
                    arena.add(e, None);
                    let mut c = Compiler {
                        file,
                        css_files: &names,
                        rules: &rules,
                        problems,
                        origins: &mut out.origins,
                        arena,
                        template: name.clone(),
                    };
                    let Some(json) = c.node(roots[0], "") else { continue };
                    match serde_json::from_value::<Spec>(json.clone()) {
                        Ok(spec) => {
                            out.templates.insert(name.clone(), spec);
                            out.json.insert(name, json);
                        },
                        Err(err) => problems.push(format!("{file}:{}:{}: template \"{name}\": {err}", e.pos.line, e.pos.col)),
                    }
                },
                Item::Element(e) => problems.push(format!(
                    "{file}:{}:{}: only <template> and <style> go at the top of the file, not <{}>",
                    e.pos.line, e.pos.col, e.tag
                )),
                Item::Text(_, pos) => problems.push(format!("{file}:{}:{}: text outside a <template>", pos.line, pos.col)),
                Item::Raw(..) => {},
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A template as the JSON form compares it: whole numbers alike, defaults and
    /// spellings that mean the same made the same.
    fn canon(v: &Json) -> Json {
        match v {
            Json::Number(n) => n.as_f64().map_or_else(|| v.clone(), jnum),
            Json::Array(l) => Json::Array(l.iter().map(canon).collect()),
            Json::Object(m) => {
                let mut out = Map::new();
                for (k, x) in m {
                    let x = canon(x);
                    let empty_list = matches!(&x, Json::Array(l) if l.is_empty());
                    if (k == "layout" && x == "column")
                        || (k == "children" && empty_list)
                        || (k == "states" && x == Json::Object(Map::new()))
                    {
                        continue;
                    }
                    let x = if ["selected", "disabled", "loading", "fixed", "drag", "float", "window", "attach"].contains(&k.as_str()) {
                        match &x {
                            Json::String(s) if s == "true" => Json::Bool(true),
                            Json::String(s) if s == "false" => Json::Bool(false),
                            _ => x,
                        }
                    } else if k == "states" {
                        // Conditions are written with single spaces around &&.
                        let Json::Object(sm) = &x else { unreachable!() };
                        Json::Object(
                            sm.iter().map(|(c, v)| (c.split("&&").map(str::trim).collect::<Vec<_>>().join(" && "), v.clone())).collect(),
                        )
                    } else {
                        x
                    };
                    out.insert(k.clone(), x);
                }
                Json::Object(out)
            },
            other => other.clone(),
        }
    }

    fn build(html: &str, css: &str) -> (Markup, Vec<String>) {
        let mut problems = vec![];
        let m = compile(
            &[Source { file: "t.html".into(), text: html.into() }],
            &[Source { file: "t.css".into(), text: css.into() }],
            Media::default(),
            &mut problems,
        );
        (m, problems)
    }

    #[test]
    fn built_in_templates_round_trip() {
        let all: Map<String, Json> = serde_json::from_str(super::super::BUILTIN).expect("built-in templates");
        let (html, css) = super::super::unmarkup::render(&all).expect("renders");
        let mut problems = vec![];
        let m = compile(
            &[Source { file: "templates.html".into(), text: html }],
            &[Source { file: "templates.css".into(), text: css }],
            Media::default(),
            &mut problems,
        );
        assert_eq!(problems, Vec::<String>::new());
        assert_eq!(m.json.len(), all.len());
        for (name, want) in &all {
            assert_eq!(canon(&m.json[name]), canon(want), "template {name}");
        }
    }

    #[test]
    fn rendered_templates_compile_like_the_json() {
        let (html, css) = super::super::builtin_markup();
        let mut problems = vec![];
        let m = compile(
            &[Source { file: "templates.html".into(), text: html }],
            &[Source { file: "templates.css".into(), text: css }],
            Media::default(),
            &mut problems,
        );
        assert_eq!(problems, Vec::<String>::new());
        let over = style::Overrides::default();
        let theme = gpui_kit::component::ThemeColor::default();
        let props = style::Props::new(&over, &theme, std::path::Path::new(""));
        let mut from_json = vec![];
        super::super::compile(&super::super::builtin(), &props, &mut from_json);
        let mut from_html = vec![];
        super::super::compile(&m.templates, &props, &mut from_html);
        assert_eq!(from_html, from_json);
    }

    fn one(html: &str, css: &str) -> Json {
        let (m, problems) = build(&format!("<template name=\"t\">{html}</template>"), css);
        assert_eq!(problems, Vec::<String>::new());
        m.json["t"].clone()
    }

    fn json(t: &str) -> Json {
        serde_json::from_str(t).expect("json")
    }

    #[test]
    fn elements_map_to_nodes() {
        let t = one(
            r##"<div class="a" data-when="focused && !x" data-frame="f" data-group="g" data-element="row">
                 <span>Hello {name}</span>
                 <img src="images/x.png"/>
                 <icon name="folder-open"/>
                 <button name="close" icon="x" variant="ghost" size="xsmall" tooltip="Close" action="workspace::X" menu="file" selected disabled="busy">Label</button>
                 <switch name="sw" selected="on"/>
                 <slot name="icon"/>
                 <use template="pane.body" element="agents.tab"/>
                 <actor name="rabbit"/>
                 <ring value="{pct}"/>
                 <each list="items"><div>row</div><after><separator/></after></each>
                 <menu-item action="a::B" label="Do"/>
                 <menu-item label="Sub"><menu-item action="a::C"/></menu-item>
                 bare text
               </div>"##,
            "",
        );
        assert_eq!(
            t,
            json(
                r##"{"when": "focused && !x", "frame": "f", "group": "g", "element": "row", "children": [
                  {"text": "Hello {name}"},
                  {"image": "images/x.png"},
                  {"icon": "folder-open"},
                  {"button": "close", "icon": "x", "variant": "ghost", "size": "xsmall", "tooltip": "Close", "action": "workspace::X",
                   "menu": "file", "selected": true, "disabled": "busy", "label": "Label"},
                  {"switch": "sw", "selected": "on"},
                  {"slot": "icon"},
                  {"use": "pane.body", "element": "agents.tab"},
                  {"actor": "rabbit"},
                  {"ring": "{pct}"},
                  {"each": "items", "item": {"text": "row"}, "children": [{"separator": true}]},
                  {"action": "a::B", "label": "Do"},
                  {"label": "Sub", "children": [{"action": "a::C"}]},
                  {"text": "bare text"}]}"##
            )
        );
    }

    #[test]
    fn data_attributes_map_to_template_fields() {
        let t = one(
            r##"<div data-dock="main" data-accepts="panel agents" data-zones="edges stack" data-strip="left" data-split="row" data-empty="keep"
                    data-size="300" data-min="2rem" data-max="400" data-fixed data-handle data-group="g" data-part="explorer" data-kind="panel"
                    data-drag="false" data-float="locked" data-attach data-window data-overlay data-border-under data-grouped
                    data-part-size="20" data-on-click="pop 0.3s" data-layout="fit"></div>"##,
            "",
        );
        assert_eq!(
            t,
            json(
                r##"{"dock": "main", "accepts": ["panel", "agents"], "zones": ["edges", "stack"], "strip": "left", "split": "row", "empty": "keep",
                    "size": 300, "min": "2rem", "max": 400, "fixed": true, "handle": true, "group": "g", "part": "explorer", "kind": "panel",
                    "drag": false, "float": "locked", "attach": true, "window": true, "overlay": true, "border_under": true, "grouped": true,
                    "part_size": 20, "on": {"click": "pop 0.3s"}, "layout": "fit"}"##
            )
        );
    }

    #[test]
    fn css_properties_map_to_styles() {
        let t = one(
            r##"<div class="a"></div>"##,
            r##".a { display: flex; flex-direction: row; align-items: flex-end; justify-content: space-between; flex-grow: 2; flex-shrink: 0;
                    width: 12px; height: 2rem; min-width: 0; max-height: auto; padding: 4px 8px; margin: 1px 2px 3px 4px; gap: 6px;
                    border-radius: 6px; border-top-left-radius: 2px; border: 1px solid #333333; background: #111111; color: var(--muted--foreground);
                    opacity: 0.5; font-family: "Fira Code"; font-size: 0.875rem; font-weight: 600; font-style: italic; line-height: 18px;
                    text-transform: uppercase; -webkit-line-clamp: {lines};
                    box-shadow: lg; background-image: url("images/b.png"); background-size: cover; z-index: 3; overflow: hidden;
                    position: absolute; top: 0; right: -1.5px; bottom: var(--bar--bottom); left: {left}; white-space: nowrap; text-overflow: ellipsis }"##,
        );
        let want = json(
            r##"{"layout": "row", "align": "end", "justify": "between", "grow": 2, "shrink": 0, "z": 3, "clip": true, "truncate": true,
                "position": {"top": 0, "right": -1.5, "bottom": "$bar.bottom", "left": "{left}"},
                "style": {"width": 12, "height": "2rem", "min_width": 0, "max_height": "auto", "padding_y": 4, "padding_x": 8,
                  "margin_top": 1, "margin_right": 2, "margin_bottom": 3, "margin_left": 4, "gap": 6, "radius": 6, "radius_top_left": 2,
                  "border_width": 1, "border": "#333333", "background": "#111111", "text": "$muted.foreground", "opacity": 0.5,
                  "font_family": "Fira Code", "font_size": "0.875rem", "font_weight": 600, "italic": 1, "line_height": 18,
                  "text_transform": "uppercase", "line_clamp": "{lines}", "shadow": "lg", "background_image": "images/b.png",
                  "background_image_fit": "cover"}}"##,
        );
        assert_eq!(canon(&t), canon(&want));
    }

    #[test]
    fn states_specificity_and_order() {
        let t = one(
            r##"<div class="a b"><span class="c">x</span></div>"##,
            r##".a { background: #111111; opacity: 1 }
               .a:hover { background: #222222 }
               .a:hover:not(:active) { opacity: 0.5 }
               .a:focus, .a:checked { color: #ffffff }
               .a:state(tabs > 1) { gap: 2px }
               .a:state(hover:stab) { opacity: 0 }
               div.a.b { opacity: 0.9 }
               .a { opacity: 0.8 }
               div .c { padding: 4px }
               div > .c { padding: 2px }
               #nope { padding: 9px }"##,
        );
        assert_eq!(
            canon(&t),
            canon(&json(
                r##"{"style": {"background": "#111111", "opacity": 0.9},
                    "states": {"hover": {"background": "#222222"}, "hover && !active": {"opacity": 0.5}, "focused": {"text": "#ffffff"},
                               "selected": {"text": "#ffffff"}, "tabs > 1": {"gap": 2}, "hover:stab": {"opacity": 0}},
                    "children": [{"text": "x", "style": {"padding": 2}}]}"##
            ))
        );
    }

    #[test]
    fn tokens_keyframes_and_motion() {
        let (m, problems) = build(
            r##"<template name="t"><div class="a">x</div></template>"##,
            r##":root { --row-height: 28px; --muted: #888888; --pad: 0.5rem; --alias: var(--muted--foreground) }
               @keyframes pulse { from { opacity: 1; transform: scale(1) } 50%, 80% { opacity: 0.5; transform: translate(3px, 4px) rotate(5deg); background: #123456 } }
               .a { animation: pulse 1.2s infinite ease-in-out; transition: background-color 0.15s ease-out, opacity 0.2s }"##,
        );
        assert_eq!(problems, Vec::<String>::new());
        assert_eq!(m.tokens["row_height"], 28);
        assert_eq!(m.tokens["pad"], "0.5rem");
        assert_eq!(m.tokens["alias"], "$muted.foreground");
        assert_eq!(
            serde_json::to_value(&m.animations).expect("json"),
            json(
                r##"{"pulse": {"from": {"opacity": 1, "scale": 1},
                    "50%, 80%": {"opacity": 0.5, "translate_x": 3, "translate_y": 4, "rotate": 5, "background": "#123456"}}}"##
            )
        );
        assert_eq!(m.json["t"]["animation"], "pulse 1.2s infinite ease-in-out");
        assert_eq!(m.json["t"]["transition"], "background 0.15s ease-out, opacity 0.2s");
        // The compiler accepts what came out.
        let over = style::Overrides::default();
        let theme = gpui_kit::component::ThemeColor::default();
        let props = style::Props::new(&over, &theme, std::path::Path::new(""));
        let mut p = vec![];
        let defs = super::super::Defs { animations: Some(&m.animations), actors: None };
        super::super::compile_with(&m.templates, &defs, &props, &mut p);
        assert_eq!(p, Vec::<String>::new());
    }

    #[test]
    fn animation_play_makes_an_object() {
        let t = one(r##"<div class="a">x</div>"##, r##".a { animation: pulse 2s infinite; -tern-animation-play: hover }"##);
        assert_eq!(t["animation"], json(r##"{"name": "pulse", "duration": 2, "iteration": "infinite", "play": "hover"}"##));
    }

    #[test]
    fn reduced_motion_rules_apply_when_asked() {
        let css = ".a { animation: pulse 1s } @media (prefers-reduced-motion: reduce) { .a { animation: none } }";
        let html = r##"<template name="t"><div class="a">x</div></template>"##;
        let run = |reduce| {
            let mut p = vec![];
            let m = compile(
                &[Source { file: "t.html".into(), text: html.into() }],
                &[Source { file: "t.css".into(), text: css.into() }],
                Media { reduce_motion: reduce },
                &mut p,
            );
            assert_eq!(p, Vec::<String>::new());
            m.json["t"].clone()
        };
        assert_eq!(run(false)["animation"], "pulse 1s");
        assert!(run(true).get("animation").is_none());
    }

    #[test]
    fn fill_and_flags() {
        let t = one(r##"<div class="a">x</div>"##, ".a { width: 100%; height: 100%; white-space: normal }");
        assert_eq!(canon(&t), canon(&json(r##"{"fill": true, "wrap": true, "text": "x"}"##)));
    }

    fn problems_of(html: &str, css: &str) -> Vec<String> {
        build(html, css).1
    }

    #[test]
    fn unsupported_things_are_problems_with_places() {
        let t = |body: &str| format!("<template name=\"t\">\n{body}\n</template>");
        let has = |p: &[String], start: &str| assert!(p.iter().any(|m| m.starts_with(start)), "no \"{start}\" in {p:?}");
        // CSS properties.
        let p = problems_of(
            &t(r##"<div class="a">x</div>"##),
            ".a {\n  float: left;\n  grid: none;\n  cursor: pointer;\n  transform: scale(2);\n  margin: 1px 2px 3px 4px 5px;\n  width: 50%;\n  color: hsl(1, 2%, 3%);\n}",
        );
        has(&p, "t.css:2:3: no property \"float\"");
        has(&p, "t.css:3:3: no property \"grid\"");
        has(&p, "t.css:4:3: no property \"cursor\"");
        has(&p, "t.css:5:3: transform works inside @keyframes only");
        has(&p, "t.css:6:3: margin: one to four lengths");
        has(&p, "t.css:7:3: width: % is not supported here");
        has(&p, "t.css:8:3: color:");
        // Elements and attributes.
        let p = problems_of(&t(r##"<table></table>"##), "");
        assert_eq!(p, ["t.html:2:1: <table> is not an element a template takes (tables and forms are not supported; there is <button>)"]);
        let p = problems_of(&t(r##"<div onclick="x" style="color: red" data-nope="1"></div>"##), "");
        assert_eq!(
            p,
            [
                "t.html:2:6: <div> has no attribute \"onclick\"",
                "t.html:2:18: style=\"…\" is not supported: give the element a class and style it in a stylesheet",
                "t.html:2:37: <div> has no attribute \"data-nope\""
            ]
        );
        let p = problems_of("<script>1</script><div></div><template></template><template name=\"a\"></template>", "");
        has(&p, "t.html:1:1: only <template> and <style> go at the top");
        assert!(p.iter().any(|m| m.contains("<template> needs a name")), "{p:?}");
        assert!(p.iter().any(|m| m.contains("template \"a\" is one element")), "{p:?}");
        let p = problems_of(&t(r##"<div><icon/><button>x</button></div>"##), "");
        assert!(p.iter().any(|m| m.contains("<icon> needs name")), "{p:?}");
        // Selectors and at-rules come from the CSS reader.
        let p = problems_of(&t("<div></div>"), "div::before { x: y }\n.a + .b { x: y }\n@import url(x.css);\n.a { color: red !important }");
        has(&p, "t.css:1:1: pseudo-elements");
        has(&p, "t.css:2:1: sibling combinators");
        has(&p, "t.css:3:1: @import is not supported");
        has(&p, "t.css:4:6: !important");
        // A box's own property in a state.
        let p = problems_of(&t(r##"<div class="a"></div>"##), ".a:hover { flex-direction: row; width: 100% }");
        assert!(p.iter().any(|m| m.contains("flex-direction can't change in a state")), "{p:?}");
        assert!(p.iter().any(|m| m.contains("100% can't change in a state")), "{p:?}");
    }

    #[test]
    fn engine_problems_name_the_markup() {
        let (m, problems) = build(
            "<template name=\"t\">\n<div>\n  <div class=\"a\"></div>\n  <icon name=\"nope-icon\"/>\n</div>\n</template>",
            ".a { padding: 1px }",
        );
        assert_eq!(problems, Vec::<String>::new());
        let over = style::Overrides::default();
        let theme = gpui_kit::component::ThemeColor::default();
        let props = style::Props::new(&over, &theme, std::path::Path::new(""));
        let mut p = vec![];
        super::super::compile(&m.templates, &props, &mut p);
        let located: Vec<String> = p.iter().map(|x| m.locate(x)).collect();
        assert_eq!(located, ["t.html:4:3: icon: no icon \"nope-icon\""], "{p:?}");
    }

    #[test]
    fn two_templates_of_one_name_say_so() {
        let p = problems_of("<template name=\"a\"><div></div></template>\n<template name=\"a\"><div></div></template>", "");
        assert_eq!(p, ["t.html:2:1: template \"a\" is also defined at t.html:1:1"]);
    }

    #[test]
    fn style_in_html_follows_the_stylesheets() {
        let (m, p) =
            build("<style>.a { opacity: 0.3 }</style><template name=\"t\"><div class=\"a\">x</div></template>", ".a { opacity: 0.9 }");
        assert_eq!(p, Vec::<String>::new());
        assert_eq!(m.json["t"]["style"]["opacity"], 0.3);
    }
}

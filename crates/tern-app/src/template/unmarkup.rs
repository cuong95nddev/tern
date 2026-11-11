//! Writes templates (JSON, as `templates.json` holds them) as HTML and CSS,
//! the form `markup` reads: the built-in templates for `reference/`, as a
//! starting point to copy. Reading the result back gives the same templates.

use std::fmt::Write as _;

use serde_json::{Map, Value as Json};

use super::html;
use super::markup::{css_name, jnum, token_ident};
use crate::style::{self, Kind};

/// The HTML of `templates` and the CSS that styles it.
pub fn render(templates: &Map<String, Json>) -> Result<(String, String), String> {
    let (mut html_out, mut css_out) = (String::new(), String::new());
    for (name, t) in templates {
        let Json::Object(root) = t else { return Err(format!("{name}: not a template")) };
        let mut w = Writer { prefix: name.replace(['.', '_'], "-"), count: 0, css: String::new() };
        writeln!(html_out, "<template name=\"{}\">", html::encode(name)).ok();
        w.node(root, 1, &mut html_out).map_err(|e| format!("{name}: {e}"))?;
        html_out.push_str("</template>\n\n");
        css_out.push_str(&w.css);
    }
    Ok((html_out, css_out))
}

struct Writer {
    prefix: String,
    count: usize,
    css: String,
}

fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn is_hole(s: &str) -> bool {
    s.len() > 2
        && s.starts_with('{')
        && s.ends_with('}')
        && s[1..s.len() - 1].chars().all(|c| c.is_ascii_alphabetic() || c == '_' || c.is_ascii_digit())
        && !s[1..].starts_with(|c: char| c.is_ascii_digit())
}

fn num(n: f64) -> String {
    jnum(n).to_string()
}

/// A value as CSS: a clean form when there is one, else a quoted string, which is read as written.
fn val_text(key: &str, kind: Kind, v: &Json) -> Result<String, String> {
    match v {
        Json::Number(n) => {
            let f = n.as_f64().ok_or("a number")?;
            Ok(if kind == Kind::Len { format!("{}px", num(f)) } else { num(f) })
        },
        Json::String(s) => Ok(if is_hole(s) {
            s.clone()
        } else if let Some(p) = s.strip_prefix('$')
            && !p.is_empty()
            && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
            && !p.starts_with(|c: char| c.is_ascii_digit() || c == '.' || c == '_')
            && !p.ends_with(['.', '_'])
            && super::markup::token_path_is_lossless(p)
        {
            format!("var({})", token_ident(p))
        } else if (kind == Kind::Len && (s == "auto" || s.strip_suffix("rem").is_some_and(|n| n.parse::<f64>().is_ok_and(|f| num(f) == n))))
            || (kind == Kind::Color
                && (s == "transparent"
                    || (s.starts_with('#') && s[1..].chars().all(|c| c.is_ascii_hexdigit()) && matches!(s.len(), 4 | 7 | 9))))
        {
            s.clone()
        } else if kind == Kind::Text && key == "background_image_fit" && s.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
            s.replace('_', "-")
        } else if kind == Kind::Text && key == "background_image" && !s.contains(['"', '\\', '{', '$']) {
            format!("url({})", quoted(s))
        } else if kind == Kind::Text
            && s.starts_with(|c: char| c.is_ascii_lowercase())
            && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            s.clone()
        } else {
            quoted(s)
        }),
        other => Err(format!("{key}: {other} is not a value")),
    }
}

/// A style property as CSS declarations.
fn style_decls(map: &Map<String, Json>, out: &mut Vec<String>) -> Result<(), String> {
    for (key, v) in map {
        if key == "element" {
            out.push(format!("-tern-element: {};", quoted(v.as_str().ok_or("element is a name")?)));
            continue;
        }
        let &(_, kind, _) = style::box_prop(key).ok_or_else(|| format!("no style property {key}"))?;
        if key == "italic" {
            match v.as_f64() {
                Some(n) if n != 0. => out.push("font-style: italic;".into()),
                Some(_) => out.push("font-style: normal;".into()),
                None => out.push(format!("{}: {};", css_name(key), val_text(key, kind, v)?)),
            }
            continue;
        }
        out.push(format!("{}: {};", css_name(key), val_text(key, kind, v)?));
    }
    Ok(())
}

/// The selector suffix of a state's condition.
fn cond_selector(key: &str) -> String {
    let key = key.trim();
    if key.contains("||") {
        return format!(":state({key})");
    }
    let mut out = String::new();
    for term in key.split("&&").map(str::trim) {
        let (neg, name) = match term.strip_prefix('!') {
            Some(n) => (true, n.trim()),
            None => (false, term),
        };
        let pseudo = match name {
            "hover" => ":hover".to_owned(),
            "active" => ":active".to_owned(),
            "focused" => ":focus".to_owned(),
            "selected" => ":checked".to_owned(),
            other => format!(":state({other})"),
        };
        if neg {
            write!(out, ":not({pseudo})").ok();
        } else {
            out.push_str(&pseudo);
        }
    }
    out
}

fn text_content(t: &str) -> String {
    let enc = html::encode(t);
    let chars: Vec<char> = enc.chars().collect();
    let mut out = String::new();
    for (i, c) in chars.iter().enumerate() {
        if *c == ' ' && (i == 0 || i == chars.len() - 1 || chars[i - 1] == ' ') {
            out.push_str("&#32;");
        } else {
            out.push(*c);
        }
    }
    out
}

fn flag_attr(name: &str, v: &Json, attrs: &mut Vec<String>) -> Result<(), String> {
    match v {
        Json::Bool(true) => attrs.push(name.to_owned()),
        Json::Bool(false) => attrs.push(format!("{name}=\"false\"")),
        Json::String(s) => attrs.push(format!("{name}=\"{}\"", html::encode(s))),
        other => return Err(format!("{name}: {other} is true, false or a condition")),
    }
    Ok(())
}

fn text_of(key: &str, v: &Json) -> Result<String, String> {
    v.as_str().map(str::to_owned).ok_or_else(|| format!("{key}: text"))
}

fn scalar_text(key: &str, v: &Json) -> Result<String, String> {
    match v {
        Json::Number(n) => Ok(num(n.as_f64().ok_or("a number")?)),
        Json::String(s) => Ok(s.clone()),
        other => Err(format!("{key}: {other} is a number or text")),
    }
}

impl Writer {
    fn node(&mut self, n: &Map<String, Json>, depth: usize, out: &mut String) -> Result<(), String> {
        let mut left = n.clone();
        let mut take = |k: &str| left.shift_remove(k);
        let pad = "  ".repeat(depth);
        let index = self.count;
        self.count += 1;
        let class = format!("{}-{index}", self.prefix);

        let has = |k: &str| n.contains_key(k);
        let tag = if has("button") {
            "button"
        } else if has("switch") {
            "switch"
        } else if has("slot") {
            "slot"
        } else if has("use") {
            "use"
        } else if has("each") {
            "each"
        } else if has("actor") {
            "actor"
        } else if has("ring") {
            "ring"
        } else if has("image") {
            "img"
        } else if has("separator") {
            "separator"
        } else if has("icon") {
            "icon"
        } else if has("action") || has("label") {
            "menu-item"
        } else if has("text") && !has("children") {
            "span"
        } else {
            "div"
        };

        let mut attrs: Vec<String> = vec![];
        let mut decls: Vec<String> = vec![];
        let named = |key: &str, attr: &str, attrs: &mut Vec<String>, take: &mut dyn FnMut(&str) -> Option<Json>| -> Result<(), String> {
            if let Some(v) = take(key) {
                attrs.push(format!("{attr}=\"{}\"", html::encode(&text_of(key, &v)?)));
            }
            Ok(())
        };
        match tag {
            "button" => named("button", "name", &mut attrs, &mut take)?,
            "switch" => named("switch", "name", &mut attrs, &mut take)?,
            "slot" => named("slot", "name", &mut attrs, &mut take)?,
            "actor" => named("actor", "name", &mut attrs, &mut take)?,
            "icon" => named("icon", "name", &mut attrs, &mut take)?,
            "use" => {
                named("use", "template", &mut attrs, &mut take)?;
                named("element", "element", &mut attrs, &mut take)?;
            },
            "each" => named("each", "list", &mut attrs, &mut take)?,
            "ring" => named("ring", "value", &mut attrs, &mut take)?,
            "img" => named("image", "src", &mut attrs, &mut take)?,
            "separator" => _ = take("separator"),
            _ => {},
        }
        if tag == "button" || tag == "menu-item" {
            named("icon", "icon", &mut attrs, &mut take)?;
            named("action", "action", &mut attrs, &mut take)?;
        }
        if tag == "button" {
            named("variant", "variant", &mut attrs, &mut take)?;
            named("size", "size", &mut attrs, &mut take)?;
        }
        if tag == "button" || tag == "switch" || tag == "menu-item" {
            named("tooltip", "tooltip", &mut attrs, &mut take)?;
        }
        if tag == "button" || tag == "switch" || tag == "menu-item" {
            for k in ["selected", "disabled", "loading"] {
                if let Some(v) = take(k) {
                    flag_attr(k, &v, &mut attrs)?;
                }
            }
        }
        for (key, attr) in [
            ("when", "data-when"),
            ("frame", "data-frame"),
            ("element", "data-element"),
            ("part", "data-part"),
            ("kind", "data-kind"),
            ("dock", "data-dock"),
            ("strip", "data-strip"),
            ("group", "data-group"),
            ("empty", "data-empty"),
            ("split", "data-split"),
        ] {
            named(key, attr, &mut attrs, &mut take)?;
        }
        for (key, attr) in [("accepts", "data-accepts"), ("zones", "data-zones")] {
            if let Some(v) = take(key) {
                let list: Vec<String> =
                    v.as_array().ok_or_else(|| format!("{key}: a list"))?.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect();
                attrs.push(format!("{attr}=\"{}\"", html::encode(&list.join(" "))));
            }
        }
        for (key, attr) in [("size", "data-size"), ("min", "data-min"), ("max", "data-max"), ("part_size", "data-part-size")] {
            if let Some(v) = take(key) {
                attrs.push(format!("{attr}=\"{}\"", html::encode(&scalar_text(key, &v)?)));
            }
        }
        for (key, attr) in
            [("fixed", "data-fixed"), ("drag", "data-drag"), ("float", "data-float"), ("window", "data-window"), ("attach", "data-attach")]
        {
            if let Some(v) = take(key) {
                flag_attr(attr, &v, &mut attrs)?;
            }
        }
        for (key, attr) in
            [("handle", "data-handle"), ("overlay", "data-overlay"), ("border_under", "data-border-under"), ("grouped", "data-grouped")]
        {
            match take(key) {
                Some(Json::Bool(true)) => attrs.push(attr.to_owned()),
                Some(Json::Bool(false)) | None => {},
                Some(other) => return Err(format!("{key}: {other}")),
            }
        }
        if let Some(Json::Object(on)) = take("on") {
            for (event, v) in on {
                let t = match &v {
                    Json::String(s) => s.clone(),
                    other => other.to_string(),
                };
                attrs.push(format!("data-on-{event}=\"{}\"", html::encode(&t)));
            }
        }

        // The box's own properties as CSS.
        match take("layout") {
            Some(Json::String(l)) => match l.as_str() {
                "row" | "column" => decls.push(format!("flex-direction: {l};")),
                "wrap" => {
                    decls.push("flex-direction: row;".into());
                    decls.push("flex-wrap: wrap;".into());
                },
                "fit" => attrs.push("data-layout=\"fit\"".into()),
                other => return Err(format!("layout {other}")),
            },
            Some(other) => return Err(format!("layout {other}")),
            None => {},
        }
        if let Some(Json::String(a)) = take("align") {
            let v = match a.as_str() {
                "start" => "flex-start",
                "end" => "flex-end",
                o => o,
            };
            decls.push(format!("align-items: {v};"));
        }
        if let Some(Json::String(j)) = take("justify") {
            let v = match j.as_str() {
                "start" => "flex-start",
                "end" => "flex-end",
                "between" => "space-between",
                "around" => "space-around",
                o => o,
            };
            decls.push(format!("justify-content: {v};"));
        }
        if take("fill") == Some(Json::Bool(true)) {
            decls.push("width: 100%;".into());
            decls.push("height: 100%;".into());
        }
        for (key, prop) in [("grow", "flex-grow"), ("shrink", "flex-shrink"), ("z", "z-index")] {
            if let Some(v) = take(key) {
                decls.push(format!("{prop}: {};", num(v.as_f64().ok_or_else(|| format!("{key}: a number"))?)));
            }
        }
        if let Some(Json::Object(p)) = take("position") {
            decls.push("position: absolute;".into());
            for side in ["top", "right", "bottom", "left"] {
                if let Some(v) = p.get(side) {
                    decls.push(format!("{side}: {};", val_text(side, Kind::Len, v)?));
                }
            }
        }
        if take("clip") == Some(Json::Bool(true)) {
            decls.push("overflow: hidden;".into());
        }
        if take("truncate") == Some(Json::Bool(true)) {
            decls.push("white-space: nowrap;".into());
            decls.push("text-overflow: ellipsis;".into());
        }
        if take("nowrap") == Some(Json::Bool(true)) {
            decls.push("white-space: nowrap;".into());
        }
        if take("wrap") == Some(Json::Bool(true)) {
            decls.push("white-space: normal;".into());
        }
        if let Some(a) = take("animation") {
            animation_decls(&a, &mut decls)?;
        }
        if let Some(t) = take("transition") {
            decls.push(format!("transition: {};", transition_text(&t)?));
        }
        if let Some(Json::Object(s)) = take("style") {
            style_decls(&s, &mut decls)?;
        }
        let states = match take("states") {
            Some(Json::Object(s)) => s,
            _ => Map::new(),
        };

        // What is left in the box: text, children.
        let text = take("text");
        let label = take("label");
        let item = take("item");
        let children = match take("children") {
            Some(Json::Array(c)) => c,
            None => vec![],
            Some(other) => return Err(format!("children {other}")),
        };
        if let Some(k) = left.keys().next() {
            return Err(format!("no HTML for \"{k}\""));
        }

        if !decls.is_empty() {
            writeln!(self.css, ".{class} {{").ok();
            decls.iter().for_each(|d| _ = writeln!(self.css, "  {d}"));
            self.css.push_str("}\n");
        }
        for (cond, props) in &states {
            let Json::Object(props) = props else { return Err(format!("state {cond}")) };
            let mut d = vec![];
            style_decls(props, &mut d)?;
            writeln!(self.css, ".{class}{} {{", cond_selector(cond)).ok();
            d.iter().for_each(|d| _ = writeln!(self.css, "  {d}"));
            self.css.push_str("}\n");
        }
        if !decls.is_empty() || !states.is_empty() {
            attrs.insert(0, format!("class=\"{class}\""));
        }
        let attrs = if attrs.is_empty() { String::new() } else { format!(" {}", attrs.join(" ")) };

        let content_text = match (tag, &text, &label) {
            ("button", _, Some(l)) if children.is_empty() => Some(text_of("label", l)?),
            ("menu-item", _, Some(l)) if children.is_empty() => Some(text_of("label", l)?),
            (_, Some(t), _) => Some(text_of("text", t)?),
            _ => None,
        };
        if (tag == "menu-item" || tag == "button")
            && let Some(l) = &label
            && !children.is_empty()
        {
            // A submenu's name is an attribute.
            let l = html::encode(&text_of("label", l)?);
            return self.finish(out, &pad, tag, &format!("{attrs} label=\"{l}\""), None, &children, None, depth);
        }
        self.finish(out, &pad, tag, &attrs, content_text, &children, item.as_ref(), depth)
    }

    #[allow(clippy::too_many_arguments)]
    fn finish(
        &mut self,
        out: &mut String,
        pad: &str,
        tag: &str,
        attrs: &str,
        text: Option<String>,
        children: &[Json],
        item: Option<&Json>,
        depth: usize,
    ) -> Result<(), String> {
        let leaf = matches!(tag, "img" | "icon" | "slot" | "actor" | "ring" | "separator");
        if let Some(t) = text {
            if !children.is_empty() {
                return Err("text and children together".into());
            }
            writeln!(out, "{pad}<{tag}{attrs}>{}</{tag}>", text_content(&t)).ok();
            return Ok(());
        }
        if tag == "each" {
            writeln!(out, "{pad}<each{attrs}>").ok();
            match item {
                Some(Json::Object(i)) => self.node(i, depth + 1, out)?,
                _ => return Err("each without an item".into()),
            }
            if !children.is_empty() {
                writeln!(out, "{pad}  <after>").ok();
                for c in children {
                    let Json::Object(c) = c else { return Err("a child is a node".into()) };
                    self.node(c, depth + 2, out)?;
                }
                writeln!(out, "{pad}  </after>").ok();
            }
            writeln!(out, "{pad}</each>").ok();
            return Ok(());
        }
        if children.is_empty() {
            if leaf || tag == "use" || tag == "menu-item" || tag == "button" || tag == "switch" {
                writeln!(out, "{pad}<{tag}{attrs}/>").ok();
            } else {
                writeln!(out, "{pad}<{tag}{attrs}></{tag}>").ok();
            }
            return Ok(());
        }
        writeln!(out, "{pad}<{tag}{attrs}>").ok();
        for c in children {
            let Json::Object(c) = c else { return Err("a child is a node".into()) };
            self.node(c, depth + 1, out)?;
        }
        writeln!(out, "{pad}</{tag}>").ok();
        Ok(())
    }
}

fn animation_one(v: &Json) -> Result<(String, Option<String>), String> {
    match v {
        Json::String(s) => Ok((s.clone(), None)),
        Json::Object(o) => {
            let mut words = vec![];
            let secs = |k: &str| o.get(k).and_then(Json::as_f64).map(|n| format!("{}s", num(n)));
            words.extend(o.get("name").and_then(Json::as_str).map(str::to_owned));
            words.extend(secs("duration"));
            words.extend(o.get("easing").and_then(Json::as_str).map(str::to_owned));
            // A delay is the second time: a duration comes before it.
            if let Some(d) = secs("delay") {
                if o.get("duration").is_none() {
                    words.push("1s".into());
                }
                words.push(d);
            }
            match o.get("iteration") {
                Some(Json::String(s)) => words.push(s.clone()),
                Some(n) => words.extend(n.as_f64().map(num)),
                None => {},
            }
            words.extend(o.get("direction").and_then(Json::as_str).map(str::to_owned));
            words.extend(o.get("fill").and_then(Json::as_str).map(str::to_owned));
            Ok((words.join(" "), o.get("play").and_then(Json::as_str).map(str::to_owned)))
        },
        other => Err(format!("animation {other}")),
    }
}

fn animation_decls(v: &Json, out: &mut Vec<String>) -> Result<(), String> {
    let list = match v {
        Json::Array(l) => l.iter().map(animation_one).collect::<Result<Vec<_>, _>>()?,
        one => vec![animation_one(one)?],
    };
    out.push(format!("animation: {};", list.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>().join(", ")));
    if let Some(p) = list.iter().find_map(|(_, p)| p.as_ref()) {
        out.push(format!("-tern-animation-play: {};", quoted(p)));
    }
    Ok(())
}

fn transition_item(v: &Json) -> Result<String, String> {
    match v {
        Json::String(s) => {
            let mut words: Vec<String> = s.split_whitespace().map(str::to_owned).collect();
            if let Some(first) = words.first_mut()
                && first != "all"
            {
                *first = css_name(first);
            }
            Ok(words.join(" "))
        },
        Json::Object(o) => {
            let prop = o.get("property").and_then(Json::as_str).unwrap_or("all");
            let mut words = vec![if prop == "all" { prop.to_owned() } else { css_name(prop) }];
            if let Some(d) = o.get("duration").and_then(Json::as_f64) {
                words.push(format!("{}s", num(d)));
            }
            words.extend(o.get("easing").and_then(Json::as_str).map(str::to_owned));
            if let Some(d) = o.get("delay").and_then(Json::as_f64) {
                if o.get("duration").is_none() {
                    words.push("0.2s".into());
                }
                words.push(format!("{}s", num(d)));
            }
            Ok(words.join(" "))
        },
        other => Err(format!("transition {other}")),
    }
}

fn transition_text(v: &Json) -> Result<String, String> {
    match v {
        Json::String(s) => {
            let items: Result<Vec<String>, String> = s.split(',').map(|i| transition_item(&Json::String(i.trim().to_owned()))).collect();
            Ok(items?.join(", "))
        },
        Json::Array(l) => Ok(l.iter().map(transition_item).collect::<Result<Vec<_>, _>>()?.join(", ")),
        one => transition_item(one),
    }
}

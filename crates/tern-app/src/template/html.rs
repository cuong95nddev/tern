//! The small HTML a template file is written in (see `markup`): elements with
//! attributes, text, comments. It is not a browser's parser: a tag closes only
//! as written (`<slot name="x"/>`, or a closing tag), an unknown tag is the
//! compiler's to refuse, and every element and attribute knows its line and
//! column so a problem can name it.

/// A place in a file: both count from 1, the column in characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Pos {
    pub line: u32,
    pub col: u32,
}

#[derive(Debug, Clone)]
pub struct Attr {
    pub name: String,
    /// `None` for an attribute written without a value (`handle`).
    pub value: Option<String>,
    pub pos: Pos,
}

#[derive(Debug, Clone)]
pub enum Item {
    Element(Element),
    /// Text with its whitespace collapsed, entities not yet read.
    Text(String, Pos),
    /// The raw text of a `<style>` element.
    Raw(String, Pos),
}

#[derive(Debug, Clone)]
pub struct Element {
    pub tag: String,
    pub attrs: Vec<Attr>,
    pub children: Vec<Item>,
    pub pos: Pos,
}

impl Element {
    pub fn attr(&self, name: &str) -> Option<&Attr> {
        self.attrs.iter().find(|a| a.name == name)
    }

    #[cfg(test)]
    pub fn classes(&self) -> impl Iterator<Item = &str> {
        self.attr("class").and_then(|a| a.value.as_deref()).unwrap_or("").split_whitespace()
    }
}

/// Tags that never have content or a closing tag.
const VOID: &[&str] = &["img", "br", "hr", "input", "meta", "link", "wbr"];

struct Cursor<'a> {
    text: &'a str,
    at: usize,
    pos: Pos,
}

impl<'a> Cursor<'a> {
    fn peek(&self) -> Option<char> {
        self.text[self.at..].chars().next()
    }

    fn rest(&self) -> &'a str {
        &self.text[self.at..]
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.at += c.len_utf8();
        if c == '\n' {
            self.pos.line += 1;
            self.pos.col = 1;
        } else {
            self.pos.col += 1;
        }
        Some(c)
    }

    fn eat(&mut self, s: &str) -> bool {
        if self.rest().starts_with(s) {
            s.chars().for_each(|_| _ = self.bump());
            true
        } else {
            false
        }
    }

    fn skip_space(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.bump();
        }
    }

    fn name(&mut self) -> String {
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | ':' | '.') {
                out.push(c);
                self.bump();
            } else {
                break;
            }
        }
        out
    }
}

/// Reads `text` as the children of a document. What is wrong goes to
/// `problems` as `file:line:col: …`; the rest still reads.
pub fn parse(text: &str, file: &str, problems: &mut Vec<String>) -> Vec<Item> {
    let mut c = Cursor { text, at: 0, pos: Pos { line: 1, col: 1 } };
    // The open elements, the document being the first.
    let mut stack: Vec<Element> = vec![Element { tag: String::new(), attrs: vec![], children: vec![], pos: c.pos }];
    let say = |pos: Pos, msg: String, problems: &mut Vec<String>| problems.push(format!("{file}:{}:{}: {msg}", pos.line, pos.col));
    while c.peek().is_some() {
        if c.eat("<!--") {
            let start = c.pos;
            loop {
                if c.eat("-->") {
                    break;
                }
                if c.bump().is_none() {
                    say(start, "comment is never closed".into(), problems);
                    break;
                }
            }
        } else if c.rest().starts_with("<!") || c.rest().starts_with("<?") {
            while let Some(ch) = c.bump() {
                if ch == '>' {
                    break;
                }
            }
        } else if c.rest().starts_with("</") {
            let pos = c.pos;
            c.eat("</");
            let name = c.name().to_ascii_lowercase();
            while let Some(ch) = c.bump() {
                if ch == '>' {
                    break;
                }
            }
            match stack.iter().rposition(|e| e.tag == name).filter(|i| *i > 0) {
                Some(i) => {
                    while stack.len() > i + 1 {
                        let open = stack.pop().expect("an open element");
                        say(open.pos, format!("<{}> is never closed (</{name}> at {}:{} ends it)", open.tag, pos.line, pos.col), problems);
                        stack.last_mut().expect("a parent").children.push(Item::Element(open));
                    }
                    let done = stack.pop().expect("the closed element");
                    stack.last_mut().expect("a parent").children.push(Item::Element(done));
                },
                None => say(pos, format!("</{name}> closes nothing"), problems),
            }
        } else if c.peek() == Some('<') && c.rest()[1..].chars().next().is_some_and(|ch| ch.is_ascii_alphabetic()) {
            let pos = c.pos;
            c.bump();
            let tag = c.name().to_ascii_lowercase();
            let mut el = Element { tag: tag.clone(), attrs: vec![], children: vec![], pos };
            let mut self_closing = false;
            loop {
                c.skip_space();
                match c.peek() {
                    None => {
                        say(pos, format!("<{tag}> is never finished"), problems);
                        break;
                    },
                    Some('>') => {
                        c.bump();
                        break;
                    },
                    Some('/') => {
                        c.bump();
                        if c.peek() == Some('>') {
                            c.bump();
                            self_closing = true;
                            break;
                        }
                    },
                    Some(_) => {
                        let apos = c.pos;
                        let name = c.name().to_ascii_lowercase();
                        if name.is_empty() {
                            say(apos, format!("unexpected \"{}\" in <{tag}>", c.peek().unwrap_or(' ')), problems);
                            c.bump();
                            continue;
                        }
                        c.skip_space();
                        let value = if c.peek() == Some('=') {
                            c.bump();
                            c.skip_space();
                            Some(match c.peek() {
                                Some(q @ ('"' | '\'')) => {
                                    c.bump();
                                    let mut v = String::new();
                                    loop {
                                        match c.bump() {
                                            Some(ch) if ch == q => break,
                                            Some(ch) => v.push(ch),
                                            None => {
                                                say(apos, format!("attribute \"{name}\" is never closed"), problems);
                                                break;
                                            },
                                        }
                                    }
                                    decode(&v)
                                },
                                _ => {
                                    let mut v = String::new();
                                    while let Some(ch) = c.peek() {
                                        if ch.is_whitespace() || ch == '>' || (ch == '/' && c.rest().starts_with("/>")) {
                                            break;
                                        }
                                        v.push(ch);
                                        c.bump();
                                    }
                                    decode(&v)
                                },
                            })
                        } else {
                            None
                        };
                        if el.attr(&name).is_some() {
                            say(apos, format!("attribute \"{name}\" twice on <{tag}>"), problems);
                        } else {
                            el.attrs.push(Attr { name, value, pos: apos });
                        }
                    },
                }
            }
            if tag == "style" || tag == "script" {
                // Its text is not markup: up to the closing tag.
                let close = format!("</{tag}");
                let start = c.pos;
                let end = c.rest().to_ascii_lowercase().find(&close).unwrap_or(c.rest().len());
                let raw = c.rest()[..end].to_owned();
                raw.chars().for_each(|_| _ = c.bump());
                if c.rest().starts_with("</") {
                    while let Some(ch) = c.bump() {
                        if ch == '>' {
                            break;
                        }
                    }
                } else if !self_closing {
                    say(pos, format!("<{tag}> is never closed"), problems);
                }
                el.children.push(Item::Raw(raw, start));
                stack.last_mut().expect("a parent").children.push(Item::Element(el));
            } else if self_closing || VOID.contains(&tag.as_str()) {
                stack.last_mut().expect("a parent").children.push(Item::Element(el));
            } else {
                stack.push(el);
            }
        } else {
            let pos = c.pos;
            let mut raw = String::new();
            while let Some(ch) = c.peek() {
                if ch == '<' && raw_ends_text(c.rest()) {
                    break;
                }
                raw.push(ch);
                c.bump();
            }
            if raw.is_empty() {
                // A lone `<`.
                raw.push('<');
                c.bump();
            }
            let collapsed = collapse(&raw);
            if !collapsed.trim().is_empty() {
                // Its place is where the words start.
                let mut at = pos;
                for ch in raw.chars().take_while(|c| c.is_whitespace()) {
                    if ch == '\n' {
                        at.line += 1;
                        at.col = 1;
                    } else {
                        at.col += 1;
                    }
                }
                stack.last_mut().expect("a parent").children.push(Item::Text(collapsed.trim().to_owned(), at));
            }
        }
    }
    while stack.len() > 1 {
        let open = stack.pop().expect("an open element");
        say(open.pos, format!("<{}> is never closed", open.tag), problems);
        stack.last_mut().expect("a parent").children.push(Item::Element(open));
    }
    stack.pop().map(|d| d.children).unwrap_or_default()
}

/// Whether the `<` at the start of `rest` starts markup (a tag, a closing tag, a comment).
fn raw_ends_text(rest: &str) -> bool {
    let mut it = rest.chars();
    it.next();
    match it.next() {
        Some(c) => c.is_ascii_alphabetic() || c == '/' || c == '!' || c == '?',
        None => false,
    }
}

/// Runs of whitespace as one space.
fn collapse(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            space = true;
        } else {
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            out.push(c);
        }
    }
    if space && !out.is_empty() {
        out.push(' ');
    }
    out
}

/// `text` with its `&amp;`-style entities read; an unknown one stays as written.
pub fn decode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest.find(';').filter(|e| *e <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let name = &rest[1..end];
        let ch = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some('\u{a0}'),
            _ => name
                .strip_prefix('#')
                .and_then(|n| match n.strip_prefix(['x', 'X']) {
                    Some(h) => u32::from_str_radix(h, 16).ok(),
                    None => n.parse().ok(),
                })
                .and_then(char::from_u32),
        };
        match ch {
            Some(ch) => {
                out.push(ch);
                rest = &rest[end + 1..];
            },
            None => {
                out.push('&');
                rest = &rest[1..];
            },
        }
    }
    out.push_str(rest);
    out
}

/// `text` as element content or an attribute value.
pub fn encode(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &str) -> (Vec<Item>, Vec<String>) {
        let mut problems = vec![];
        let items = parse(text, "t.html", &mut problems);
        (items, problems)
    }

    fn el(item: &Item) -> &Element {
        match item {
            Item::Element(e) => e,
            other => panic!("not an element: {other:?}"),
        }
    }

    #[test]
    fn reads_elements_attributes_and_text() {
        let (items, problems) = read("<div class=\"a b\" data-x='1' handle>\n  Hello  {name}\n  <icon name=\"x\"/>\n</div>");
        assert_eq!(problems, Vec::<String>::new());
        let d = el(&items[0]);
        assert_eq!(d.tag, "div");
        assert_eq!(d.classes().collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(d.attr("data-x").and_then(|a| a.value.as_deref()), Some("1"));
        assert_eq!(d.attr("handle").map(|a| a.value.clone()), Some(None));
        assert!(matches!(&d.children[0], Item::Text(t, p) if t == "Hello {name}" && p.line == 2));
        assert_eq!(el(&d.children[1]).tag, "icon");
        assert_eq!(el(&d.children[1]).pos, Pos { line: 3, col: 3 });
    }

    #[test]
    fn says_where_markup_is_wrong() {
        let (_, problems) = read("<div>\n<span>\n</div>\n</p>");
        assert_eq!(problems, ["t.html:2:1: <span> is never closed (</div> at 3:1 ends it)", "t.html:4:1: </p> closes nothing"]);
        let (_, problems) = read("<div class=\"x\" class=\"y\"></div>");
        assert_eq!(problems, ["t.html:1:16: attribute \"class\" twice on <div>"]);
        let (_, problems) = read("<div>");
        assert_eq!(problems, ["t.html:1:1: <div> is never closed"]);
    }

    #[test]
    fn entities_and_style() {
        assert_eq!(decode("a &amp;&amp; b &lt; &#32;c &unknown; R&D"), "a && b <  c &unknown; R&D");
        assert_eq!(encode("a & <b> \"c\""), "a &amp; &lt;b&gt; &quot;c&quot;");
        let (items, problems) = read("<style>a > b { x: y }</style><p>t</p>");
        assert_eq!(problems, Vec::<String>::new());
        assert!(matches!(&el(&items[0]).children[0], Item::Raw(t, _) if t == "a > b { x: y }"));
        assert_eq!(el(&items[1]).tag, "p");
    }
}

//! The CSS a template file is styled with (see `markup`): rules with
//! selectors and declarations, `@keyframes`, `@media (prefers-reduced-motion)`
//! and `:root` tokens. `cssparser` reads it; this keeps what the compiler needs
//! (each declaration's place, each selector's specificity) and matches
//! selectors against the elements of a template.

use cssparser::{
    AtRuleParser, CowRcStr, DeclarationParser, ParseError, ParseErrorKind, Parser, ParserState, QualifiedRuleParser, RuleBodyItemParser,
    RuleBodyParser, SourceLocation, StyleSheetParser, Token,
};

use super::html::Pos;

/// A value's parts, as written.
#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Num(f64),
    Dim(f64, String),
    Pct(f64),
    Ident(String),
    Str(String),
    /// `#rrggbb`, without the `#`.
    Hash(String),
    Func(String, Vec<Tok>),
    /// `{name}`: a value the code gives.
    Hole(String),
    Comma,
    Slash,
}

#[derive(Debug, Clone)]
pub struct Decl {
    pub name: String,
    pub value: Vec<Tok>,
    pub pos: Pos,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Simple {
    Tag(String),
    Class(String),
    Id(String),
    /// `[name]` or `[name=value]`.
    Attr(String, Option<String>),
}

/// A run of simple selectors on one element, and the state tests (`:hover`) it adds.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Compound {
    pub simples: Vec<Simple>,
    /// `:not(.x)`.
    pub nots: Vec<Simple>,
    /// Template conditions: the test's text, and whether it is negated.
    pub states: Vec<(String, bool)>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Comb {
    Descendant,
    Child,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Selector {
    /// Each compound with the combinator that joins it to the one before.
    pub parts: Vec<(Comb, Compound)>,
    pub specificity: (u32, u32, u32),
}

impl Selector {
    /// The condition the last compound's state tests make (`hover && !active`), if any.
    pub fn condition(&self) -> Option<String> {
        let states = &self.parts.last()?.1.states;
        if states.is_empty() {
            return None;
        }
        let terms: Vec<String> = states.iter().map(|(t, neg)| if *neg { format!("!{t}") } else { t.clone() }).collect();
        Some(terms.join(" && "))
    }
}

#[derive(Debug, Clone)]
pub struct Rule {
    pub selectors: Vec<Selector>,
    pub decls: Vec<Decl>,
}

#[derive(Debug, Clone)]
pub struct Frames {
    pub name: String,
    pub stops: Vec<(String, Vec<Decl>)>,
    pub pos: Pos,
}

#[derive(Debug, Clone, Default)]
pub struct Sheet {
    pub rules: Vec<Rule>,
    pub frames: Vec<Frames>,
    /// `--name: value` on `:root`.
    pub tokens: Vec<Decl>,
}

/// What `@media (prefers-reduced-motion)` asks about.
#[derive(Debug, Clone, Copy, Default)]
pub struct Media {
    pub reduce_motion: bool,
}

fn here(loc: SourceLocation, line_offset: u32) -> Pos {
    Pos { line: loc.line + 1 + line_offset, col: loc.column }
}

fn describe(e: &ParseError<String>) -> String {
    match &e.kind {
        ParseErrorKind::Custom(m) => m.clone(),
        ParseErrorKind::Basic(k) => k.to_string(),
    }
}

fn number(v: f32) -> f64 {
    format!("{v}").parse().unwrap_or(f64::from(v))
}

/// The parts of a declaration's value, up to its end.
fn tokens<'i>(input: &mut Parser<'i>, important: &mut bool) -> Result<Vec<Tok>, ParseError<String>> {
    let mut out = vec![];
    while let Ok(t) = input.next().cloned() {
        out.push(match t {
            Token::Ident(s) => Tok::Ident(s.to_string()),
            Token::QuotedString(s) => Tok::Str(s.to_string()),
            Token::Hash(s) | Token::IDHash(s) => Tok::Hash(s.to_string()),
            Token::Number { value, .. } => Tok::Num(number(value)),
            Token::Dimension { value, unit, .. } => Tok::Dim(number(value), unit.to_ascii_lowercase()),
            Token::Percentage { unit_value, .. } => Tok::Pct(number(unit_value * 100.)),
            Token::Comma => Tok::Comma,
            Token::Delim('/') => Tok::Slash,
            Token::Delim('!') => {
                input.expect_ident_matching("important")?;
                *important = true;
                continue;
            },
            Token::UnquotedUrl(u) => Tok::Func("url".into(), vec![Tok::Str(u.to_string())]),
            Token::Function(name) => {
                let name = name.to_ascii_lowercase();
                let args = input.parse_nested_block(|i| tokens(i, &mut false))?;
                Tok::Func(name, args)
            },
            Token::CurlyBracketBlock => {
                let name = input.parse_nested_block(|i| {
                    let n = i.expect_ident()?.to_string();
                    i.expect_exhausted()?;
                    Ok::<_, ParseError<String>>(n)
                });
                match name {
                    Ok(n) => Tok::Hole(n),
                    Err(_) => return Err(ParseError::custom("{…} holds a name: {name}".to_owned())),
                }
            },
            _ => return Err(ParseError::custom("unexpected token in a value".to_owned())),
        });
    }
    Ok(out)
}

struct Decls {
    line_offset: u32,
    out: Vec<Decl>,
    problems: Vec<String>,
    file: String,
}

impl<'i> DeclarationParser<'i> for Decls {
    type Declaration = ();
    type Error = String;

    fn parse_value(&mut self, name: CowRcStr<'i>, input: &mut Parser<'i>, start: &ParserState) -> Result<(), ParseError<String>> {
        let pos = here(start.source_location(), self.line_offset);
        let mut important = false;
        let value = tokens(input, &mut important)?;
        if important {
            self.problems
                .push(format!("{}:{}:{}: !important is not supported; order and specificity decide", self.file, pos.line, pos.col));
        }
        let name = if name.starts_with("--") { name.to_string() } else { name.to_ascii_lowercase() };
        self.out.push(Decl { name, value, pos });
        Ok(())
    }
}

impl<'i> AtRuleParser<'i> for Decls {
    type Prelude = ();
    type AtRule = ();
    type Error = String;
}

impl<'i> QualifiedRuleParser<'i> for Decls {
    type Prelude = ();
    type QualifiedRule = ();
    type Error = String;
}

impl<'i> RuleBodyItemParser<'i, (), String> for Decls {
    fn parse_declarations(&self) -> bool {
        true
    }

    fn parse_qualified(&self) -> bool {
        false
    }
}

fn block<'i>(input: &mut Parser<'i>, line_offset: u32, file: &str, problems: &mut Vec<String>) -> Vec<Decl> {
    let mut d = Decls { line_offset, out: vec![], problems: vec![], file: file.to_owned() };
    let mut errs = vec![];
    for r in RuleBodyParser::new(input, &mut d) {
        if let Err((e, _, loc)) = r {
            errs.push((describe(&e), loc));
        }
    }
    for (m, loc) in errs {
        let p = here(loc, line_offset);
        d.problems.push(format!("{file}:{}:{}: {m}", p.line, p.col));
    }
    problems.append(&mut d.problems);
    d.out
}

enum At {
    Frames(String),
    Media(bool),
}

struct Top {
    file: String,
    line_offset: u32,
    media: Media,
    sheet: Sheet,
    problems: Vec<String>,
}

impl Top {
    fn new(file: &str, line_offset: u32, media: Media) -> Self {
        Top { file: file.to_owned(), line_offset, media, sheet: Sheet::default(), problems: vec![] }
    }
}

impl<'i> AtRuleParser<'i> for Top {
    type Prelude = At;
    type AtRule = ();
    type Error = String;

    fn parse_prelude(&mut self, name: CowRcStr<'i>, input: &mut Parser<'i>) -> Result<At, ParseError<String>> {
        match name.to_ascii_lowercase().as_str() {
            "keyframes" => {
                let n = input.expect_ident_or_string()?.to_string();
                input.expect_exhausted()?;
                Ok(At::Frames(n))
            },
            "media" => {
                let start = input.position();
                while input.next().is_ok() {}
                let text: String = input.slice_from(start).split_whitespace().collect::<Vec<_>>().join(" ").to_ascii_lowercase();
                let text = text.replace("( ", "(").replace(" )", ")").replace(" :", ":");
                match text.as_str() {
                    "(prefers-reduced-motion: reduce)" | "(prefers-reduced-motion)" => Ok(At::Media(self.media.reduce_motion)),
                    "(prefers-reduced-motion: no-preference)" => Ok(At::Media(!self.media.reduce_motion)),
                    _ => Err(ParseError::custom(format!(
                        "@media {text}: only (prefers-reduced-motion: reduce) or (prefers-reduced-motion: no-preference)"
                    ))),
                }
            },
            other => {
                Err(ParseError::custom(format!("@{other} is not supported (there are @keyframes and @media (prefers-reduced-motion))")))
            },
        }
    }

    fn parse_block(&mut self, prelude: At, start: &ParserState, input: &mut Parser<'i>) -> Result<(), ParseError<String>> {
        let pos = here(start.source_location(), self.line_offset);
        match prelude {
            At::Frames(name) => {
                let mut k = Keys { line_offset: self.line_offset, file: self.file.clone(), stops: vec![], problems: vec![] };
                let mut errs = vec![];
                for r in StyleSheetParser::new(input, &mut k) {
                    if let Err((e, _, loc)) = r {
                        errs.push((describe(&e), loc));
                    }
                }
                for (m, loc) in errs {
                    let p = here(loc, self.line_offset);
                    k.problems.push(format!("{}:{}:{}: {m}", self.file, p.line, p.col));
                }
                self.problems.append(&mut k.problems);
                self.sheet.frames.push(Frames { name, stops: k.stops, pos });
            },
            At::Media(active) => {
                let mut inner = Top::new(&self.file, self.line_offset, self.media);
                let mut errs = vec![];
                for r in StyleSheetParser::new(input, &mut inner) {
                    if let Err((e, _, loc)) = r {
                        errs.push((describe(&e), loc));
                    }
                }
                for (m, loc) in errs {
                    let p = here(loc, self.line_offset);
                    inner.problems.push(format!("{}:{}:{}: {m}", self.file, p.line, p.col));
                }
                self.problems.append(&mut inner.problems);
                if active {
                    self.sheet.rules.append(&mut inner.sheet.rules);
                    self.sheet.frames.append(&mut inner.sheet.frames);
                    self.sheet.tokens.append(&mut inner.sheet.tokens);
                }
            },
        }
        Ok(())
    }
}

/// What a qualified rule's prelude is: `:root`, or selectors.
enum Head {
    Root,
    Selectors(Vec<Selector>),
}

impl<'i> QualifiedRuleParser<'i> for Top {
    type Prelude = Head;
    type QualifiedRule = ();
    type Error = String;

    fn parse_prelude(&mut self, input: &mut Parser<'i>) -> Result<Head, ParseError<String>> {
        let start = input.position();
        if input
            .try_parse(|i| {
                i.expect_colon()?;
                i.expect_ident_matching("root")?;
                i.expect_exhausted()
            })
            .is_ok()
        {
            return Ok(Head::Root);
        }
        let mut out = vec![];
        let _ = start;
        loop {
            out.push(selector(input)?);
            if input.is_exhausted() {
                break;
            }
        }
        Ok(Head::Selectors(out))
    }

    fn parse_block(&mut self, prelude: Head, _: &ParserState, input: &mut Parser<'i>) -> Result<(), ParseError<String>> {
        let decls = block(input, self.line_offset, &self.file, &mut self.problems);
        match prelude {
            Head::Root => {
                for d in decls {
                    if d.name.starts_with("--") {
                        self.sheet.tokens.push(d);
                    } else {
                        self.problems.push(format!(
                            "{}:{}:{}: :root takes tokens (--name: value), not \"{}\"",
                            self.file, d.pos.line, d.pos.col, d.name
                        ));
                    }
                }
            },
            Head::Selectors(selectors) => {
                for d in &decls {
                    if d.name.starts_with("--") {
                        self.problems.push(format!("{}:{}:{}: tokens (--name) go in :root", self.file, d.pos.line, d.pos.col));
                    }
                }
                let decls = decls.into_iter().filter(|d| !d.name.starts_with("--")).collect();
                self.sheet.rules.push(Rule { selectors, decls });
            },
        }
        Ok(())
    }
}

struct Keys {
    line_offset: u32,
    file: String,
    stops: Vec<(String, Vec<Decl>)>,
    problems: Vec<String>,
}

impl<'i> AtRuleParser<'i> for Keys {
    type Prelude = ();
    type AtRule = ();
    type Error = String;
}

impl<'i> QualifiedRuleParser<'i> for Keys {
    type Prelude = String;
    type QualifiedRule = ();
    type Error = String;

    fn parse_prelude(&mut self, input: &mut Parser<'i>) -> Result<String, ParseError<String>> {
        let start = input.position();
        while input.next().is_ok() {}
        Ok(input.slice_from(start).split_whitespace().collect::<Vec<_>>().join(" "))
    }

    fn parse_block(&mut self, key: String, _: &ParserState, input: &mut Parser<'i>) -> Result<(), ParseError<String>> {
        let decls = block(input, self.line_offset, &self.file, &mut self.problems);
        self.stops.push((key, decls));
        Ok(())
    }
}

/// One selector, up to a comma or the end.
fn selector<'i>(input: &mut Parser<'i>) -> Result<Selector, ParseError<String>> {
    let mut parts: Vec<(Comb, Compound)> = vec![];
    let mut cur = Compound::default();
    let mut started = false;
    let mut comb = Comb::Descendant;
    let mut pending_space = false;
    let fail = |_: &Parser<'i>, m: &str| ParseError::custom(m.to_owned());
    while let Ok(t) = input.next_including_whitespace().cloned() {
        match t {
            Token::Comma => break,
            Token::WhiteSpace(_) => {
                if started {
                    pending_space = true;
                }
                continue;
            },
            Token::Delim('>') => {
                if !started {
                    return Err(fail(input, "a selector can't start with >"));
                }
                parts.push((comb, std::mem::take(&mut cur)));
                started = false;
                comb = Comb::Child;
                pending_space = false;
                continue;
            },
            Token::Delim('+' | '~') => return Err(fail(input, "sibling combinators (+, ~) are not supported")),
            _ => {},
        }
        if pending_space {
            parts.push((comb, std::mem::take(&mut cur)));
            comb = Comb::Descendant;
            pending_space = false;
        }
        started = true;
        match t {
            Token::Ident(n) => {
                if !cur.simples.is_empty() || !cur.states.is_empty() {
                    return Err(fail(input, "a tag goes first in a selector"));
                }
                cur.simples.push(Simple::Tag(n.to_ascii_lowercase()));
            },
            Token::Delim('*') => {},
            Token::Delim('.') => cur.simples.push(Simple::Class(input.expect_ident()?.to_string())),
            Token::IDHash(n) => cur.simples.push(Simple::Id(n.to_string())),
            Token::SquareBracketBlock => {
                let s = input.parse_nested_block(|i| attribute(i))?;
                cur.simples.push(s);
            },
            Token::Colon => {
                let t = input.next_including_whitespace()?.clone();
                match t {
                    Token::Ident(n) => match n.to_ascii_lowercase().as_str() {
                        "hover" => cur.states.push(("hover".into(), false)),
                        "active" => cur.states.push(("active".into(), false)),
                        "focus" => cur.states.push(("focused".into(), false)),
                        "checked" => cur.states.push(("selected".into(), false)),
                        "root" => return Err(fail(input, ":root stands alone")),
                        other => {
                            return Err(fail(
                                input,
                                &format!(":{other} is not supported (there are :hover, :active, :focus, :checked, :not(), :state())"),
                            ));
                        },
                    },
                    Token::Function(n) => match n.to_ascii_lowercase().as_str() {
                        "state" => {
                            let text = input.parse_nested_block(|i| {
                                let s = i.position();
                                while i.next_including_whitespace().is_ok() {}
                                Ok::<_, ParseError<String>>(i.slice_from(s).trim().to_owned())
                            })?;
                            if text.is_empty() {
                                return Err(fail(input, ":state() needs a condition"));
                            }
                            cur.states.push((text, false));
                        },
                        "not" => {
                            let inner = input.parse_nested_block(|i| not_arg(i))?;
                            match inner {
                                Neg::State(s) => cur.states.push((s, true)),
                                Neg::Simple(s) => cur.nots.push(s),
                            }
                        },
                        other => return Err(fail(input, &format!(":{other}() is not supported (there are :not() and :state())"))),
                    },
                    Token::Colon => return Err(fail(input, "pseudo-elements (::x) are not supported")),
                    _ => return Err(fail(input, "expected a pseudo-class after :")),
                }
            },
            _ => return Err(fail(input, "unexpected token in a selector")),
        }
    }
    if !started {
        return Err(fail(input, "an empty selector"));
    }
    parts.push((comb, cur));
    // State tests are on the element the rule is for: the last compound.
    if parts[..parts.len() - 1].iter().any(|(_, c)| !c.states.is_empty()) {
        return Err(fail(input, "state pseudo-classes (:hover, :state()) go on the last part of a selector"));
    }
    if let Some(c) = parts.last()
        && c.1.states.len() > 1
        && c.1.states.iter().any(|(t, _)| t.contains("||"))
    {
        return Err(fail(input, "a condition with || stands alone in :state()"));
    }
    let mut spec = (0, 0, 0);
    for (_, c) in &parts {
        for s in c.simples.iter().chain(&c.nots) {
            match s {
                Simple::Id(_) => spec.0 += 1,
                Simple::Class(_) | Simple::Attr(..) => spec.1 += 1,
                Simple::Tag(_) => spec.2 += 1,
            }
        }
        spec.1 += c.states.len() as u32;
    }
    Ok(Selector { parts, specificity: spec })
}

fn attribute<'i>(i: &mut Parser<'i>) -> Result<Simple, ParseError<String>> {
    let name = i.expect_ident()?.to_ascii_lowercase();
    if i.is_exhausted() {
        return Ok(Simple::Attr(name, None));
    }
    i.expect_delim('=')?;
    let v = match i.next()?.clone() {
        Token::Ident(s) | Token::QuotedString(s) => s.to_string(),
        _ => return Err(ParseError::custom("an attribute's value is a name or a string".to_owned())),
    };
    i.expect_exhausted()?;
    Ok(Simple::Attr(name, Some(v)))
}

enum Neg {
    State(String),
    Simple(Simple),
}

fn not_arg<'i>(i: &mut Parser<'i>) -> Result<Neg, ParseError<String>> {
    let t = i.next()?.clone();
    let out = match t {
        Token::Colon => match i.next()?.clone() {
            Token::Ident(n) => match n.to_ascii_lowercase().as_str() {
                "hover" => Neg::State("hover".into()),
                "active" => Neg::State("active".into()),
                "focus" => Neg::State("focused".into()),
                "checked" => Neg::State("selected".into()),
                _ => {
                    return Err(ParseError::custom(
                        ":not() takes :hover, :active, :focus, :checked, :state(), or a class, id or tag".to_owned(),
                    ));
                },
            },
            Token::Function(n) if n.eq_ignore_ascii_case("state") => {
                let text = i.parse_nested_block(|j| {
                    let s = j.position();
                    while j.next_including_whitespace().is_ok() {}
                    Ok::<_, ParseError<String>>(j.slice_from(s).trim().to_owned())
                })?;
                if text.contains("||") || text.contains("&&") || text.contains(' ') && !text.contains("==") && !text.contains("!=") {
                    return Err(ParseError::custom("negate a single test in :not(:state())".to_owned()));
                }
                Neg::State(text)
            },
            _ => return Err(ParseError::custom(":not() takes a pseudo-class, class, id or tag".to_owned())),
        },
        Token::Delim('.') => Neg::Simple(Simple::Class(i.expect_ident()?.to_string())),
        Token::IDHash(n) => Neg::Simple(Simple::Id(n.to_string())),
        Token::Ident(n) => Neg::Simple(Simple::Tag(n.to_ascii_lowercase())),
        Token::SquareBracketBlock => Neg::Simple(i.parse_nested_block(|j| attribute(j))?),
        _ => return Err(ParseError::custom(":not() takes a pseudo-class, class, id or tag".to_owned())),
    };
    i.expect_exhausted()?;
    Ok(out)
}

/// Reads `text` (a CSS file, or the content of a `<style>` that starts
/// `line_offset` lines down its file). Problems go to `problems`.
pub fn parse(text: &str, file: &str, line_offset: u32, media: Media, problems: &mut Vec<String>) -> Sheet {
    let mut parser = Parser::new(text);
    let mut top = Top::new(file, line_offset, media);
    let mut errs = vec![];
    for r in StyleSheetParser::new(&mut parser, &mut top) {
        if let Err((e, _, loc)) = r {
            errs.push((describe(&e), loc));
        }
    }
    for (m, loc) in errs {
        let p = here(loc, line_offset);
        top.problems.push(format!("{file}:{}:{}: {m}", p.line, p.col));
    }
    problems.append(&mut top.problems);
    top.sheet
}

/// The elements a selector is matched against.
pub trait Dom {
    fn tag(&self, el: usize) -> &str;
    fn attr(&self, el: usize, name: &str) -> Option<&str>;
    fn parent(&self, el: usize) -> Option<usize>;
}

fn simple_matches(s: &Simple, dom: &dyn Dom, el: usize) -> bool {
    match s {
        Simple::Tag(t) => dom.tag(el) == t,
        Simple::Id(i) => dom.attr(el, "id") == Some(i),
        Simple::Class(c) => dom.attr(el, "class").is_some_and(|l| l.split_whitespace().any(|x| x == c)),
        Simple::Attr(n, v) => match (dom.attr(el, n), v) {
            (Some(_), None) => true,
            (Some(a), Some(v)) => a == v,
            _ => false,
        },
    }
}

fn compound_matches(c: &Compound, dom: &dyn Dom, el: usize) -> bool {
    c.simples.iter().all(|s| simple_matches(s, dom, el)) && !c.nots.iter().any(|s| simple_matches(s, dom, el))
}

/// Whether `sel` selects element `el`: its parts without the state tests, which are the rule's condition.
pub fn matches(sel: &Selector, dom: &dyn Dom, el: usize) -> bool {
    fn up(parts: &[(Comb, Compound)], dom: &dyn Dom, el: usize) -> bool {
        let Some(((comb, c), rest)) = parts.split_last() else { return true };
        if !compound_matches(c, dom, el) {
            return false;
        }
        if rest.is_empty() {
            return true;
        }
        match comb {
            Comb::Child => dom.parent(el).is_some_and(|p| up(rest, dom, p)),
            Comb::Descendant => {
                let mut at = dom.parent(el);
                while let Some(p) = at {
                    if up(rest, dom, p) {
                        return true;
                    }
                    at = dom.parent(p);
                }
                false
            },
        }
    }
    up(&sel.parts, dom, el)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet(text: &str) -> (Sheet, Vec<String>) {
        let mut problems = vec![];
        let s = parse(text, "t.css", 0, Media::default(), &mut problems);
        (s, problems)
    }

    #[test]
    fn reads_rules_with_places() {
        let (s, problems) = sheet(".a > .b:hover:not(:active) {\n  padding: 4px 8px;\n  color: var(--x--y);\n  width: {w};\n}");
        assert_eq!(problems, Vec::<String>::new());
        let r = &s.rules[0];
        assert_eq!(r.selectors[0].condition().as_deref(), Some("hover && !active"));
        assert_eq!(r.selectors[0].specificity, (0, 4, 0));
        assert_eq!(r.decls[0].pos, Pos { line: 2, col: 3 });
        assert_eq!(r.decls[0].value, [Tok::Dim(4., "px".into()), Tok::Dim(8., "px".into())]);
        assert_eq!(r.decls[1].value, [Tok::Func("var".into(), vec![Tok::Ident("--x--y".into())])]);
        assert_eq!(r.decls[2].value, [Tok::Hole("w".into())]);
    }

    #[test]
    fn root_keyframes_and_media() {
        let (s, problems) = sheet(
            ":root { --row-height: 28px; }\n@keyframes pulse { from { opacity: 1 } 50%, 80% { opacity: .5 } }\n\
             @media (prefers-reduced-motion: reduce) { .a { animation: none } }",
        );
        assert_eq!(problems, Vec::<String>::new());
        assert_eq!(s.tokens[0].name, "--row-height");
        assert_eq!(s.frames[0].stops.iter().map(|s| s.0.as_str()).collect::<Vec<_>>(), ["from", "50%, 80%"]);
        // Reduced motion is off here: the media rule doesn't apply.
        assert!(s.rules.is_empty());
        let mut p = vec![];
        let s =
            parse("@media (prefers-reduced-motion: reduce) { .a { animation: none } }", "t.css", 0, Media { reduce_motion: true }, &mut p);
        assert_eq!(s.rules.len(), 1);
    }

    #[test]
    fn unsupported_things_are_problems_with_places() {
        let (_, p) = sheet(".a { color: red !important }");
        assert_eq!(p, ["t.css:1:6: !important is not supported; order and specificity decide"]);
        let (_, p) = sheet("\n@font-face { x: y }");
        assert!(p[0].starts_with("t.css:2:1: @font-face is not supported"), "{p:?}");
        let (_, p) = sheet(".a::before { x: y }");
        assert!(p[0].starts_with("t.css:1:1: pseudo-elements"), "{p:?}");
        let (_, p) = sheet(".a + .b { x: y }");
        assert!(p[0].contains("sibling combinators"), "{p:?}");
        let (_, p) = sheet("@media (min-width: 3px) { .a { x: y } }");
        assert!(p[0].contains("only (prefers-reduced-motion"), "{p:?}");
        let (_, p) = sheet(".a:hover .b { x: y }");
        assert!(p[0].contains("last part of a selector"), "{p:?}");
        let (_, p) = sheet(".a { --x: 1 }");
        assert!(p[0].contains("go in :root"), "{p:?}");
    }
}

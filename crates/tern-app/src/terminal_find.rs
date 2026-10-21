//! Find in a terminal (⌘F): the query's options, its matches over the
//! screen and scrollback, and which one is current. The bar is the
//! `terminal.find` template, drawn by [`crate::terminal_view`].

use tern_term::vt::grid::Dimensions;
use tern_term::vt::index::{Column, Direction, Point};
use tern_term::vt::term::Term;
use tern_term::vt::term::search::{Match, RegexIter, RegexSearch};

/// The most matches counted: past that the count says "+".
pub const MAX_MATCHES: usize = 10_000;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    pub text: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
}

impl Query {
    /// The regex to search with, or `None` for an empty query.
    pub fn pattern(&self) -> Option<String> {
        if self.text.is_empty() {
            return None;
        }
        let body = if self.regex { self.text.clone() } else { escape(&self.text) };
        // ASCII word boundaries: alacritty's lazy DFAs can't do Unicode's.
        let body = if self.whole_word { format!(r"(?-u:\b)(?:{body})(?-u:\b)") } else { body };
        // Explicit, over alacritty's smart case (any capital letter makes
        // it case-sensitive).
        Some(format!("(?{}){body}", if self.case_sensitive { "-i" } else { "i" }))
    }
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if r"\.+*?()|[]{}^$#&-~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[derive(Default)]
pub struct Matches {
    pub all: Vec<Match>,
    /// Index into `all`.
    pub current: Option<usize>,
    /// More than [`MAX_MATCHES`].
    pub more: bool,
    /// The query isn't a valid regex.
    pub error: bool,
}

impl Matches {
    /// Every match of `query` from the top of the scrollback down. The
    /// current one is the last that starts on or above `bottom` (the
    /// newest one shown): finding starts where the user looks.
    pub fn find<T>(term: &Term<T>, query: &Query, bottom: Point) -> Self {
        let Some(pattern) = query.pattern() else { return Self::default() };
        let Ok(mut regex) = RegexSearch::new(&pattern) else { return Self { error: true, ..Self::default() } };
        let start = Point::new(term.topmost_line(), Column(0));
        let end = Point::new(term.bottommost_line(), term.last_column());
        let mut all: Vec<Match> = RegexIter::new(start, end, Direction::Right, term, &mut regex).take(MAX_MATCHES + 1).collect();
        let more = all.len() > MAX_MATCHES;
        all.truncate(MAX_MATCHES);
        let current = all.iter().rposition(|m| *m.start() <= bottom).or((!all.is_empty()).then_some(0));
        Self { all, current, more, error: false }
    }

    /// Keep the current match on the same text after a new search: the
    /// match at `at`, or the nearest above it.
    pub fn keep_near(&mut self, at: Point) {
        if self.all.is_empty() {
            return;
        }
        self.current = Some(self.all.iter().rposition(|m| *m.start() <= at).unwrap_or(0));
    }

    /// Move to the match above (`up`, older output) or below, wrapping.
    pub fn step(&mut self, up: bool) -> Option<&Match> {
        let n = self.all.len();
        if n == 0 {
            return None;
        }
        let i = match self.current {
            None => n - 1,
            Some(i) if up => (i + n - 1) % n,
            Some(i) => (i + 1) % n,
        };
        self.current = Some(i);
        self.all.get(i)
    }

    pub fn current_match(&self) -> Option<&Match> {
        self.all.get(self.current?)
    }

    /// `3/12`, `0/0`, `3/10000+`.
    pub fn count(&self) -> String {
        if self.all.is_empty() {
            return "0/0".into();
        }
        let at = self.current.map_or(0, |i| i + 1);
        format!("{at}/{}{}", self.all.len(), if self.more { "+" } else { "" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tern_term::vt::event::VoidListener;
    use tern_term::vt::index::Line;
    use tern_term::vt::term::Config;
    use tern_term::vt::vte::ansi::{Processor, StdSyncHandler};

    fn term(text: &str) -> Term<VoidListener> {
        let size = tern_term::TermSize { cols: 20, rows: 4, cell_width: 8, cell_height: 16 };
        let mut t = Term::new(Config { scrolling_history: 100, ..Config::default() }, &size, VoidListener);
        Processor::<StdSyncHandler>::new().advance(&mut t, text.as_bytes());
        t
    }

    fn q(text: &str) -> Query {
        Query { text: text.into(), ..Query::default() }
    }

    fn bottom<T>(t: &Term<T>) -> Point {
        Point::new(t.bottommost_line(), t.last_column())
    }

    #[test]
    fn plain_text_is_literal_and_case_insensitive() {
        let t = term("a.b axb\r\nA.B\r\n");
        let m = Matches::find(&t, &q("a.b"), bottom(&t));
        assert_eq!(m.all.len(), 2);
        assert_eq!(m.current, Some(1));
        assert_eq!(m.count(), "2/2");
        let m = Matches::find(&t, &Query { case_sensitive: true, ..q("A.B") }, bottom(&t));
        assert_eq!(m.all.len(), 1);
        assert_eq!(*m.all[0].start(), Point::new(Line(1), Column(0)));
    }

    #[test]
    fn regex_and_whole_word() {
        let t = term("cat concat cats\r\n");
        assert_eq!(Matches::find(&t, &Query { regex: true, ..q("ca.s?") }, bottom(&t)).all.len(), 3);
        assert_eq!(Matches::find(&t, &Query { whole_word: true, ..q("cat") }, bottom(&t)).all.len(), 1);
        assert!(Matches::find(&t, &Query { regex: true, ..q("(") }, bottom(&t)).error);
        assert!(Matches::find(&t, &q(""), bottom(&t)).all.is_empty());
    }

    #[test]
    fn steps_wrap_around() {
        let t = term("x x x\r\n");
        let mut m = Matches::find(&t, &q("x"), bottom(&t));
        assert_eq!(m.current, Some(2));
        m.step(true);
        assert_eq!(m.current, Some(1));
        m.step(false);
        m.step(false);
        assert_eq!(m.current, Some(0));
        m.keep_near(Point::new(Line(0), Column(3)));
        assert_eq!(m.current, Some(1));
    }
}

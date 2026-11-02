//! Inheritance navigation over a server's answers, ported from OpenIDEA
//! (`lineMarkers.ts`, `structureNav.ts`): which declarations get a gutter
//! icon ("is implemented by" ↓, "implements" ↑), what it says, and the
//! super methods of a method. Pure, apart from the server calls a caller
//! hands in.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use lsp_types::{DocumentSymbol, Position, SymbolKind};
use regex::Regex;

use crate::{HierarchyItem, Location};

/// Types whose subtypes / implementations are worth a marker (not structs,
/// enums: they can't be extended). Rust traits come as interfaces.
const INHERITABLE_TYPES: &[SymbolKind] = &[SymbolKind::CLASS, SymbolKind::INTERFACE];
const ABSTRACT_TYPES: &[SymbolKind] = &[SymbolKind::INTERFACE];
/// Types whose methods may implement or override a supertype's (Rust `impl`
/// blocks come as objects).
const MEMBER_OWNERS: &[SymbolKind] = &[SymbolKind::CLASS, SymbolKind::INTERFACE, SymbolKind::ENUM, SymbolKind::STRUCT, SymbolKind::OBJECT];
const METHODS: &[SymbolKind] = &[SymbolKind::FUNCTION, SymbolKind::METHOD, SymbolKind::CONSTRUCTOR];
const TYPES: &[SymbolKind] = &[
    SymbolKind::CLASS,
    SymbolKind::INTERFACE,
    SymbolKind::STRUCT,
    SymbolKind::ENUM,
    SymbolKind::MODULE,
    SymbolKind::NAMESPACE,
    SymbolKind::OBJECT,
];

/// Names listed in a tooltip, at most.
const MAX_NAMES: usize = 8;
/// Supertype levels searched for a super method.
const SUPER_DEPTH: usize = 3;

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("valid regex")
}

/// Modifiers that rule out implementations / overriders.
static NOT_INHERITABLE: LazyLock<Regex> = LazyLock::new(|| re(r"\b(private|static|final)\b"));
static NOT_OVERRIDING: LazyLock<Regex> = LazyLock::new(|| re(r"\b(private|static)\b"));
static ABSTRACT: LazyLock<Regex> = LazyLock::new(|| re(r"\babstract\b"));
/// An interface method with a body (Java `default`).
static DEFAULT: LazyLock<Regex> = LazyLock::new(|| re(r"\bdefault\b"));
static ANNOTATION: LazyLock<Regex> = LazyLock::new(|| re(r"@[\w.]+(\([^)]*\))?"));
static TYPE_DECLARATION: LazyLock<Regex> =
    LazyLock::new(|| re(r"\b(?:class|interface|trait|object|struct|enum|record|impl)\s+([\p{L}_$][\p{L}\p{N}_$]*)"));
static COMMENT: LazyLock<Regex> = LazyLock::new(|| re(r"^\s*(//|\*|/\*)"));
static SIGNATURE_TAIL: LazyLock<Regex> = LazyLock::new(|| re(r"\s*[(<].*$"));
static RETURN_TAIL: LazyLock<Regex> = LazyLock::new(|| re(r"\s*:.*$"));

/// A declaration that may get a marker.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// Outline name (`eat(String)` with jdtls), matched against supertypes' members.
    pub symbol: String,
    /// Bare name, for titles.
    pub name: String,
    /// Where its identifier is (characters).
    pub line: u32,
    pub col: u32,
    pub method: bool,
    /// Declared abstract: an interface / trait, a member of one, or marked `abstract`.
    pub is_abstract: bool,
    /// Can have implementations / overriders (asked with an implementation request).
    pub inheritable: bool,
    /// Can implement / override a supertype's method (looked up in the type hierarchy).
    pub overriding: bool,
    /// Identifier position of the enclosing type (methods).
    pub owner: Option<Position>,
}

impl Candidate {
    pub fn key(&self) -> String {
        format!("{}:{}:{}", self.line, self.col, self.symbol)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MarkerKind {
    /// ↓ on an abstract declaration with implementations.
    Implemented,
    /// ↓ on a concrete one with overriders / subclasses.
    Overridden,
    /// ↑ on a method implementing an abstract one.
    Implementing,
    /// ↑ on a method overriding a concrete one.
    Overriding,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LineMarker {
    pub kind: MarkerKind,
    pub line: u32,
    /// Name of the declaration it is on.
    pub name: String,
    pub targets: Vec<Location>,
    pub tooltip: String,
    /// Title of the targets chooser.
    pub chooser_title: String,
}

/// `beta(int)` (jdtls) and `beta` name the same method.
pub fn base_name(name: &str) -> String {
    SIGNATURE_TAIL.replace(name, "").into_owned()
}

/// `beta(int) : void` (jdtls) is the signature `beta(int)`.
fn signature(name: &str) -> String {
    RETURN_TAIL.replace(name, "").into_owned()
}

fn chars(s: &str, from: u32, to: Option<u32>) -> String {
    let it = s.chars().skip(from as usize);
    match to {
        Some(to) => it.take(to.saturating_sub(from) as usize).collect(),
        None => it.collect(),
    }
}

/// The declaration's text before its identifier (annotations and modifiers included).
fn declaration_prefix(item: &DocumentSymbol, line_text: &dyn Fn(u32) -> String) -> String {
    let (start, at) = (item.range.start, item.selection_range.start);
    if start.line > at.line {
        return chars(&line_text(at.line), 0, Some(at.character));
    }
    let mut parts = vec![];
    let mut l = start.line;
    while l <= at.line && parts.len() < 12 {
        let text = line_text(l);
        let from = if l == start.line { start.character } else { 0 };
        parts.push(if l == at.line { chars(&text, from, Some(at.character)) } else { chars(&text, from, None) });
        l += 1;
    }
    // Annotation arguments (`@Foo(final = …)`) are not modifiers.
    ANNOTATION.replace_all(&parts.join(" "), " ").into_owned()
}

/// Declarations that may get a marker, in document order.
pub fn candidates(outline: &[DocumentSymbol], line_text: impl Fn(u32) -> String) -> Vec<Candidate> {
    fn walk(items: &[DocumentSymbol], parent: Option<&DocumentSymbol>, line_text: &dyn Fn(u32) -> String, out: &mut Vec<Candidate>) {
        for it in items {
            let at = it.selection_range.start;
            let base = |method, is_abstract, inheritable, overriding, owner| Candidate {
                symbol: it.name.clone(),
                name: base_name(&it.name),
                line: at.line,
                col: at.character,
                method,
                is_abstract,
                inheritable,
                overriding,
                owner,
            };
            if INHERITABLE_TYPES.contains(&it.kind) {
                let p = declaration_prefix(it, line_text);
                out.push(base(
                    false,
                    ABSTRACT_TYPES.contains(&it.kind) || ABSTRACT.is_match(&p),
                    !NOT_INHERITABLE.is_match(&p),
                    false,
                    None,
                ));
            } else if it.kind == SymbolKind::METHOD
                && let Some(parent) = parent.filter(|p| MEMBER_OWNERS.contains(&p.kind))
            {
                let p = declaration_prefix(it, line_text);
                let in_abstract = ABSTRACT_TYPES.contains(&parent.kind);
                out.push(base(
                    true,
                    (in_abstract && !DEFAULT.is_match(&p)) || ABSTRACT.is_match(&p),
                    INHERITABLE_TYPES.contains(&parent.kind) && !NOT_INHERITABLE.is_match(&p),
                    !NOT_OVERRIDING.is_match(&p),
                    Some(parent.selection_range.start),
                ));
            }
            if let Some(children) = &it.children {
                walk(children, Some(it), line_text, out);
            }
        }
    }
    let mut out = vec![];
    walk(outline, None, &line_text, &mut out);
    out.sort_by_key(|c| (c.line, c.col));
    out
}

/// Servers answer an implementation request on a declaration with the declaration itself too.
pub fn without_self(path: &Path, c: &Candidate, targets: Vec<Location>) -> Vec<Location> {
    targets.into_iter().filter(|t| !(t.path == path && t.range.start.line == c.line)).collect()
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// The identifier at character `col` of `line`.
pub fn identifier_at(line: &str, col: u32) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    let col = (col as usize).min(chars.len());
    let mut start = col;
    while start > 0 && is_ident(chars[start - 1]) {
        start -= 1;
    }
    let mut end = col;
    while end < chars.len() && is_ident(chars[end]) {
        end += 1;
    }
    (end > start).then(|| chars[start..end].iter().collect())
}

/// Name of the type a target sits in: the nearest type declaration above it, else the file name.
fn enclosing_type_name(lines: Option<&Vec<String>>, target: &Location) -> String {
    if let Some(lines) = lines {
        let from = target.range.start.line as usize;
        for l in (from.saturating_sub(2000)..from.min(lines.len())).rev() {
            let text = &lines[l];
            if let Some(m) = TYPE_DECLARATION.captures(text)
                && !COMMENT.is_match(text)
            {
                return m[1].to_owned();
            }
        }
    }
    target.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Display name of a target: the type itself for type markers, the type around it for methods.
pub fn target_name(lines: Option<&Vec<String>>, target: &Location, method: bool) -> String {
    if !method
        && let Some(name) =
            lines.and_then(|l| l.get(target.range.start.line as usize)).and_then(|l| identifier_at(l, target.range.start.character))
    {
        return name;
    }
    enclosing_type_name(lines, target)
}

/// How a place is shown in a chooser: the name declared there, and the type
/// it is in (none when it is that type).
#[derive(Debug, Clone, PartialEq)]
pub struct Described {
    pub name: String,
    pub container: String,
}

/// The name at `target` and the type around it, from `lines` of its file.
pub fn describe(target: &Location, lines: &dyn Fn(&Path) -> Option<Vec<String>>) -> Described {
    let text = lines(&target.path);
    let name = target_name(text.as_ref(), target, false);
    // A type's own declaration line names it: it is no member of the type above.
    let declares_type = text
        .as_ref()
        .and_then(|l| l.get(target.range.start.line as usize))
        .and_then(|l| TYPE_DECLARATION.captures(l))
        .is_some_and(|m| m[1] == name);
    let container = enclosing_type_name(text.as_ref(), target);
    let container = if declares_type || container == name || text.is_none() { String::new() } else { container };
    Described { name, container }
}

/// `A, B, C`, sorted and without repeats, at most [`MAX_NAMES`] of them.
pub fn name_list(names: &[String]) -> String {
    let mut unique: Vec<&String> = names.iter().collect::<HashSet<_>>().into_iter().collect();
    unique.sort();
    let shown = unique.iter().take(MAX_NAMES).map(|s| s.as_str()).collect::<Vec<_>>().join(", ");
    if unique.len() > MAX_NAMES { format!("{shown} and {} more", unique.len() - MAX_NAMES) } else { shown }
}

/// ↓ marker of `c` from its implementations (self excluded already); none without any.
pub fn inheritors_marker(c: &Candidate, targets: Vec<Location>, files: &HashMap<PathBuf, Vec<String>>) -> Option<LineMarker> {
    if targets.is_empty() {
        return None;
    }
    let kind = if c.is_abstract { MarkerKind::Implemented } else { MarkerKind::Overridden };
    let names = name_list(&targets.iter().map(|t| target_name(files.get(&t.path), t, c.method)).collect::<Vec<_>>());
    let (n, name) = (targets.len(), &c.name);
    let (tooltip, chooser_title) = match (c.method, c.is_abstract) {
        (false, true) => (format!("Is implemented by {names}"), format!("Choose Implementation of {name} ({n} found)")),
        (false, false) => (format!("Is subclassed by {names}"), format!("Choose Subclass of {name} ({n} found)")),
        (true, true) => (format!("Is implemented in {names}"), format!("Choose Implementation of {name} ({n} found)")),
        (true, false) => (format!("Is overridden in {names}"), format!("Choose Overriding Method of {name} ({n} found)")),
    };
    Some(LineMarker { kind, line: c.line, name: c.name.clone(), targets, tooltip, chooser_title })
}

/// A method of a supertype that a method implements or overrides.
#[derive(Debug, Clone, PartialEq)]
pub struct SuperMethod {
    pub location: Location,
    /// The supertype declaring it.
    pub owner: HierarchyItem,
    pub method: DocumentSymbol,
}

/// Whether a super method is abstract: declared in an interface / trait, or marked `abstract`.
fn is_abstract_super(s: &SuperMethod, files: &HashMap<PathBuf, Vec<String>>) -> bool {
    let lines = files.get(&s.location.path);
    let line_text = |l: u32| lines.and_then(|ls| ls.get(l as usize)).cloned().unwrap_or_default();
    let prefix = declaration_prefix(&s.method, &line_text);
    (ABSTRACT_TYPES.contains(&s.owner.kind) && !DEFAULT.is_match(&prefix)) || ABSTRACT.is_match(&prefix)
}

/// ↑ marker of method `c` from its super methods; none without any.
pub fn supers_marker(c: &Candidate, supers: &[SuperMethod], files: &HashMap<PathBuf, Vec<String>>) -> Option<LineMarker> {
    if supers.is_empty() {
        return None;
    }
    // Implementing wins when one of several super methods is abstract, as an interface method would.
    let implementing = supers.iter().any(|s| is_abstract_super(s, files));
    let owners = name_list(&supers.iter().map(|s| base_name(&s.owner.name)).collect::<Vec<_>>());
    Some(LineMarker {
        kind: if implementing { MarkerKind::Implementing } else { MarkerKind::Overriding },
        line: c.line,
        name: c.name.clone(),
        targets: supers.iter().map(|s| s.location.clone()).collect(),
        tooltip: if implementing { format!("Implements method in {owners}") } else { format!("Overrides method in {owners}") },
        chooser_title: format!("Choose Super Method of {} ({} found)", c.name, supers.len()),
    })
}

fn before(a: Position, b: Position) -> bool {
    a.line < b.line || (a.line == b.line && a.character <= b.character)
}

fn inside(i: &DocumentSymbol, p: Position) -> bool {
    before(i.range.start, p) && before(p, i.range.end)
}

/// The innermost method at `caret`, and the innermost type around it (around the method when there is one).
pub fn member_at(outline: &[DocumentSymbol], caret: Position) -> (Option<&DocumentSymbol>, Option<&DocumentSymbol>) {
    let (mut method, mut ty) = (None, None);
    let mut level = outline;
    while let Some(hit) = level.iter().find(|i| inside(i, caret)) {
        if METHODS.contains(&hit.kind) {
            method = Some(hit);
        } else if TYPES.contains(&hit.kind) && method.is_none() {
            ty = Some(hit);
        }
        level = hit.children.as_deref().unwrap_or_default();
    }
    (method, ty)
}

/// The outline node of the type declared at `item` (found by position).
fn type_of<'a>(outline: &'a [DocumentSymbol], item: &HierarchyItem) -> Option<&'a DocumentSymbol> {
    let at = item.selection_range.start;
    for i in outline {
        if TYPES.contains(&i.kind) && i.selection_range.start.line == at.line {
            return Some(i);
        }
        if inside(i, at)
            && let Some(deeper) = type_of(i.children.as_deref().unwrap_or_default(), item)
        {
            return Some(deeper);
        }
    }
    None
}

/// Methods named `name` directly inside the type declared at `item`: the
/// overload with the same signature when the server names them with one
/// (`eat(String)`), else every method of that name.
fn methods_in_type(outline: &[DocumentSymbol], item: &HierarchyItem, name: &str) -> Vec<DocumentSymbol> {
    let bare = base_name(name);
    let named: Vec<&DocumentSymbol> = type_of(outline, item)
        .and_then(|t| t.children.as_deref())
        .unwrap_or_default()
        .iter()
        .filter(|c| METHODS.contains(&c.kind) && base_name(&c.name) == bare)
        .collect();
    let exact: Vec<&DocumentSymbol> = named.iter().copied().filter(|c| signature(&c.name) == signature(name)).collect();
    (if exact.is_empty() { named } else { exact }).into_iter().cloned().collect()
}

/// What the super method search asks the server.
pub trait SuperDeps {
    /// The supertypes of the type at `pos` in `path` (none when it can't tell).
    fn supertypes(&self, path: &Path, pos: Position) -> impl Future<Output = Vec<HierarchyItem>>;
    /// The outline of `path` (none when it can't tell).
    fn outline(&self, path: &Path) -> impl Future<Output = Vec<DocumentSymbol>>;
}

/// Super methods of `method` (a member of the type at `type_pos` in `path`),
/// nearest supertype level first, at most [`SUPER_DEPTH`] levels up.
pub async fn super_method_targets(path: &Path, type_pos: Position, method: &str, deps: &impl SuperDeps) -> Vec<SuperMethod> {
    let mut level = vec![(path.to_path_buf(), type_pos)];
    let mut seen = HashSet::new();
    for _ in 0..SUPER_DEPTH {
        if level.is_empty() {
            break;
        }
        let (mut next, mut found) = (vec![], vec![]);
        for (p, pos) in &level {
            for s in deps.supertypes(p, *pos).await {
                if !seen.insert((s.path.clone(), s.selection_range.start.line)) {
                    continue;
                }
                let methods = methods_in_type(&deps.outline(&s.path).await, &s, method);
                if methods.is_empty() {
                    next.push((s.path.clone(), s.selection_range.start));
                }
                for m in methods {
                    found.push(SuperMethod {
                        location: Location { path: s.path.clone(), range: m.selection_range },
                        owner: s.clone(),
                        method: m,
                    });
                }
            }
        }
        if !found.is_empty() {
            return found;
        }
        level = next;
    }
    vec![]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GotoKind {
    Definition,
    Declaration,
    Implementation,
}

/// Without a type hierarchy: languages whose servers answer "super" through another request.
pub fn super_fallback_kind(language: &str) -> Option<GotoKind> {
    match language {
        "go" => Some(GotoKind::Implementation),
        "rust" => Some(GotoKind::Declaration),
        _ => None,
    }
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use lsp_types::{Range, SymbolKind as K};

    fn r(l: u32, c: u32, l2: u32, c2: u32) -> Range {
        Range::new(Position::new(l, c), Position::new(l2, c2))
    }

    fn item(name: &str, kind: K, line: u32, end: u32, children: Vec<DocumentSymbol>, col: u32) -> DocumentSymbol {
        DocumentSymbol {
            name: name.into(),
            detail: None,
            kind,
            tags: None,
            deprecated: None,
            range: r(line, 0, end, 1),
            selection_range: r(line, col, line, col + name.len() as u32),
            children: (!children.is_empty()).then_some(children),
        }
    }

    fn loc(path: &str, line: u32, col: u32) -> Location {
        Location { path: path.into(), range: r(line, col, line, col + 3) }
    }

    fn hier(name: &str, kind: K, path: &str, line: u32) -> HierarchyItem {
        HierarchyItem::new(name.into(), kind, path.into(), r(line, 0, line + 20, 1), r(line, 4, line, 4 + name.len() as u32))
    }

    // Shape.java, as jdtls outlines it.
    const SHAPE: &[&str] = &[
        "public interface Shape {",
        "    double area();",
        "    default String label() { return \"\"; }",
        "    static Shape unit() { return null; }",
        "}",
    ];
    fn shape_outline() -> Vec<DocumentSymbol> {
        vec![item(
            "Shape",
            K::INTERFACE,
            0,
            4,
            vec![
                item("area()", K::METHOD, 1, 1, vec![], 11),
                item("label()", K::METHOD, 2, 2, vec![], 19),
                item("unit()", K::METHOD, 3, 3, vec![], 17),
            ],
            17,
        )]
    }
    const CIRCLE: &[&str] = &[
        "public class Circle extends Base implements Shape {",
        "    @Override",
        "    public double area() { return 1; }",
        "    private void helper() {}",
        "    public final String label() { return \"c\"; }",
        "}",
    ];
    fn circle_outline() -> Vec<DocumentSymbol> {
        let mut area = item("area()", K::METHOD, 1, 2, vec![], 4);
        // Its identifier sits on line 2 (its range starts at the annotation).
        area.selection_range = r(2, 18, 2, 22);
        vec![item(
            "Circle",
            K::CLASS,
            0,
            5,
            vec![area, item("helper()", K::METHOD, 3, 3, vec![], 17), item("label()", K::METHOD, 4, 4, vec![], 24)],
            13,
        )]
    }
    const BASE: &[&str] =
        &["public abstract class Base {", "    public abstract void run();", "    public String label() { return \"\"; }", "}"];
    fn base_outline() -> Vec<DocumentSymbol> {
        vec![item(
            "Base",
            K::CLASS,
            0,
            3,
            vec![item("run()", K::METHOD, 1, 1, vec![], 25), item("label()", K::METHOD, 2, 2, vec![], 18)],
            22,
        )]
    }

    fn files() -> HashMap<PathBuf, Vec<String>> {
        let f = |p: &str, l: &[&str]| (PathBuf::from(p), l.iter().map(|s| s.to_string()).collect());
        HashMap::from([
            f("/p/Shape.java", SHAPE),
            f("/p/Circle.java", CIRCLE),
            f("/p/Square.java", &["public class Square implements Shape {", "    public double area() { return 2; }", "}"]),
            f("/p/Base.java", BASE),
        ])
    }

    fn lines(text: &'static [&'static str]) -> impl Fn(u32) -> String {
        move |l| text.get(l as usize).copied().unwrap_or("").to_owned()
    }

    #[test]
    fn candidates_cover_inheritable_types_and_their_methods() {
        let shape = candidates(&shape_outline(), lines(SHAPE));
        let got: Vec<_> = shape.iter().map(|c| (c.name.as_str(), c.is_abstract, c.inheritable, c.overriding)).collect();
        // A default method has a body: overriding it is not implementing it.
        assert_eq!(
            got,
            [("Shape", true, true, false), ("area", true, true, true), ("label", false, true, true), ("unit", true, false, false)]
        );
        let circle = candidates(&circle_outline(), lines(CIRCLE));
        let got: Vec<_> = circle.iter().map(|c| (c.name.as_str(), c.line, c.inheritable, c.overriding)).collect();
        assert_eq!(got, [("Circle", 0, true, false), ("area", 2, true, true), ("helper", 3, false, false), ("label", 4, false, true)]);
        let base = candidates(&base_outline(), lines(BASE));
        let got: Vec<_> = base.iter().map(|c| (c.name.as_str(), c.is_abstract)).collect();
        assert_eq!(got, [("Base", true), ("run", true), ("label", false)]);
    }

    #[test]
    fn candidates_skip_structs_enums_and_free_functions() {
        let outline = vec![
            item("Point", K::STRUCT, 0, 3, vec![item("len", K::METHOD, 1, 1, vec![], 4)], 4),
            item("main", K::FUNCTION, 5, 6, vec![], 4),
        ];
        let got: Vec<_> = candidates(&outline, |_| String::new()).into_iter().map(|c| (c.name, c.inheritable, c.overriding)).collect();
        assert_eq!(got, [("len".to_owned(), false, true)]);
    }

    #[test]
    fn inheritors_markers_name_their_targets() {
        let shape = candidates(&shape_outline(), lines(SHAPE));
        // The server lists the declaration itself too.
        let targets = without_self(
            Path::new("/p/Shape.java"),
            &shape[0],
            vec![loc("/p/Shape.java", 0, 4), loc("/p/Circle.java", 0, 13), loc("/p/Square.java", 0, 13)],
        );
        let m = inheritors_marker(&shape[0], targets, &files()).unwrap();
        assert_eq!((m.line, m.kind, m.targets.len()), (0, MarkerKind::Implemented, 2));
        assert_eq!(m.tooltip, "Is implemented by Circle, Square");
        assert_eq!(m.chooser_title, "Choose Implementation of Shape (2 found)");
        let m = inheritors_marker(&shape[1], vec![loc("/p/Circle.java", 2, 18), loc("/p/Square.java", 1, 18)], &files()).unwrap();
        assert_eq!(m.tooltip, "Is implemented in Circle, Square");
        assert!(inheritors_marker(&shape[1], vec![], &files()).is_none());

        let base = candidates(&base_outline(), lines(BASE));
        let m = inheritors_marker(&base[0], vec![loc("/p/Circle.java", 0, 13)], &files()).unwrap();
        assert_eq!((m.kind, m.tooltip.as_str()), (MarkerKind::Implemented, "Is implemented by Circle"));
        let m = inheritors_marker(&base[2], vec![loc("/p/Circle.java", 4, 25)], &files()).unwrap();
        assert_eq!((m.kind, m.tooltip.as_str()), (MarkerKind::Overridden, "Is overridden in Circle"));
        assert_eq!(m.chooser_title, "Choose Overriding Method of label (1 found)");
    }

    struct Deps {
        supers: HashMap<PathBuf, Vec<HierarchyItem>>,
        outlines: HashMap<PathBuf, Vec<DocumentSymbol>>,
    }

    impl SuperDeps for Deps {
        async fn supertypes(&self, path: &Path, _: Position) -> Vec<HierarchyItem> {
            self.supers.get(path).cloned().unwrap_or_default()
        }
        async fn outline(&self, path: &Path) -> Vec<DocumentSymbol> {
            self.outlines.get(path).cloned().unwrap_or_default()
        }
    }

    #[test]
    fn supers_markers_from_the_type_hierarchy() {
        let deps = Deps {
            supers: HashMap::from([(
                "/p/Circle.java".into(),
                vec![hier("Base", K::CLASS, "/p/Base.java", 0), hier("Shape", K::INTERFACE, "/p/Shape.java", 0)],
            )]),
            outlines: HashMap::from([("/p/Shape.java".into(), shape_outline()), ("/p/Base.java".into(), base_outline())]),
        };
        let circle = candidates(&circle_outline(), lines(CIRCLE));
        let path = Path::new("/p/Circle.java");
        let mut out = vec![];
        for c in circle.iter().filter(|c| c.overriding && c.owner.is_some()) {
            let supers = block_on(super_method_targets(path, c.owner.unwrap(), &c.symbol, &deps));
            if let Some(m) = supers_marker(c, &supers, &files()) {
                let targets: Vec<_> = m.targets.iter().map(|t| format!("{}:{}", t.path.display(), t.range.start.line)).collect();
                out.push((m.line, m.kind, m.tooltip, targets, m.chooser_title));
            }
        }
        assert_eq!(
            out,
            [
                (
                    2,
                    MarkerKind::Implementing,
                    "Implements method in Shape".to_owned(),
                    vec!["/p/Shape.java:1".to_owned()],
                    "Choose Super Method of area (1 found)".to_owned()
                ),
                // Base.label() and the default Shape.label() have bodies.
                (
                    4,
                    MarkerKind::Overriding,
                    "Overrides method in Base, Shape".to_owned(),
                    vec!["/p/Base.java:2".to_owned(), "/p/Shape.java:2".to_owned()],
                    "Choose Super Method of label (2 found)".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn super_search_goes_level_by_level() {
        let deps = Deps {
            supers: HashMap::from([
                ("/Foo.java".into(), vec![hier("Base", K::CLASS, "/Base.java", 0)]),
                ("/Base.java".into(), vec![hier("Root", K::CLASS, "/Root.java", 0)]),
            ]),
            outlines: HashMap::from([
                ("/Base.java".into(), vec![item("Base", K::CLASS, 0, 10, vec![item("other", K::METHOD, 2, 3, vec![], 4)], 4)]),
                ("/Root.java".into(), vec![item("Root", K::INTERFACE, 0, 10, vec![item("a", K::METHOD, 4, 4, vec![], 4)], 4)]),
            ]),
        };
        let at = Position::new(0, 4);
        let found = block_on(super_method_targets(Path::new("/Foo.java"), at, "a", &deps));
        assert_eq!(
            found.iter().map(|s| s.location.clone()).collect::<Vec<_>>(),
            [Location { path: "/Root.java".into(), range: r(4, 4, 4, 5) }]
        );
        assert!(block_on(super_method_targets(Path::new("/Foo.java"), at, "missing", &deps)).is_empty());
        // jdtls names methods with their parameters.
        assert_eq!(block_on(super_method_targets(Path::new("/Foo.java"), at, "a() : void", &deps)).len(), 1);
    }

    #[test]
    fn super_search_prefers_the_same_overload() {
        let deps = Deps {
            supers: HashMap::from([("/Foo.java".into(), vec![hier("Base", K::CLASS, "/Base.java", 0)])]),
            outlines: HashMap::from([(
                "/Base.java".into(),
                vec![item(
                    "Base",
                    K::CLASS,
                    0,
                    10,
                    vec![item("eat(String)", K::METHOD, 2, 2, vec![], 4), item("eat(int)", K::METHOD, 4, 4, vec![], 4)],
                    4,
                )],
            )]),
        };
        let lines = |name: &str| -> Vec<u32> {
            block_on(super_method_targets(Path::new("/Foo.java"), Position::new(0, 4), name, &deps))
                .iter()
                .map(|s| s.location.range.start.line)
                .collect()
        };
        assert_eq!(lines("eat(int)"), [4]);
        // Without a matching signature (or without signatures at all), every overload.
        assert_eq!(lines("eat(T)"), [2, 4]);
        assert_eq!(lines("eat"), [2, 4]);
    }

    #[test]
    fn member_at_finds_method_and_type() {
        let outline = vec![
            item("Foo", K::CLASS, 0, 20, vec![item("Inner", K::CLASS, 11, 15, vec![item("c", K::METHOD, 12, 14, vec![], 4)], 4)], 4),
            item("main", K::FUNCTION, 22, 25, vec![], 4),
        ];
        let at = |l, c| {
            let (m, t) = member_at(&outline, Position::new(l, c));
            (m.map(|m| m.name.clone()), t.map(|t| t.name.clone()))
        };
        assert_eq!(at(13, 2), (Some("c".into()), Some("Inner".into())));
        assert_eq!(at(10, 0), (None, Some("Foo".into())));
        assert_eq!(at(23, 0), (Some("main".into()), None));
    }

    #[test]
    fn names_list_caps_at_eight() {
        let names: Vec<String> = ["J", "I", "H", "G", "F", "E", "D", "C", "B", "A", "A"].iter().map(|s| s.to_string()).collect();
        assert_eq!(name_list(&names), "A, B, C, D, E, F, G, H and 2 more");
    }

    #[test]
    fn describe_names_targets_and_their_types() {
        let lines = |p: &Path| files().get(p).cloned();
        let d = describe(&loc("/p/Circle.java", 2, 18), &lines);
        assert_eq!((d.name.as_str(), d.container.as_str()), ("area", "Circle"));
        let d = describe(&loc("/p/Circle.java", 0, 13), &lines);
        assert_eq!((d.name.as_str(), d.container.as_str()), ("Circle", ""), "a type is its own container");
        let d = describe(&loc("/p/Gone.java", 3, 0), &lines);
        assert_eq!((d.name.as_str(), d.container.as_str()), ("Gone", ""));
        // A type declared under another one is still its own container.
        let swift = |_: &Path| Some(["class Base: Shape {", "}", "", "class Circle: Base {", "}"].map(String::from).to_vec());
        let d = describe(&loc("/p/Shape.swift", 3, 6), &swift);
        assert_eq!((d.name.as_str(), d.container.as_str()), ("Circle", ""));
    }

    #[test]
    fn super_fallback_by_language() {
        assert_eq!(super_fallback_kind("go"), Some(GotoKind::Implementation));
        assert_eq!(super_fallback_kind("rust"), Some(GotoKind::Declaration));
        assert_eq!(super_fallback_kind("java"), None);
    }
}

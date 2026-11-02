use std::path::Path;

use futures::executor::block_on;
use lsp_types::{Position, Range};
use serde_json::{Value, json};
use tern_lsp::{Location, LspError};

mod common;
use common::*;

fn uri(p: &Path) -> String {
    tern_lsp::uri::path_to_uri(p).unwrap().as_str().to_owned()
}

fn range(l: u32, a: u32, b: u32) -> Value {
    json!({"start": {"line": l, "character": a}, "end": {"line": l, "character": b}})
}

fn r(l: u32, a: u32, b: u32) -> Range {
    Range::new(Position::new(l, a), Position::new(l, b))
}

const ALL: &str = r#"{"definitionProvider": true, "declarationProvider": true, "implementationProvider": true, "documentSymbolProvider": true, "typeHierarchyProvider": true, "positionEncoding": "utf-32"}"#;

fn caps() -> Value {
    serde_json::from_str(ALL).unwrap()
}

#[test]
fn definition_handles_scalar_array_and_links() {
    let f = Fixture::new();
    let other = f.ws.join("src/lib.rs");
    std::fs::write(&other, "pub struct S;\npub struct T;\n").unwrap();
    let u = uri(&other);
    let script = json!({"capabilities": caps(), "responses": {
        "textDocument/definition@0:3": {"uri": u, "range": range(0, 11, 12)},
        "textDocument/definition@0:4": [{"uri": u, "range": range(1, 11, 12)}, {"uri": u, "range": range(0, 11, 12)}, {"uri": u, "range": range(0, 11, 12)}],
        "textDocument/definition@0:5": [{"targetUri": u, "targetRange": range(1, 0, 13), "targetSelectionRange": range(1, 11, 12)}],
    }});
    let (m, _) = f.manager(vec![f.mock("a", script)]);
    let def = |c| block_on(m.definition(&f.main_rs(), Position::new(0, c))).unwrap();
    assert_eq!(def(3), [Location { path: other.clone(), range: r(0, 11, 12) }]);
    // Deduped and sorted.
    assert_eq!(def(4), [Location { path: other.clone(), range: r(0, 11, 12) }, Location { path: other.clone(), range: r(1, 11, 12) }]);
    // A link's selection range: the name, not the whole declaration.
    assert_eq!(def(5), [Location { path: other.clone(), range: r(1, 11, 12) }]);
    assert_eq!(def(6), []);
}

#[test]
fn implementations_batch_keeps_order_and_empty_for_failures() {
    let f = Fixture::new();
    let u = uri(&f.main_rs());
    let script = json!({"capabilities": caps(),
        "responses": {"textDocument/implementation@0:1": [{"uri": u, "range": range(0, 0, 2)}]},
        "errors": {"textDocument/implementation@0:2": "boom"}});
    let (m, _) = f.manager(vec![f.mock("a", script)]);
    let got = block_on(m.implementations(&f.main_rs(), &[Position::new(0, 1), Position::new(0, 2), Position::new(0, 3)])).unwrap();
    assert_eq!(got, [vec![Location { path: f.main_rs(), range: r(0, 0, 2) }], vec![], vec![]]);
    // All failing: the error.
    let all = block_on(m.implementations(&f.main_rs(), &[Position::new(0, 2)]));
    assert!(matches!(all, Err(LspError::Server { .. })), "{all:?}");
}

#[test]
fn unsupported_without_capability() {
    let f = Fixture::new();
    let (m, _) = f.manager(vec![f.mock("a", json!({"capabilities": {"definitionProvider": true}}))]);
    assert_eq!(block_on(m.implementation(&f.main_rs(), Position::new(0, 0))), Err(LspError::Unsupported));
    assert_eq!(block_on(m.supertypes(&f.main_rs(), Position::new(0, 0))), Err(LspError::Unsupported));
    assert_eq!(block_on(m.definition(&f.main_rs(), Position::new(0, 0))), Ok(vec![]));
}

#[test]
fn outline_nests_flat_symbols_by_container() {
    let f = Fixture::new();
    let u = uri(&f.main_rs());
    let script = json!({"capabilities": caps(), "responses": {"textDocument/documentSymbol": [
        {"name": "A", "kind": 5, "location": {"uri": u, "range": range(0, 0, 9)}},
        {"name": "m", "kind": 6, "containerName": "A", "location": {"uri": u, "range": range(0, 4, 5)}},
        {"name": "f", "kind": 12, "location": {"uri": u, "range": range(0, 7, 8)}},
    ]}});
    let (m, _) = f.manager(vec![f.mock("a", script)]);
    let outline = block_on(m.outline(&f.main_rs())).unwrap();
    let names: Vec<_> = outline.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["A", "f"]);
    let children = outline[0].children.as_ref().unwrap();
    assert_eq!(children[0].name, "m");
    assert_eq!(children[0].selection_range, r(0, 4, 5));
}

#[test]
fn outline_keeps_hierarchical_symbols() {
    let f = Fixture::new();
    let script = json!({"capabilities": caps(), "responses": {"textDocument/documentSymbol": [
        {"name": "A", "kind": 5, "range": range(0, 0, 9), "selectionRange": range(0, 1, 2), "children": [
            {"name": "m", "kind": 6, "range": range(0, 3, 6), "selectionRange": range(0, 4, 5)}
        ]}
    ]}});
    let (m, _) = f.manager(vec![f.mock("a", script)]);
    let outline = block_on(m.outline(&f.main_rs())).unwrap();
    assert_eq!(outline[0].selection_range, r(0, 1, 2));
    assert_eq!(outline[0].children.as_ref().unwrap()[0].name, "m");
}

#[test]
fn supertypes_two_step() {
    let f = Fixture::new();
    let other = f.ws.join("src/lib.rs");
    std::fs::write(&other, "trait T {}\n").unwrap();
    let item = |name: &str, p: &Path, l| json!({"name": name, "kind": 5, "uri": uri(p), "range": range(l, 0, 9), "selectionRange": range(l, 6, 7)});
    let script = json!({"capabilities": caps(), "responses": {
        "textDocument/prepareTypeHierarchy": [item("A", &f.main_rs(), 0)],
        "typeHierarchy/supertypes": [item("T", &other, 0)],
    }});
    let (m, _) = f.manager(vec![f.mock("a", script)]);
    let supers = block_on(m.supertypes(&f.main_rs(), Position::new(0, 7))).unwrap();
    assert_eq!(supers.len(), 1);
    assert_eq!(supers[0].name, "T");
    assert_eq!(supers[0].path, other);
    assert_eq!(supers[0].selection_range, r(0, 6, 7));
    let asked = f.received().into_iter().find(|m| m["method"] == "typeHierarchy/supertypes").unwrap();
    assert_eq!(asked["params"]["item"]["name"], "A");
}

#[test]
fn utf16_server_columns_converted() {
    let f = Fixture::new();
    std::fs::write(f.main_rs(), "let a = \"😀\"; x\n").unwrap();
    let u = uri(&f.main_rs());
    // No positionEncoding: UTF-16. Char 13 ('x') is UTF-16 unit 14.
    let script = json!({"capabilities": {"definitionProvider": true}, "responses": {
        "textDocument/definition@0:14": {"uri": u, "range": range(0, 14, 15)},
    }});
    let (m, _) = f.manager(vec![f.mock("a", script)]);
    let got = block_on(m.definition(&f.main_rs(), Position::new(0, 13))).unwrap();
    assert_eq!(got, [Location { path: f.main_rs(), range: r(0, 13, 14) }]);
}

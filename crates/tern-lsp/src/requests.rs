//! The navigation requests, in the editor's terms: paths, and positions in
//! characters (converted to and from the server's encoding).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use futures::StreamExt as _;
use lsp_types::{DocumentSymbol, GotoDefinitionResponse, Position, Range, SymbolInformation, SymbolKind};
use serde_json::{Value, json};

use crate::manager::{Manager, Server};
use crate::text::{Encoding, from_server, to_server};
use crate::uri::uri_to_path;
use crate::{Location, LspError};

/// Implementation requests of one batch in flight at once.
const CONCURRENCY: usize = 4;

/// A type in a type hierarchy.
#[derive(Debug, Clone, PartialEq)]
pub struct HierarchyItem {
    pub name: String,
    pub kind: SymbolKind,
    pub path: PathBuf,
    pub range: Range,
    pub selection_range: Range,
    /// The server's own item, which `typeHierarchy/supertypes` takes back.
    raw: Value,
}

impl HierarchyItem {
    /// An item not from a server (tests, callers' own).
    pub fn new(name: String, kind: SymbolKind, path: PathBuf, range: Range, selection_range: Range) -> Self {
        Self { name, kind, path, range, selection_range, raw: Value::Null }
    }
}

/// A capability, by its key in the server's capabilities.
#[derive(Clone, Copy)]
enum Cap {
    Definition,
    Declaration,
    Implementation,
    DocumentSymbol,
    TypeHierarchy,
}

impl Cap {
    fn key(self) -> &'static str {
        match self {
            Cap::Definition => "definitionProvider",
            Cap::Declaration => "declarationProvider",
            Cap::Implementation => "implementationProvider",
            Cap::DocumentSymbol => "documentSymbolProvider",
            Cap::TypeHierarchy => "typeHierarchyProvider",
        }
    }
}

fn has(server: &Server, cap: Cap) -> bool {
    !matches!(server.caps.get(cap.key()), None | Some(Value::Null) | Some(Value::Bool(false)))
}

/// Lines of files, read once per answer.
#[derive(Default)]
struct Lines(HashMap<PathBuf, Vec<String>>);

impl Lines {
    fn line(&mut self, manager: &Manager, path: &Path, line: u32) -> &str {
        let lines = self
            .0
            .entry(path.to_path_buf())
            .or_insert_with(|| manager.text_of(path).map(|t| t.lines().map(str::to_owned).collect()).unwrap_or_default());
        lines.get(line as usize).map_or("", String::as_str)
    }

    fn range(&mut self, manager: &Manager, path: &Path, r: Range, enc: Encoding) -> Range {
        if enc == Encoding::Utf32 {
            return r;
        }
        let mut pos = |p: Position| Position::new(p.line, from_server(self.line(manager, path, p.line), p.character, enc));
        Range::new(pos(r.start), pos(r.end))
    }
}

impl Manager {
    async fn server_with(&self, path: &Path, cap: Cap) -> Result<std::sync::Arc<Server>, LspError> {
        let server = self.server_for(path).await?;
        if has(&server, cap) { Ok(server) } else { Err(LspError::Unsupported) }
    }

    /// `pos` (characters) in the server's units.
    fn server_pos(&self, server: &Server, path: &Path, pos: Position) -> Value {
        let character = match server.encoding {
            Encoding::Utf32 => pos.character,
            enc => to_server(&self.line_of(path, pos.line), pos.character, enc),
        };
        json!({"line": pos.line, "character": character})
    }

    async fn goto(&self, method: &str, cap: Cap, path: &Path, pos: Position) -> Result<Vec<Location>, LspError> {
        let server = self.server_with(path, cap).await?;
        self.goto_on(&server, method, path, pos).await
    }

    async fn goto_on(&self, server: &Server, method: &str, path: &Path, pos: Position) -> Result<Vec<Location>, LspError> {
        let params = json!({"position": self.server_pos(server, path, pos)});
        let answer = self.request_on(server, path, method, params).await?;
        let locations: Vec<lsp_types::Location> = match serde_json::from_value::<Option<GotoDefinitionResponse>>(answer) {
            Ok(Some(GotoDefinitionResponse::Scalar(l))) => vec![l],
            Ok(Some(GotoDefinitionResponse::Array(ls))) => ls,
            Ok(Some(GotoDefinitionResponse::Link(links))) => {
                links.into_iter().map(|l| lsp_types::Location { uri: l.target_uri, range: l.target_selection_range }).collect()
            },
            Ok(None) => vec![],
            Err(e) => return Err(LspError::Server { code: 0, message: format!("unreadable answer: {e}") }),
        };
        let mut lines = Lines::default();
        let mut out: Vec<Location> = locations
            .into_iter()
            .filter_map(|l| {
                let path = self.local_path(uri_to_path(&l.uri)?);
                let range = lines.range(self, &path, l.range, server.encoding);
                Some(Location { path, range })
            })
            .collect();
        out.sort_by(|a, b| {
            a.path.cmp(&b.path).then((a.range.start.line, a.range.start.character).cmp(&(b.range.start.line, b.range.start.character)))
        });
        out.dedup();
        Ok(out)
    }

    pub async fn definition(&self, path: &Path, pos: Position) -> Result<Vec<Location>, LspError> {
        self.goto("textDocument/definition", Cap::Definition, path, pos).await
    }

    /// Like [`Self::definition`], but without waiting for a server to start
    /// (it is started meanwhile): the caller has another answer (tree-sitter).
    pub async fn definition_now(&self, path: &Path, pos: Position) -> Result<Vec<Location>, LspError> {
        if !self.is_running_for(path) {
            self.start_soon(path);
            return Err(LspError::Unavailable("the language server is starting".into()));
        }
        self.definition(path, pos).await
    }

    pub async fn declaration(&self, path: &Path, pos: Position) -> Result<Vec<Location>, LspError> {
        self.goto("textDocument/declaration", Cap::Declaration, path, pos).await
    }

    pub async fn implementation(&self, path: &Path, pos: Position) -> Result<Vec<Location>, LspError> {
        self.goto("textDocument/implementation", Cap::Implementation, path, pos).await
    }

    /// Implementations of the symbols at each of `positions` (gutter
    /// markers), one list each in order. A position whose request fails gets
    /// none; when every one fails, the first error.
    pub async fn implementations(&self, path: &Path, positions: &[Position]) -> Result<Vec<Vec<Location>>, LspError> {
        let server = self.server_with(path, Cap::Implementation).await?;
        let server = &server;
        let results: Vec<Result<Vec<Location>, LspError>> = futures::stream::iter(positions.iter().copied())
            .map(|pos| async move { self.goto_on(server, "textDocument/implementation", path, pos).await })
            .buffered(CONCURRENCY)
            .collect()
            .await;
        if !results.is_empty() && results.iter().all(Result::is_err) {
            return Err(results.into_iter().find_map(Result::err).expect("all failed"));
        }
        Ok(results.into_iter().map(Result::unwrap_or_default).collect())
    }

    /// The symbols of `path`, nested (a flat answer is nested by container name).
    pub async fn outline(&self, path: &Path) -> Result<Vec<DocumentSymbol>, LspError> {
        let server = self.server_with(path, Cap::DocumentSymbol).await?;
        let answer = self.request_on(&server, path, "textDocument/documentSymbol", json!({})).await?;
        let items = answer.as_array().cloned().unwrap_or_default();
        let hierarchical = items.first().is_some_and(|i| i.get("selectionRange").is_some());
        let mut lines = Lines::default();
        let mut symbols = if hierarchical {
            serde_json::from_value::<Vec<DocumentSymbol>>(Value::Array(items)).unwrap_or_default()
        } else {
            nest(serde_json::from_value::<Vec<SymbolInformation>>(Value::Array(items)).unwrap_or_default())
        };
        if server.encoding != Encoding::Utf32 {
            fn convert(s: &mut DocumentSymbol, f: &mut dyn FnMut(Range) -> Range) {
                s.range = f(s.range);
                s.selection_range = f(s.selection_range);
                for c in s.children.iter_mut().flatten() {
                    convert(c, f);
                }
            }
            for s in &mut symbols {
                convert(s, &mut |r| lines.range(self, path, r, server.encoding));
            }
        }
        Ok(symbols)
    }

    /// The supertypes of the type at `pos`.
    pub async fn supertypes(&self, path: &Path, pos: Position) -> Result<Vec<HierarchyItem>, LspError> {
        let server = self.server_with(path, Cap::TypeHierarchy).await?;
        let params = json!({"position": self.server_pos(&server, path, pos)});
        let prepared = self.request_on(&server, path, "textDocument/prepareTypeHierarchy", params).await?;
        let Some(item) = prepared.as_array().and_then(|a| a.first()).cloned() else { return Ok(vec![]) };
        let answer = server.conn.request("typeHierarchy/supertypes", json!({"item": item}), self.timeouts().request).await?;
        let mut lines = Lines::default();
        Ok(answer
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|raw| {
                let item: lsp_types::TypeHierarchyItem = serde_json::from_value(raw.clone()).ok()?;
                let path = self.local_path(uri_to_path(&item.uri)?);
                Some(HierarchyItem {
                    name: item.name,
                    kind: item.kind,
                    range: lines.range(self, &path, item.range, server.encoding),
                    selection_range: lines.range(self, &path, item.selection_range, server.encoding),
                    path,
                    raw: raw.clone(),
                })
            })
            .collect())
    }
}

/// Flat symbols nested under the symbol their `containerName` names.
#[allow(deprecated)]
fn nest(flat: Vec<SymbolInformation>) -> Vec<DocumentSymbol> {
    let to_doc = |s: &SymbolInformation| DocumentSymbol {
        name: s.name.clone(),
        detail: None,
        kind: s.kind,
        tags: s.tags.clone(),
        deprecated: s.deprecated,
        range: s.location.range,
        selection_range: s.location.range,
        children: None,
    };
    let mut top: Vec<DocumentSymbol> = vec![];
    for s in &flat {
        let parent = s.container_name.as_deref().and_then(|c| top.iter_mut().find(|t| t.name == c));
        match parent {
            Some(p) => p.children.get_or_insert_with(Vec::new).push(to_doc(s)),
            None => top.push(to_doc(s)),
        }
    }
    top
}

/// The super method search, asking the servers (nothing when they can't tell).
impl crate::nav::SuperDeps for Manager {
    async fn supertypes(&self, path: &Path, pos: Position) -> Vec<HierarchyItem> {
        Manager::supertypes(self, path, pos).await.unwrap_or_default()
    }

    async fn outline(&self, path: &Path) -> Vec<DocumentSymbol> {
        Manager::outline(self, path).await.unwrap_or_default()
    }
}

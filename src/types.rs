use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_lsp::lsp_types::{self as lsp, Url as Uri};
use derive_more::{Constructor, Deref, Display, From, Into};
use sha2::{Digest, Sha256};

use crate::builder::Build;
use crate::parser::Token;
use crate::state::State;

#[derive(Debug, Clone, Constructor)]
pub struct BuildWithVersion {
    pub build: Arc<Build>,
    pub version: i32,
}

#[derive(Debug, Clone)]
pub struct Document {
    pub path: Arc<PathBuf>,
    pub build_uri: Arc<Uri>,

    pub source: Arc<Source>,
    pub source_hash: SourceHash,

    #[allow(unused)]
    /// need for tokens lifetime
    pub content: Arc<String>,
    pub buffer: ropey::Rope,
    pub tokens: Arc<Vec<Token<'static>>>,

    pub transpile_hash: TranspileHash,
    pub decl_stmt: Arc<DocumentDeclarationStatement>,
    pub link_stmt: Arc<DocumentLinkStatement>,
}

// TODO: refactor with from SourceMap::Token, LSP Uri (< SourceUri)
#[derive(Debug, Eq, PartialEq, Hash, Clone, From, Into, Deref, Display, Constructor)]
pub struct Source(String);

impl Source {
    pub fn from_path(path: &Path, project: &Path) -> anyhow::Result<Self> {
        let relative = path.strip_prefix(project).map_err(|_| {
            anyhow::anyhow!(
                "Path {} is not relative to project root {}",
                path.display(),
                project.display()
            )
        })?;

        let source_str = relative
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Invalid UTF-8 in path"))?
            .to_lowercase()
            .replace('\\', "/");

        Ok(Source(source_str))
    }
}

#[derive(Debug, Copy, Clone, Deref)]
pub struct TranspileHash(Option<u64>);

impl Hash for TranspileHash {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl PartialEq for TranspileHash {
    fn eq(&self, other: &Self) -> bool {
        self.is_some_and(|s| other.is_some_and(|o| o == s))
    }
}

type DocumentSources<'a> = (
    &'a Vec<Token<'a>>,
    Option<&'a [lsp::TextDocumentContentChangeEvent]>,
);

impl From<DocumentSources<'_>> for TranspileHash {
    fn from(doc_sources: DocumentSources<'_>) -> Self {
        let (tokens, changes) = doc_sources;
        let hasher = &mut fxhash::FxHasher64::default();
        let changes = changes.unwrap_or(&[]);

        for t in tokens.iter() {
            let r_token = match t {
                Token::IncludePath(path_lit) => {
                    let (col, ln) = (path_lit.line_col.col, path_lit.line_col.line);
                    let end_col = col + path_lit.path.len() as u32 + 2;
                    col.hash(hasher);
                    ln.hash(hasher);
                    path_lit.path.hash(hasher);
                    lsp::Range::new(lsp::Position::new(ln, col), lsp::Position::new(ln, end_col))
                }
                Token::RegionOpen(span) | Token::RegionClose(span) => {
                    let (col, ln) = (span.line_col.col, span.line_col.line);
                    let end_col = ln + span.len;
                    col.hash(hasher);
                    ln.hash(hasher);
                    span.len.hash(hasher);
                    lsp::Range::new(lsp::Position::new(ln, col), lsp::Position::new(ln, end_col))
                }
                _ => continue,
            };

            for change in changes {
                let Some(r_change) = change.range.as_ref() else {
                    continue;
                };

                if r_token.start <= r_change.end && r_change.start <= r_token.end {
                    return Self(None);
                }
            }
        }

        Self(hasher.finish().into())
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Deref)]
pub struct SourceHash(u64);

impl SourceHash {
    pub fn new(source: &Source) -> Self {
        Self(fxhash::hash64(source.as_str()))
    }
}

#[derive(Constructor, Clone)]
pub struct SourcePattern<'a> {
    pub lit: &'a str,
    pub source: SourceHash,
}

/// compatibility ECMAScript identifier hash from [`Source`]
#[derive(Debug, Clone, Deref)]
pub struct DocumentIdentifier(Source);

impl DocumentIdentifier {
    pub fn new(source: &Source) -> Self {
        let digest = Sha256::digest(source.as_bytes());
        let hex = hex::encode(digest);
        Self(format!("{:_<width$}", hex, width = source.len()).into())
    }
}

pub const SCRIPT_IDENTIFIER_PREFIX: &str = "$glscript_file_decl_";
const LINK_START_STMT: &str = "\n/** {@link ";
const LEFT_OFFSET: usize = LINK_START_STMT.len();

#[derive(Debug, Clone, Deref)]
pub struct DocumentDeclarationStatement(String);

impl DocumentDeclarationStatement {
    pub fn create(source: &Source, identifier: &DocumentIdentifier) -> Self {
        const DECL_START_STMT: &str = "\n/** @typedef";
        let mut stmt = String::with_capacity(!0u8 as usize);
        stmt.push_str(DECL_START_STMT);
        stmt.push_str(" {'");
        stmt.push_str(source);
        stmt.push_str("'} ");
        stmt.push_str(SCRIPT_IDENTIFIER_PREFIX);
        stmt.push_str(identifier);
        stmt.push_str(" */{};\n");
        Self(stmt)
    }
}

#[derive(Debug, Clone, Deref, Constructor)]
pub struct DocumentLinkStatement {
    pub left_offset: u32,
    pub right_offset: u32,
    #[deref]
    stmt: String,
}

impl DocumentLinkStatement {
    pub fn create(source: &Source, identifier: &DocumentIdentifier) -> Self {
        let right_offset = LEFT_OFFSET + SCRIPT_IDENTIFIER_PREFIX.len() + identifier.len();
        let mut stmt = String::with_capacity(!0u8 as usize);
        stmt.push_str(LINK_START_STMT);
        stmt.push_str(SCRIPT_IDENTIFIER_PREFIX);
        stmt.push_str(identifier);
        stmt.push_str(" '");
        stmt.push_str(source);
        stmt.push_str("'} */{};\n");
        Self::new(LEFT_OFFSET as u32, right_offset as u32, stmt)
    }

    pub fn undefined() -> Self {
        const RIGHT_OFFSET: usize = LEFT_OFFSET + SCRIPT_IDENTIFIER_PREFIX.len() + 1;
        const SUFFIX: &str = "0 '0'} */{};\n";
        let undefined_stmt = LINK_START_STMT.to_owned() + SCRIPT_IDENTIFIER_PREFIX + SUFFIX;
        Self::new(LEFT_OFFSET as u32, RIGHT_OFFSET as u32, undefined_stmt)
    }
}

// TODO: move to builder.rs
pub struct SourceMapBuilder {
    pub tokens: Vec<sourcemap::RawToken>,
    sources: Vec<Arc<Source>>,
    source_map: fxhash::FxHashMap<Arc<Source>, u32>,

    #[cfg(debug_assertions)]
    source_contents: Vec<Option<Arc<str>>>,
}

impl SourceMapBuilder {
    pub fn with_capacity(tokens_capacity: usize, sources_capacity: usize) -> Self {
        Self {
            tokens: Vec::with_capacity(tokens_capacity),
            sources: Vec::with_capacity(sources_capacity),
            source_map: fxhash::FxHashMap::default(),

            #[cfg(debug_assertions)]
            source_contents: Vec::with_capacity(sources_capacity),
        }
    }

    pub fn add_source_with_id(&mut self, source: Arc<Source>) -> u32 {
        let count = self.sources.len() as u32;
        let id = *self.source_map.entry(source.clone()).or_insert(count);
        if id == count {
            self.sources.push(source);
        }
        id
    }

    #[allow(unused_mut)]
    pub fn into_sourcemap(mut self, _state: &State) -> sourcemap::SourceMap {
        let contents;

        #[cfg(debug_assertions)]
        {
            let project = _state.get_project();
            if self.sources.len() > self.source_contents.len() {
                self.source_contents.resize(self.sources.len(), None);
            }
            for (id, source) in self.sources.iter().enumerate() {
                let path = source.as_str();
                let doc_uri = _state.path_to_uri(&project.join(path)).unwrap();
                let contents = _state.get_doc(&doc_uri).unwrap().buffer.to_string();
                self.source_contents[id] = Some(contents.into());
            }

            contents = match self.source_contents.is_empty() {
                false => Some(self.source_contents),
                true => None,
            };
        }

        #[cfg(not(debug_assertions))]
        {
            contents = None
        }

        let sources = self.sources.iter();
        let sources = sources.map(|s| s.as_str().into()).collect();

        let mut sm = sourcemap::SourceMap::new(None, self.tokens, vec![], sources, contents);

        sm.set_source_root(None::<Arc<str>>);
        sm.set_debug_id(None);
        sm
    }
}

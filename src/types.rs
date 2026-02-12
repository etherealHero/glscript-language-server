use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_lsp::lsp_types::{self as lsp, Url as Uri};
use derive_more::{Constructor, Deref, Display, From, Into};
use sha2::{Digest, Sha256};

use crate::builder::Build;
use crate::parser::{Parse, Token};

#[derive(Debug, Clone, Constructor)]
pub struct BuildWithVersion {
    pub build: Arc<Build>,
    pub version: i32,
}

#[derive(Debug, Clone)]
pub struct Document {
    pub path: Arc<PathBuf>,
    pub bundle_uri: Arc<Uri>,
    pub transpile_uri: Arc<Uri>,

    pub source: Arc<Source>,
    pub source_hash: SourceHash,

    pub parse: Arc<Parse<'static>>,
    pub parse_content: Arc<String>, // needs for parse static lifetime
    pub buffer: ropey::Rope,

    pub transpile_hash: TranspileHash,
    pub decl_stmt: Arc<DocumentDeclarationStatement>,
    pub link_stmt: Arc<DocumentLinkStatement>,
}

impl Document {
    pub fn first_non_include_build_pos(&self, build: &Build) -> Option<lsp::Position> {
        self.parse
            .compressed_tokens
            .iter()
            .rposition(|token| matches!(token, Token::IncludePath(_)))
            .map(|include_idx| self.parse.compressed_tokens.get(include_idx + 1))
            .unwrap_or(self.parse.compressed_tokens.first())
            .map(|token| match token {
                Token::Include(_) | Token::IncludePath(_) => unreachable!(),
                Token::RegionOpen(s) | Token::RegionClose(s) => s.line_col.clone(),
                Token::LineTerminator(lc) | Token::Eoi(lc) => lc.clone(),
                Token::Common(rt) | Token::CommonWithLineEnding(rt) => rt.line_col.clone(),
            })
            .map(|line_col| lsp::Position::new(line_col.line, line_col.col))
            .map(|source_pos| build.forward_src_position(&source_pos, &self.source))
            .map(Option::unwrap)
    }
}

// TODO: refactor with from SourceMap::Token, LSP Uri (< SourceUri)
/// must contains lowercase canonicalized strip prefixed path
/// - is used as source in [`sourcemap`]
/// - use to identify the source file
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
                    let end_col = col + path_lit.lit.len() as u32 + 2;
                    col.hash(hasher);
                    ln.hash(hasher);
                    path_lit.lit.hash(hasher);
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

/// compatibility [ECMAScript identifier](https://tc39.es/ecma262/#prod-grammar-notation-Identifier) by [`Source`]
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

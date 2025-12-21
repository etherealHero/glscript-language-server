use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;

use async_lsp::lsp_types::Url as Uri;
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

    pub source: Arc<Source>,
    pub source_ident: Arc<DocumentIdentifier>,
    pub source_hash: SourceHash,

    pub tokens: Arc<Vec<Token>>,
    pub dependency_hash: DependencyHash,
    pub buffer: Arc<ropey::Rope>,

    pub decl_stmt: Arc<DocumentDeclarationStatement>,
    pub link_stmt: Arc<DocumentLinkStatement>,
}

/**
 * Source
 */

// TODO: refactor with from IncludeToken, SourceMap::Token, LSP Uri
#[derive(Debug, Eq, PartialEq, Hash, Clone, From, Into, Deref, Display, Constructor)]
pub struct Source(String);

impl Source {
    pub fn to_uri(&self, st: &State) -> anyhow::Result<Uri> {
        let source_uri = Uri::from_file_path(st.get_project().join(&self.0))
            .map_err(|_| anyhow::Error::msg("invalid source"))?;

        Ok(source_uri)
    }
}

/**
 * DependencyHash
 */

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Deref)]
pub struct DependencyHash(u64);

impl From<&Vec<Token>> for DependencyHash {
    fn from(tokens: &Vec<Token>) -> Self {
        let ref mut hasher = fxhash::FxHasher64::default();

        for t in tokens {
            match t {
                Token::IncludePath(raw_span) => {
                    raw_span.pos.col.hash(hasher);
                    raw_span.pos.line.hash(hasher);
                    raw_span.text.hash(hasher);
                }
                _ => {}
            }
        }

        Self(hasher.finish())
    }
}

impl From<&Vec<DependencyHash>> for DependencyHash {
    fn from(hashes: &Vec<DependencyHash>) -> Self {
        let ref mut hasher = fxhash::FxHasher64::default();
        hashes.iter().for_each(|h| h.hash(hasher));
        Self(hasher.finish())
    }
}

/**
 * SourceHash
 */

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Deref)]
pub struct SourceHash(u64);

impl SourceHash {
    pub fn new(source: &Source) -> Self {
        Self(fxhash::hash64(&**source))
    }
}

/**
 * DocumentIdentifier
 */

/// compatibility ECMAScript identifier hash from [`S ource`]
#[derive(Debug, Clone, Deref)]
pub struct DocumentIdentifier(Source);

impl DocumentIdentifier {
    pub fn new(source: &Source) -> Self {
        let digest = Sha256::digest(source.as_bytes());
        let hex = hex::encode(digest);
        Self(format!("{:_<width$}", hex, width = source.len()).into())
    }
}

/**
 * Statements
 */
pub const IDENTIFIER_PREFIX: &'static str = "$MODULE_";

/**
 * DocumentDeclarationStatement
 */

#[derive(Debug, Clone, Deref)]
pub struct DocumentDeclarationStatement(String);

impl DocumentDeclarationStatement {
    /// returns module declaration statement:
    /// ```js
    /// \n/** @typedef {'%source%'} %identifier% */{};\n
    /// ```
    pub fn new(source: &Source, identifier: &DocumentIdentifier) -> Self {
        const DECL_START_STMT: &'static str = "\n/** @typedef";
        let mut stmt = String::from(DECL_START_STMT);
        stmt.push_str(" {'");
        stmt.push_str(source);
        stmt.push_str("'} ");
        stmt.push_str(IDENTIFIER_PREFIX);
        stmt.push_str(identifier);
        stmt.push_str(" */{};\n");
        Self(stmt)
    }
}

/**
 * DocumentLinkStatement
 */

#[derive(Debug, Clone)]
pub struct DocumentLinkStatement {
    pub left_offset: usize,
    pub right_offset: usize,
    stmt: String,
}

impl DocumentLinkStatement {
    /// returns module link statement:
    /// ```js
    /// \n/** {@link %identifier%} */{};\n
    /// ```
    pub fn new(source: &Source, identifier: &DocumentIdentifier) -> Self {
        const LINK_START_STMT: &'static str = "/** {@link ";
        let left_offset = LINK_START_STMT.len();
        let right_offset = left_offset + IDENTIFIER_PREFIX.len() + identifier.len();
        let mut stmt = String::from("\n");

        stmt.push_str(LINK_START_STMT);
        stmt.push_str(IDENTIFIER_PREFIX);
        stmt.push_str(identifier);
        stmt.push_str(" '");
        stmt.push_str(source);
        stmt.push_str("'} */{};\n");

        Self {
            stmt,
            left_offset,
            right_offset,
        }
    }
}

impl std::ops::Deref for DocumentLinkStatement {
    type Target = str;
    fn deref(&self) -> &Self::Target {
        &self.stmt
    }
}

/**
 * PendingMap
 */

#[derive(Constructor)]
pub struct PendingMap {
    dst_line: usize,
    dst_col: usize,
    src_line: usize,
    src_col: usize,
    source: Option<Arc<Source>>,
}

impl PendingMap {
    pub fn into_sourcemap(maps: &Vec<PendingMap>, _state: &State) -> sourcemap::SourceMap {
        type SrcId = u32;

        let mut smb = sourcemap::SourceMapBuilder::new(None);
        let add = |smb: &mut sourcemap::SourceMapBuilder, m: &PendingMap| -> SrcId {
            let t = smb.add(
                m.dst_line as u32,
                m.dst_col as u32,
                m.src_line as u32,
                m.src_col as u32,
                m.source.as_ref().map(|v| &*v.as_str()),
                None,
                false,
            );

            t.src_id
        };

        #[cfg(debug_assertions)]
        {
            let project = _state.get_project();
            let mut sources = std::collections::HashMap::<u32, Arc<Source>>::new();

            for m in maps {
                let src_id = add(&mut smb, m);
                if let (Some(source), false) = (&m.source, sources.contains_key(&src_id)) {
                    sources.insert(src_id, source.clone());
                }
            }

            for (src_id, source) in sources {
                let ref doc_uri = Uri::from_file_path(project.join(source.as_str())).unwrap();
                let ref contents = _state.get_doc(doc_uri).unwrap().buffer.to_string();
                smb.set_source_contents(src_id, Some(contents));
            }
        }

        #[cfg(not(debug_assertions))]
        {
            for m in maps {
                add(&mut smb, m);
            }
        }

        smb.into_sourcemap()
    }
}

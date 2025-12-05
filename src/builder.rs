use std::collections::{HashSet, hash_map::DefaultHasher};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use pest::Parser;
use pest_derive::Parser;

use async_lsp::lsp_types as lsp;
use async_lsp::lsp_types::Url as Uri;

use sha2::{Digest, Sha256};
use sourcemap::{SourceMap, SourceMapBuilder, Token};

use crate::state::{Source, SourcePath, State, ToSource, ToSourcePath};

pub const BUILD_FILE: &'static str = "build.js.emitted";

#[cfg(debug_assertions)]
pub const BUILD_SOURCEMAP_FILE: &'static str = "build.js.emitted.map";

const DECL_PREFIX: &'static str = "/** @typedef";
const LINK_PREFIX: &'static str = "/** {@link ";
pub const MODULE_PREFIX: &'static str = "$MODULE_";

#[derive(Parser)]
#[grammar = "./glscript_subset_grammar.pest"]
struct GlScriptSubsetGrammar;

#[derive(Clone)]
pub struct Build {
    pub emit_text: String,
    pub emit_uri: Uri,
    pub emit_hash: u64,
    project: SourcePath,
    source_map: SourceMap,
}

impl Build {
    pub fn sources(&self) -> HashSet<Source> {
        self.source_map.sources().map(String::from).collect()
    }

    pub fn forward_src_position(
        &self,
        pos: &lsp::Position,
        pos_source: &Uri,
    ) -> Option<lsp::Position> {
        let mut token: Option<Token> = None;
        let source = &pos_source.source_path().source(&self.project);

        if !self.sources().contains(source) {
            return None;
        }

        for t in self.source_map.tokens() {
            if t.get_source().is_none() || source != t.get_source().expect("has source") {
                continue;
            }
            if t.get_src_line() == pos.line && t.get_src_col() <= pos.character {
                token = Some(t);
            }
            if t.get_src_line() > pos.line {
                break;
            }
        }

        if let Some(t) = token {
            let line = t.get_dst_line();
            let character = t.get_dst_col() + (pos.character - t.get_src_col());
            Some(lsp::Position::new(line, character))
        } else {
            None
        }
    }

    pub fn forward_src_range(&self, range: &lsp::Range, range_source: &Uri) -> Option<lsp::Range> {
        let build_start_pos = self.forward_src_position(&range.start, range_source);
        let build_end_pos = self.forward_src_position(&range.end, range_source);
        match (build_start_pos, build_end_pos) {
            (Some(start), Some(end)) => Some(lsp::Range::new(start, end)),
            _ => None,
        }
    }

    pub fn forward_build_position(&self, pos: &lsp::Position) -> Option<(lsp::Position, Uri)> {
        match self.source_map.lookup_token(pos.line, pos.character) {
            Some(t) if t.get_source().is_none() => return None,
            None => return None,
            Some(t) => {
                let line = t.get_src_line();
                let character = t.get_src_col() + (pos.character - t.get_dst_col());
                let source = t.get_source().expect("forward back token must have source");
                let source_uri =
                    Uri::from_file_path(self.project.join(source)).expect("valid source");
                Some((lsp::Position::new(line, character), source_uri))
            }
        }
    }

    pub fn forward_build_range(&self, range: &lsp::Range) -> Option<(lsp::Range, Uri)> {
        let source_start_pos = self.forward_build_position(&range.start);
        let source_end_pos = self.forward_build_position(&range.end);
        match (source_start_pos, source_end_pos) {
            (Some((start, source)), Some((end, _))) => Some((lsp::Range::new(start, end), source)),
            _ => None,
        }
    }

    pub fn new(state: &State, uri: &Uri) -> anyhow::Result<Self> {
        let mut smb = SourceMapBuilder::new(None);
        let visited_sources = &mut HashSet::<Source>::with_capacity(100);
        let emit_hasher = &mut DefaultHasher::new();
        let project = state.get_project();
        let emit_text = Self::emit(
            state,
            uri,
            project,
            &mut smb,
            &mut 0,
            visited_sources,
            emit_hasher,
        )?;
        let source_map = smb.into_sourcemap();

        #[cfg(debug_assertions)]
        {
            let mut sm_json = Vec::new();
            let _ = source_map.to_writer(&mut sm_json);
            let emitted_source_map = String::from_utf8(sm_json)?;
            let _ = fs::write(project.join(BUILD_SOURCEMAP_FILE), emitted_source_map);
            let build = format!("{emit_text}\n//# sourceMappingURL=/{BUILD_SOURCEMAP_FILE}");
            let _ = fs::write(project.join(BUILD_FILE), build);
        }

        let source_hash = Self::source_hash(&uri.source_path().source(project));
        let source_path = project.join(format!("{source_hash}.js",));
        let emit_uri = Uri::from_file_path(source_path).expect("valid build uri");

        source_hash.hash(emit_hasher);

        let b = Self {
            emit_uri,
            emit_text,
            emit_hash: emit_hasher.finish(),
            project: project.to_owned(),
            source_map,
        };

        Ok(b)
    }
}

impl Build {
    // TODO: rewrite with EmitConfig
    fn emit(
        state: &State,
        uri: &Uri,
        project: &SourcePath,
        smb: &mut SourceMapBuilder,
        dst_line: &mut u32,
        visited: &mut HashSet<String>,
        hasher: &mut impl Hasher,
    ) -> anyhow::Result<String> {
        let path = &uri.try_source_path()?;
        let source = &path.source(project);
        match visited.contains(source) {
            true => return Ok("".into()), // must be empty for emit global doc
            false => visited.insert(source.clone()),
        };

        let global_uri = &state.get_global_doc(); // TODO: create once before emit loop
        let global_text = Self::emit(&state, global_uri, project, smb, dst_line, visited, hasher);

        let raw_text = if let Some(text) = state.get_doc(uri) {
            text
        } else {
            let text = fs::read_to_string(path)?;
            state.set_doc(uri, &text);
            state.get_doc(uri).expect("doc saved in mem")
        };

        assert!(!raw_text.contains("\r\n"));

        // TODO: ? append context prefix with root entry uri if definition failed with other builds
        let ident = Self::source_hash(&source);
        let prepend_module = format!(
            "{}{DECL_PREFIX} {{'{source}'}} {MODULE_PREFIX}{ident} */{{}};\n",
            global_text.unwrap_or_default(),
        );

        ident.hash(hasher);
        smb.add(*dst_line, 0, 0, 0, Some(source), None, false);
        *dst_line += 1; // prepend_module end breakline

        let emitted_source_file = GlScriptSubsetGrammar::parse(self::Rule::SourceFile, &raw_text)?
            .next()
            .ok_or(anyhow::Error::msg("NodeNotFound"))?
            .into_inner()
            .fold(prepend_module, |acc, r| match r.as_rule() {
                self::Rule::IncludeToken => acc,
                self::Rule::IncludePath => {
                    let sp = r.as_span();
                    let line_col = sp.start_pos().line_col();
                    let (line, col) = (line_col.0 as u32 - 1, line_col.1 as u32 - 1);

                    let path_lit = &sp.as_str();
                    let dep_rpath = &path_lit.trim_matches(|c| ['\'', '"', '<', '>'].contains(&c));
                    let dep_path = Self::resolve_path(path, project, dep_rpath);
                    let dep_uri = Uri::from_file_path(&dep_path).expect("valid dep_path");
                    let dep_source = match dep_uri.try_source_path() {
                        Ok(dep_source_path) => dep_source_path.source(project),
                        _ => dep_rpath.to_string(),
                    };
                    let dep_ident = Self::source_hash(&dep_source);
                    format!("{dep_ident}{line}{col}").hash(hasher);

                    let link_ref = format!("\n{LINK_PREFIX}{MODULE_PREFIX}{dep_ident}}} */{{}};\n");
                    let prefix = LINK_PREFIX.len() as u32;
                    let suffix = prefix + MODULE_PREFIX.len() as u32 + dep_ident.len() as u32;
                    let acc_buf = acc + link_ref.as_str() + "\n";

                    *dst_line += 1;
                    smb.add(*dst_line, prefix, line, col, Some(source), None, false);
                    smb.add(*dst_line, suffix, 0, 0, None, None, false);
                    *dst_line += 2;

                    let dep_build =
                        Self::emit(&state, &dep_uri, project, smb, dst_line, visited, hasher);
                    let dep_text = match dep_build {
                        Ok(emitted_dep_text) => {
                            let blk = " ".repeat(col as usize + path_lit.len());
                            format!("{}{}", emitted_dep_text, blk)
                        }
                        _ => "".into(),
                    };

                    acc_buf + dep_text.as_str()
                }
                _ => {
                    let line_col = r.as_span().start_pos().line_col();
                    let (line, col) = (line_col.0 as u32 - 1, line_col.1 as u32 - 1);

                    smb.add(*dst_line, col, line, col, Some(source), None, false);

                    match r.as_rule() {
                        self::Rule::LineTerminator | self::Rule::EOI => {
                            *dst_line += 1;
                            acc + "\n"
                        }
                        _ => acc + r.as_span().as_str(),
                    }
                }
            });

        Ok(emitted_source_file)
    }

    fn resolve_path(module_path: &Path, project_root: &Path, include_literal: &str) -> PathBuf {
        #[inline]
        fn is_relative_path(path: &str) -> bool {
            path.starts_with("./")
                || path.starts_with(".\\")
                || path.starts_with("../")
                || path.starts_with("..\\")
        }

        #[inline]
        fn normalize_path(path: &Path) -> PathBuf {
            let mut buf = PathBuf::new();
            for component in path.components() {
                match component {
                    std::path::Component::ParentDir => {
                        buf.pop().eq(&false).then(|| buf.push(".."));
                    }
                    std::path::Component::CurDir => {}
                    _ => buf.push(component.as_os_str()),
                }
            }
            buf
        }

        let path = include_literal.replace("\\\\", "/").replace("\\", "/");

        if is_relative_path(&path) {
            let module_dir = module_path.parent().unwrap_or(project_root);
            normalize_path(&module_dir.join(path))
        } else {
            normalize_path(&project_root.join(path))
        }
    }

    /// compatibility ECMAScript identifier hash from [`Source`]
    #[inline]
    fn source_hash(source: &Source) -> String {
        let digest = Sha256::digest(source.as_bytes());
        let hex = hex::encode(digest);
        let hash = format!("{:_<width$}", hex, width = &source.len());

        hash
    }
}

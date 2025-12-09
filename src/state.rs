use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use crate::builder::Build;
use crate::parser::{Token, parse};

use async_lsp::lsp_types::Url as Uri;
use dashmap::DashMap;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug)]
pub struct BuildWithVersion {
    pub build: Build,
    pub version: i32,
}

#[derive(Debug)]
pub struct Document {
    pub text: Arc<String>,
    pub tokens: Arc<Vec<Token>>,
}

#[derive(Default, Debug)]
pub struct State {
    project_path: Arc<OnceLock<SourcePath>>,
    global_document: Arc<OnceLock<Uri>>,
    documents: DashMap<SourcePath, Document>,
    builds: DashMap<SourcePath, BuildWithVersion>,

    // cache storages
    // TODO: refactor with DashMap<Uri, {source_hash, source_path, real_casesensetive_syspath}>
    source_to_hash: DashMap<Source, String>,
    uri_to_source_path: DashMap<Uri, SourcePath>,
    source_path_to_source: DashMap<SourcePath, Source>,
}

pub type SourcePath = PathBuf;

pub trait ToSourcePath {
    fn source_path(&self) -> SourcePath;
}

impl ToSourcePath for Uri {
    #[inline]
    fn source_path(&self) -> SourcePath {
        let err = format!("Expected valid input Uri from Language Services, but found: {self}");
        dunce::canonicalize(self.to_file_path().expect(&err)).expect(&err)
    }
}

pub trait Canonicalize {
    fn canonicalize(&self) -> Self;
}

impl Canonicalize for Uri {
    fn canonicalize(&self) -> Self {
        Uri::from_file_path(self.to_file_path().expect("valid filepath")).expect("valid filepath")
    }
}

pub type Source = String;

pub trait ToSource {
    fn source(&self, root: &SourcePath) -> Source;
}

impl ToSource for SourcePath {
    fn source(&self, root: &SourcePath) -> Source {
        self.strip_prefix(root)
            .expect("existed source of project")
            .to_str()
            .expect("existed source of project")
            .to_lowercase()
            .replace('\\', "/")
    }
}

impl State {
    pub fn set_build(&self, source_uri: &Uri) -> BuildWithVersion {
        let new_build = Build::new(&self, source_uri).expect("build success");
        let path = &self.uri_to_source_path(source_uri).unwrap();

        match self.builds.get_mut(path) {
            Some(mut b) => {
                b.build = new_build;
                b.version += 1;
            }
            None => {
                let b = BuildWithVersion {
                    build: new_build,
                    version: 1,
                };
                self.builds.insert(path.into(), b);
            }
        }

        self.builds
            .get(path)
            .map(|guard| guard.clone())
            .expect("build saved")
    }

    pub fn get_build(&self, source_uri: &Uri) -> Option<Build> {
        self.builds
            .get(&self.uri_to_source_path(source_uri).unwrap())
            .map(|guard| guard.build.clone())
    }

    pub fn get_build_by_emit_uri(&self, emit_uri: &Uri) -> Option<Build> {
        let emit_uri_canonicalized = emit_uri.canonicalize();
        self.builds
            .iter()
            .find(|e| e.build.emit_uri.canonicalize() == emit_uri_canonicalized)
            .map(|e| e.build.clone())
    }

    /// returns SourcePath for canonicalize interface
    pub fn get_builds_contains_document(&self, source_uri: &Uri) -> Vec<SourcePath> {
        let source_path = &self.uri_to_source_path(source_uri).unwrap();
        let source = &self.source_path_to_source(source_path).unwrap();
        self.builds
            .iter()
            .filter(|e| e.value().build.sources().contains(source))
            .map(|e| e.key().clone())
            .collect()
    }

    pub fn set_doc(&self, source_uri: &Uri, text: &String) {
        let text = Arc::new(text.replace("\r\n", "\n").replace("\r", ""));
        let tokens = Arc::new(parse(&text));
        let sp = self.uri_to_source_path(source_uri).unwrap();
        self.documents.insert(sp, Document { tokens, text });
    }

    pub fn get_doc(&self, source_uri: &Uri) -> Option<Arc<String>> {
        let sp = &self.uri_to_source_path(source_uri).unwrap();
        self.documents.get(sp).map(|guard| guard.text.clone())
    }

    // TODO: change source_uri to struct ProjectUri which must be valid
    pub fn get_doc_tokens(&self, source_uri: &Uri) -> Arc<Vec<Token>> {
        let source_path = &self.uri_to_source_path(source_uri).unwrap();
        if let Some(doc) = self.documents.get(source_path) {
            return doc.tokens.clone();
        };

        let text = &fs::read_to_string(source_path).expect("content of real source uri");
        self.set_doc(source_uri, text);
        self.get_doc_tokens(source_uri)
    }

    pub fn set_project(&self, source_uri: &Uri) {
        let sp = self.uri_to_source_path(source_uri).unwrap();
        self.project_path.set(sp).expect("project set once");
    }

    pub fn get_project(&self) -> &SourcePath {
        self.project_path.get().expect("project installed")
    }

    pub fn set_global_doc(&self, source_uri: Uri) {
        self.global_document
            .set(source_uri)
            .expect("global_document set once");
    }

    // FIXME: if global doc invalid or not installed ? change with constant global.js file
    pub fn get_global_doc(&self) -> Option<Uri> {
        self.global_document.get().cloned()
    }
}

impl State {
    /// compatibility ECMAScript identifier hash from [`Source`]
    #[inline]
    pub fn source_to_hash(&self, source: &Source) -> String {
        if let Some(hash) = self.source_to_hash.get(source) {
            return hash.clone();
        }

        let digest = Sha256::digest(source.as_bytes());
        let hex = hex::encode(digest);
        let hash = format!("{:_<width$}", hex, width = &source.len());

        self.source_to_hash.insert(source.to_owned(), hash.clone());

        hash
    }

    #[inline]
    pub fn uri_to_source_path(&self, uri: &Uri) -> anyhow::Result<SourcePath> {
        if let Some(source_path) = self.uri_to_source_path.get(uri) {
            return Ok(source_path.clone());
        }

        let sp = uri.to_file_path();
        let sp = sp.map_err(|_| anyhow::anyhow!("uri to file path fail: {uri}"))?;
        let sp = dunce::canonicalize(sp)?;

        self.uri_to_source_path.insert(uri.clone(), sp.clone());
        Ok(sp)
    }

    #[inline]
    pub fn source_path_to_source(&self, source_path: &SourcePath) -> anyhow::Result<Source> {
        if let Some(source) = self.source_path_to_source.get(source_path) {
            return Ok(source.clone());
        }

        let source = source_path.source(self.get_project());
        self.source_path_to_source
            .insert(source_path.clone(), source.clone());

        Ok(source)
    }
}

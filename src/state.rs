use async_lsp::lsp_types::Url as Uri;
use dashmap::DashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use crate::builder::Build;

#[derive(Clone)]
pub struct BuildWithVersion {
    pub build: Build,
    pub version: i32,
}

#[derive(Default)]
pub struct State {
    project_path: Arc<OnceLock<SourcePath>>,
    global_document: Arc<OnceLock<Uri>>,
    documents: Arc<DashMap<SourcePath, String>>,
    builds: Arc<DashMap<SourcePath, BuildWithVersion>>,
}

pub type SourcePath = PathBuf;

pub trait ToSourcePath {
    fn source_path(&self) -> SourcePath;
    fn try_source_path(&self) -> anyhow::Result<SourcePath>;
}

impl ToSourcePath for Uri {
    fn source_path(&self) -> SourcePath {
        let err = format!("Expected valid input Uri from Language Services, but found: {self}");
        dunce::canonicalize(self.to_file_path().expect(&err)).expect(&err)
    }

    fn try_source_path(&self) -> anyhow::Result<SourcePath> {
        let source_path = self
            .to_file_path()
            .map_err(|_| anyhow::anyhow!("invalid file uri: {self}"))?;
        let source_path = dunce::canonicalize(source_path)?;

        Ok(source_path)
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
        let new_build = Build::build(&self, source_uri).expect("build success");
        let path = &source_uri.source_path();

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
            .get(&source_uri.source_path())
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
        let source = &source_uri.source_path().source(&self.get_project());
        self.builds
            .iter()
            .filter(|e| e.value().build.sources().contains(source))
            .map(|e| e.key().clone())
            .collect()
    }

    pub fn set_doc(&self, source_uri: &Uri, text: &String) {
        let text = text.replace("\r\n", "\n").replace("\r", "");
        self.documents.insert(source_uri.source_path(), text);
    }

    pub fn get_doc(&self, source_uri: &Uri) -> Option<String> {
        let path = &source_uri.source_path();
        self.documents.get(path).map(|guard| guard.clone())
    }

    pub fn set_project(&self, source_uri: &Uri) {
        self.project_path
            .set(source_uri.source_path())
            .expect("project set once");
    }

    pub fn get_project(&self) -> &SourcePath {
        self.project_path.get().expect("project installed")
    }

    pub fn set_global_doc(&self, source_uri: Uri) {
        self.global_document
            .set(source_uri)
            .expect("global_document set once");
    }

    pub fn get_global_doc(&self) -> Uri {
        self.global_document
            .get()
            .expect("global_document installed")
            .to_owned()
    }
}

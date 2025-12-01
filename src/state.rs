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
        self.to_file_path().expect(&err).canonicalize().expect(&err)
    }

    fn try_source_path(&self) -> anyhow::Result<SourcePath> {
        let source_path = self
            .to_file_path()
            .map_err(|_| anyhow::anyhow!("invalid file uri: {self}"))?
            .canonicalize()?;

        Ok(source_path)
    }
}

pub type Source = String;

pub trait ToSource {
    fn source(&self, root: &SourcePath) -> Source;
}

impl ToSource for SourcePath {
    fn source(&self, root: &SourcePath) -> Source {
        self.strip_prefix(root)
            .unwrap()
            .to_str()
            .unwrap()
            .to_lowercase()
    }
}

impl State {
    pub fn set_build(&self, uri: &Uri) -> BuildWithVersion {
        let new_build = Build::build(&self, uri).unwrap();
        let path = &uri.source_path();

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

        self.builds.get(path).map(|guard| guard.clone()).unwrap()
    }

    pub fn get_build(&self, uri: &Uri) -> Option<Build> {
        self.builds
            .get(&uri.source_path())
            .map(|guard| guard.build.clone())
    }

    /// returns SourcePath for canonicalize interface
    pub fn get_builds_contains_document(&self, uri: &Uri) -> Vec<SourcePath> {
        let source = &uri.source_path().source(&self.get_project());
        self.builds
            .iter()
            .filter(|e| e.value().build.sources().contains(source))
            .map(|e| e.key().clone())
            .collect()
    }

    pub fn set_doc(&self, uri: &Uri, text: &String) {
        let text = text.replace("\r\n", "\n").replace("\r", "");
        self.documents.insert(uri.source_path(), text);
    }

    pub fn get_doc(&self, uri: &Uri) -> Option<String> {
        let path = &uri.source_path();
        self.documents.get(path).map(|guard| guard.clone())
    }

    pub fn set_project(&self, uri: &Uri) {
        self.project_path.set(uri.source_path()).unwrap();
    }

    pub fn get_project(&self) -> &SourcePath {
        self.project_path.get().unwrap()
    }
}

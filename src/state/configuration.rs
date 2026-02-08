use std::path::PathBuf;

use async_lsp::lsp_types::Url as Uri;
use async_lsp::{ClientSocket, lsp_types as lsp};

use crate::proxy::{Canonicalize, DEFAULT_SCRIPT_FILENAME, PROXY_WORKSPACE};
use crate::proxy::{DECL_FILE_EXT, JS_FILE_EXT};
use crate::state::State;

/// State of configuration
impl State {
    pub fn initialize_project(
        &self,
        source_uri: &Uri,
        token_types: Option<Vec<lsp::SemanticTokenType>>,
    ) {
        let path = self.uri_to_path(source_uri).unwrap();
        let msg = "project initialize once";
        let ident = lsp::NumberOrString::String("glscript".into());

        if let Some(types) = token_types {
            self.token_types_capabilities.set(types).expect(msg);
        }

        self.project.set(path).expect(msg);
        self.work_done_progress_token.set(ident).expect(msg);

        // TODO: configure in client on release
        self.diagnostics_compatibility.set(false).expect(msg);
    }

    pub fn get_project(&self) -> &PathBuf {
        self.project.get().expect("project initialized")
    }

    pub fn get_default_doc(&self) -> Uri {
        let path = self.project.get().unwrap();
        let path = path.join(PROXY_WORKSPACE).join(DEFAULT_SCRIPT_FILENAME);
        let default_doc = self.path_to_uri(&path);

        default_doc.unwrap_or(Uri::from_file_path(path).unwrap().canonicalize().unwrap())
    }

    pub fn get_token_types_capabilities(&self) -> Option<&Vec<lsp::SemanticTokenType>> {
        self.token_types_capabilities.get()
    }

    pub fn is_diagnostics_enabled(&self) -> bool {
        *(self.diagnostics_compatibility.get().unwrap_or(&false))
    }

    pub fn tsserver_initialized(&self) -> bool {
        *self.tsserver_initialized.get().unwrap_or(&false)
    }

    /// must called once after initialization of proxy and tsserver
    #[tracing::instrument(skip_all)]
    pub async fn index_project(&self, mut client: ClientSocket) {
        use ignore::Walk;

        self.create_progress(&mut client).await;
        tokio::time::sleep(tokio::time::Duration::from_nanos(1)).await;

        let project = self.get_project();
        let (js, decl) = (&JS_FILE_EXT[1..], &DECL_FILE_EXT[1..]);
        let mut raw_entries = vec![];

        for entry in Walk::new(project).flatten() {
            if entry.file_type().is_some_and(|ft| ft.is_file()) {
                let path = entry.path().to_owned();

                if !path.extension().is_some_and(|ext| ext == js || ext == decl) {
                    continue;
                }

                raw_entries.push(entry.path().to_owned());
            }
        }

        let raw_entries_len = raw_entries.len();
        let mut last_percentage = 0u32;

        for (i, p) in raw_entries.iter().enumerate() {
            let uri = self.path_to_uri(p.as_path()).ok();

            if uri.is_none() {
                continue;
            }

            if let Some(doc) = uri.as_ref().and_then(|u| self.get_doc(u).ok()) {
                let percentage = ((i + 1) * 100 / raw_entries_len) as u32;
                if percentage != last_percentage {
                    tokio::time::sleep(tokio::time::Duration::from_nanos(1)).await;
                    let msg = &format!("indexing: {}", doc.source);
                    self.send_progress(&mut client, (i, raw_entries_len), msg);
                    last_percentage = percentage;
                }
            };
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

        self.destroy_progress(&mut client);
        self.tsserver_initialized.set(true).unwrap();
    }
}

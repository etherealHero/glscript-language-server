use async_lsp::lsp_types::{Url as Uri, notification as N, request as R};
use async_lsp::{LanguageServer, lsp_types as lsp};

use crate::proxy::language_server::{Error, NotifyResult};
use crate::proxy::{PROXY_WORKSPACE, Proxy, ResFut};

pub fn initialize(this: &mut Proxy, mut params: lsp::InitializeParams) -> ResFut<R::Initialize> {
    const JSCONFIG: &str = "jsconfig.json";

    // FIXME: if node_modules not installed show user error
    // FIXME: if client not has workspace_folders show client Error message for open Project Folder
    // TODO: if workspace_folders.len() > 2 show error for unsupported
    if let Some([root_ws, ..]) = params.workspace_folders.as_deref_mut() {
        let ws_dir = &root_ws.uri.to_file_path().unwrap();
        let proxy_ws_dir = &mut ws_dir.clone().join(PROXY_WORKSPACE);
        let jsconfig_content = std::fs::read(ws_dir.join(JSCONFIG))
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap()
            .replace("./node_modules/@types", "../../node_modules/@types");
        let token_types = params
            .capabilities
            .text_document
            .as_ref()
            .map(|d| d.semantic_tokens.as_ref().map(|s| s.token_types.clone()))
            .map(Option::unwrap);

        std::fs::create_dir_all(&proxy_ws_dir).unwrap();
        std::fs::write(proxy_ws_dir.join(JSCONFIG), jsconfig_content).unwrap();

        this.state.initialize_project(&root_ws.uri, token_types);

        let default_doc = this.state.get_default_doc();
        let _ = std::fs::File::create_new(default_doc.to_file_path().unwrap());

        this.state.set_bundle(&default_doc).unwrap();

        root_ws.uri = Uri::from_directory_path(proxy_ws_dir).unwrap();
    }

    #[allow(deprecated)]
    {
        params.root_path = None;
        params.root_uri = None;
    }

    let mut service = this.server();
    Box::pin(async move { service.initialize(params).await.map_err(Error::internal) })
}

pub fn initialized(this: &mut Proxy, params: lsp::InitializedParams) -> NotifyResult {
    let _ = this.server().initialized(params);
    std::ops::ControlFlow::Continue(())
}

pub fn shutdown(this: &mut Proxy, (): <R::Shutdown as R::Request>::Params) -> ResFut<R::Shutdown> {
    let mut service = this.server();
    Box::pin(async move {
        let _ = service.shutdown(()).await;
        Ok(())
    })
}

pub fn exit(this: &mut Proxy, (): <N::Exit as N::Notification>::Params) -> NotifyResult {
    let _ = this.server().exit(());
    std::ops::ControlFlow::Break(Ok(()))
}

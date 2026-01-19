use async_lsp::lsp_types::{Url as Uri, request as R};
use async_lsp::{LanguageServer, lsp_types as lsp};

use crate::language_server::{Error, NotifyResult};
use crate::proxy::PROXY_WORKSPACE;
use crate::proxy::{Proxy, ResFut};

pub fn initialize(this: &mut Proxy, mut params: lsp::InitializeParams) -> ResFut<R::Initialize> {
    const JSCONFIG: &str = "jsconfig.json";

    // FIXME: if node_modules not installed show user error
    // FIXME: if client not has workspace_folders show client Error message for open Project Folder
    // TODO: if workspace_folders.len() > 2 show error for unsupported
    if let Some([root_ws, ..]) = params.workspace_folders.as_deref_mut() {
        let ws_dir = &root_ws.uri.to_file_path().unwrap();
        let proxy_ws_dir = &mut ws_dir.clone().join(PROXY_WORKSPACE);

        std::fs::create_dir_all(&proxy_ws_dir).unwrap();
        std::fs::copy(ws_dir.join(JSCONFIG), proxy_ws_dir.join(JSCONFIG)).unwrap();

        this.state.initialize_project(&root_ws.uri);

        let _ = std::fs::File::create_new(this.state.get_default_doc().to_file_path().unwrap());

        this.state.set_build(&this.state.get_default_doc()).unwrap();

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

pub fn shutdown(this: &mut Proxy) -> ResFut<R::Shutdown> {
    let mut service = this.server();
    Box::pin(async move {
        let _ = service.shutdown(()).await;
        Ok(())
    })
}

pub fn exit(this: &mut Proxy) -> NotifyResult {
    let _ = this.server().exit(());
    std::ops::ControlFlow::Break(Ok(()))
}

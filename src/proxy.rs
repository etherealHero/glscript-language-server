use derive_more::Constructor;
use futures::future::BoxFuture;
use tower::ServiceBuilder;

use async_lsp::lsp_types::{Url as Uri, request::Request};
use async_lsp::router::Router;
use async_lsp::{ClientSocket, ResponseError, ServerSocket};

use crate::proxy::language_server::init_language_server_router;
use crate::state::State;
use forward_layer::{ForwardingLayer, TService};

mod forward_layer;
mod language_client;
mod language_server;
mod macros;

pub const JS_LANG_ID: &str = "javascript";
pub const JS_FILE_EXT: &str = ".js";
pub const DECL_FILE_EXT: &str = ".d.ts";
pub const PROXY_WORKSPACE: &str = "./.local/gls-proxy-workspace";
pub const DEFAULT_SCRIPT_FILENAME: &str = "DEFAULT_INCLUDED.js";
pub const DEFAULT_TIMEOUT_MS: u64 = 5000;

pub type ResFut<R> = BoxFuture<'static, Result<<R as Request>::Result, ResponseError>>;
pub type ResReqProxy<R> = Result<<R as Request>::Result, ResponseError>;

pub trait Canonicalize {
    fn canonicalize(&self) -> anyhow::Result<Self>
    where
        Self: std::marker::Sized;

    fn try_canonicalize(&self) -> Self;
}

impl Canonicalize for Uri {
    fn canonicalize(&self) -> anyhow::Result<Self> {
        let msg = "uri canonicalize failed";
        let path = self.to_file_path().map_err(|_| anyhow::Error::msg(msg))?;
        Uri::from_file_path(path).map_err(|_| anyhow::Error::msg(msg))
    }

    fn try_canonicalize(&self) -> Self {
        self.canonicalize().unwrap_or_else(|_| self.to_owned())
    }
}

#[derive(Default, Clone, Constructor)]
pub struct Proxy {
    client: std::sync::Arc<std::sync::OnceLock<ClientSocket>>,
    server: std::sync::Arc<std::sync::OnceLock<ServerSocket>>,
    pub state: std::sync::Arc<State>,
}

impl Proxy {
    pub fn server(&self) -> ServerSocket {
        self.server.get().expect("server socket linked").clone()
    }

    pub fn client(&self) -> ClientSocket {
        self.client.get().expect("client socket linked").clone()
    }

    pub fn init(
        server: std::sync::Arc<std::sync::OnceLock<ServerSocket>>,
        client: std::sync::Arc<std::sync::OnceLock<ClientSocket>>,
    ) -> (impl TService<Future: Send>, impl TService<Future: Send>) {
        let proxy = Self::new(client, server, std::sync::Arc::new(State::default()));
        let sr = init_language_server_router(proxy.clone());
        let cr = Router::from_language_client(proxy);
        let server = ServiceBuilder::new().layer(ForwardingLayer).service(sr);
        let client = ServiceBuilder::new().layer(ForwardingLayer).service(cr);
        // .layer(async_lsp::tracing::TracingLayer::default())
        (server, client)
    }
}

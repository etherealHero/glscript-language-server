use derive_more::{Constructor, Display};
use futures::future::BoxFuture;
use tower::ServiceBuilder;

use async_lsp::lsp_types::{self as lsp, Url as Uri, request::Request};
use async_lsp::{ClientSocket, ErrorCode, ResponseError, ServerSocket};

use crate::builder::Build;
use crate::proxy::language_client::init_language_client_router;
use crate::proxy::language_server::init_language_server_router;
use crate::state::State;
use crate::types::Source;
use forward_layer::{ForwardingLayer, TService};
pub use tracing_formatter::Formatter;

mod forward_layer;
mod language_client;
mod language_server;
mod macros;
mod tracing_formatter;

pub const JS_LANG_ID: &str = "javascript";
pub const JS_FILE_EXT: &str = ".js";
pub const DECL_FILE_EXT: &str = ".d.ts";
pub const PROXY_WORKSPACE: &str = "./.local/glproxy-workspace";
pub const DEFAULT_SCRIPT_FILENAME: &str = "DEFAULT_INCLUDED.js";
pub const DEFAULT_TIMEOUT_MS: u64 = 5000;

pub type ResFut<R> = BoxFuture<'static, Result<<R as Request>::Result, ResponseError>>;
pub type ResReqProxy<R> = Result<<R as Request>::Result, ResponseError>;
pub type NotifyResult = std::ops::ControlFlow<async_lsp::Result<()>>;

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
        let server = init_language_server_router(proxy.clone());
        let client = init_language_client_router(proxy);
        let init = || {
            ServiceBuilder::new()
                .layer(async_lsp::tracing::TracingLayer::default())
                .layer(ForwardingLayer)
        };
        (init().service(server), init().service(client))
    }
}

#[derive(Display)]
pub struct Error {}

impl Error {
    pub fn internal(e: impl core::fmt::Display) -> ResponseError {
        ResponseError::new(ErrorCode::INTERNAL_ERROR, e)
    }

    pub fn request_failed(e: impl core::fmt::Display) -> ResponseError {
        ResponseError::new(ErrorCode::REQUEST_FAILED, e)
    }

    pub fn unexpected_source() -> ResponseError {
        use crate::proxy::{DECL_FILE_EXT, JS_FILE_EXT};

        let reason = "Missmatched source extension";
        let err = format!("{reason}, expect '{JS_FILE_EXT}' or '{DECL_FILE_EXT}'. Request aborted");
        ResponseError::new(ErrorCode::REQUEST_FAILED, err)
    }

    #[tracing::instrument(skip_all)]
    pub fn forward_failed() -> ResponseError {
        ResponseError::new(ErrorCode::REQUEST_FAILED, "Forward failed")
    }

    pub fn unbuild_fallback() -> ResponseError {
        let message = "Build not found, fallback request...";
        ResponseError::new(ErrorCode::INTERNAL_ERROR, message)
    }
}

pub fn forward_build_range(range: &mut lsp::Range, build: &Build) -> Result<Source, ResponseError> {
    let source_range = build.forward_build_range(range);
    if source_range.is_none() {
        return Err(Error::forward_failed());
    }
    let source_range = source_range.expect("is some");
    *range = source_range.0;
    Ok(source_range.1)
}

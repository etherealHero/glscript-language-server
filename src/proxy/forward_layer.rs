use std::future::Future;
use std::ops::ControlFlow;
use std::pin::Pin;
use std::task::{Context, Poll};

use pin_project_lite::pin_project;
use tower_layer::Layer;
use tower_service::Service;

use async_lsp::{AnyEvent, AnyNotification, AnyRequest, LspService};
use async_lsp::{ErrorCode, ResponseError};

pub trait TService:
    LspService + Service<AnyRequest, Response = serde_json::Value, Error = ResponseError> + Send
where
    Self::Future: Send + 'static,
{
}

impl<T> TService for T
where
    T: LspService + Service<AnyRequest, Response = serde_json::Value, Error = ResponseError> + Send,
    T::Future: Send + 'static,
{
}

pub struct ForwardingLayer;

impl<S> Layer<S> for ForwardingLayer {
    type Service = ForwardingMiddleware<S>;
    fn layer(&self, inner: S) -> Self::Service {
        ForwardingMiddleware { inner }
    }
}

pub struct ForwardingMiddleware<S> {
    pub inner: S,
}

impl<S: TService<Future: Send> + 'static> Service<AnyRequest> for ForwardingMiddleware<S> {
    type Response = S::Response;
    type Error = S::Error;
    type Future = ForwardingFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: AnyRequest) -> Self::Future {
        let method = req.method.clone();
        ForwardingFuture {
            method,
            fut: self.inner.call(req),
        }
    }
}

pin_project! {
    pub struct ForwardingFuture<Fut> {
        method: String,
        #[pin]
        fut: Fut,
    }
}

impl<Fut> Future for ForwardingFuture<Fut>
where
    Fut: Future<Output = Result<serde_json::Value, ResponseError>>,
{
    type Output = Fut::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        match this.fut.poll(cx) {
            Poll::Ready(Ok(result_req)) => {
                // tracing::info!((this.method, &result_req));
                Poll::Ready(Ok(result_req))
            }
            Poll::Ready(Err(unimpl_req)) if unimpl_req.code == ErrorCode::METHOD_NOT_FOUND => {
                tracing::warn!("unimplemented");
                Poll::Ready(Ok(serde_json::Value::Null))
            }
            Poll::Ready(Err(fail_req)) => {
                tracing::error!("failed request {}: {}", this.method, &fail_req);
                Poll::Ready(Err(fail_req))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<S: TService<Future: Send> + 'static> LspService for ForwardingMiddleware<S> {
    fn notify(&mut self, notif: AnyNotification) -> ControlFlow<async_lsp::Result<()>> {
        let result = self.inner.notify(notif);
        match &result {
            ControlFlow::Break(Err(async_lsp::Error::Routing(_))) => {
                tracing::warn!("unimplemented");
                ControlFlow::Continue(())
            }
            ControlFlow::Break(_) | ControlFlow::Continue(_) => result,
        }
    }

    fn emit(&mut self, event: AnyEvent) -> ControlFlow<async_lsp::Result<()>> {
        self.inner.emit(event)
    }
}

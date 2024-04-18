//! Method routing that closely mimics [`axum::routing`] while extending
//! it with API documentation-specific features..

use std::{convert::Infallible, mem};

use crate::{
    gen::GenContext,
    openapi::{Operation, PathItem, ReferenceOr, Response, StatusCode},
    Error,
};
use axum::{
    body::{Body, HttpBody},
    handler::Handler,
    response::IntoResponse,
    routing::{self, MethodRouter, Route},
    BoxError,
};
use bytes::Bytes;
use http::Request;
use indexmap::IndexMap;
use tower_layer::Layer;
use tower_service::Service;

use crate::{
    gen::in_context,
    operation::{OperationHandler, OperationInput, OperationOutput},
    transform::TransformOperation,
};

/// A wrapper over [`axum::routing::MethodRouter`] that adds
/// API documentation-specific features.
#[must_use]
pub struct ApiMethodRouter<S = (), B = Body, E = Infallible> {
    pub(crate) operations: IndexMap<&'static str, Operation>,
    pub(crate) router: MethodRouter<S, B, E>,
}

impl<S, B, E> From<ApiMethodRouter<S, B, E>> for MethodRouter<S, B, E> {
    fn from(router: ApiMethodRouter<S, B, E>) -> Self {
        router.router
    }
}

impl<S, B, E> From<MethodRouter<S, B, E>> for ApiMethodRouter<S, B, E> {
    fn from(router: MethodRouter<S, B, E>) -> Self {
        Self {
            operations: IndexMap::default(),
            router,
        }
    }
}

impl<S, B, E> ApiMethodRouter<S, B, E> {
    pub(crate) fn take_path_item(&mut self) -> PathItem {
        let mut path = PathItem::default();

        for (method, op) in mem::take(&mut self.operations) {
            match method {
                "delete" => path.delete = Some(op),
                "get" => path.get = Some(op),
                "head" => path.head = Some(op),
                "options" => path.options = Some(op),
                "patch" => path.patch = Some(op),
                "post" => path.post = Some(op),
                "put" => path.put = Some(op),
                "trace" => path.trace = Some(op),
                _ => unreachable!(),
            }
        }

        path
    }
}

macro_rules! method_router_chain_method {
    ($name:ident, $name_with:ident) => {
        #[doc = concat!("Route `", stringify!($name) ,"` requests to the given handler. See [`axum::routing::MethodRouter::", stringify!($name) , "`] for more details.")]
        pub fn $name<H, I, O, T>(mut self, handler: H) -> Self
        where
            H: Handler<T, S, B> + OperationHandler<I, O>,
            I: OperationInput,
            O: OperationOutput,
            B: Send + 'static,
            T: 'static,
        {
            let mut operation = Operation::default();
            in_context(|ctx| {
                I::operation_input(ctx, &mut operation);

                for (code, res) in O::inferred_responses(ctx, &mut operation) {
                    set_inferred_response(ctx, &mut operation, code, res);
                }
            });
            self.operations.insert(stringify!($name), operation);
            self.router = self.router.$name(handler);
            self
        }

        #[doc = concat!("Route `", stringify!($name) ,"` requests to the given handler. See [`axum::routing::MethodRouter::", stringify!($name) , "`] for more details.")]
        ///
        /// This method additionally accepts a transform function,
        /// see [`crate::axum`] for more details.
        pub fn $name_with<H, I, O, T, F>(mut self, handler: H, transform: F) -> Self
        where
            H: Handler<T, S, B> + OperationHandler<I, O>,
            I: OperationInput,
            O: OperationOutput,
            B: Send + 'static,
            T: 'static,
            F: FnOnce(TransformOperation) -> TransformOperation,
        {
            let mut operation = Operation::default();
            in_context(|ctx| {
                I::operation_input(ctx, &mut operation);

                if ctx.infer_responses {
                    for (code, res) in O::inferred_responses(ctx, &mut operation) {
                        set_inferred_response(ctx, &mut operation, code, res);
                    }

                    // On conflict, input early responses potentially overwrite
                    // output inferred responses on purpose, as they
                    // are stronger in a sense that the request won't
                    // even reach the handler body.
                    for (code, res) in I::inferred_early_responses(ctx, &mut operation) {
                        set_inferred_response(ctx, &mut operation, code, res);
                    }
                }
            });

            let t = transform(TransformOperation::new(&mut operation));

            if !t.hidden {
                self.operations.insert(stringify!($name), operation);
            }

            self.router = self.router.$name(handler);
            self
        }
    };
}

macro_rules! method_router_top_level {
    ($name:ident, $name_with:ident) => {
        #[doc = concat!("Route `", stringify!($name) ,"` requests to the given handler. See [`axum::routing::", stringify!($name) , "`] for more details.")]
        #[tracing::instrument(skip_all)]
        pub fn $name<H, I, O, T, B, S>(handler: H) -> ApiMethodRouter<S, B, Infallible>
        where
            H: Handler<T, S, B> + OperationHandler<I, O>,
            I: OperationInput,
            O: OperationOutput,
            B: HttpBody + Send + Sync + 'static,
            S: Clone + Send + Sync + 'static,
            T: 'static,
        {
            let mut router = ApiMethodRouter::from(routing::$name(handler));
            let mut operation = Operation::default();
            in_context(|ctx| {
                I::operation_input(ctx, &mut operation);

                for (code, res) in O::inferred_responses(ctx, &mut operation) {
                    set_inferred_response(ctx, &mut operation, code, res);
                }

                // On conflict, input early responses potentially overwrite
                // output inferred responses on purpose, as they
                // are stronger in a sense that the request won't
                // even reach the handler body.
                for (code, res) in I::inferred_early_responses(ctx, &mut operation) {
                    set_inferred_response(ctx, &mut operation, code, res);
                }
            });

            router.operations.insert(stringify!($name), operation);

            router
        }

        #[doc = concat!("Route `", stringify!($name) ,"` requests to the given handler. See [`axum::routing::", stringify!($name) , "`] for more details.")]
        ///
        /// This method additionally accepts a transform function,
        /// see [`crate::axum`] for more details.
        #[tracing::instrument(skip_all)]
        pub fn $name_with<H, I, O, T, B, S, F>(
            handler: H,
            transform: F,
        ) -> ApiMethodRouter<S, B, Infallible>
        where
            H: Handler<T, S, B> + OperationHandler<I, O>,
            I: OperationInput,
            O: OperationOutput,
            B: axum::body::HttpBody + Send + Sync + 'static,
            S: Clone + Send + Sync + 'static,
            T: 'static,
            F: FnOnce(TransformOperation) -> TransformOperation,
        {
            let mut router = ApiMethodRouter::from(routing::$name(handler));
            let mut operation = Operation::default();
            in_context(|ctx| {
                I::operation_input(ctx, &mut operation);

                if ctx.infer_responses {
                    for (code, res) in O::inferred_responses(ctx, &mut operation) {
                        set_inferred_response(ctx, &mut operation, code, res);
                    }

                    // On conflict, input early responses potentially overwrite
                    // output inferred responses on purpose, as they
                    // are stronger in a sense that the request won't
                    // even reach the handler body.
                    for (code, res) in I::inferred_early_responses(ctx, &mut operation) {
                        set_inferred_response(ctx, &mut operation, code, res);
                    }
                }
            });

            let t = transform(TransformOperation::new(&mut operation));

            if !t.hidden {
                router.operations.insert(stringify!($name), operation);
            }

            router
        }
    };
}

fn set_inferred_response(
    ctx: &mut GenContext,
    operation: &mut Operation,
    status: Option<u16>,
    res: Response,
) {
    if operation.responses.is_none() {
        operation.responses = Some(Default::default());
    }

    let responses = operation.responses.as_mut().unwrap();

    match status {
        Some(status) => {
            if responses.responses.contains_key(&StatusCode::Code(status)) {
                ctx.error(Error::InferredResponseConflict(status));
            } else {
                responses
                    .responses
                    .insert(StatusCode::Code(status), ReferenceOr::Item(res));
            }
        }
        None => {
            if responses.default.is_some() {
                ctx.error(Error::InferredDefaultResponseConflict);
            } else {
                responses.default = Some(ReferenceOr::Item(res));
            }
        }
    }
}

impl<S, B> ApiMethodRouter<S, B, Infallible>
where
    S: Clone + Send + Sync + 'static,
    B: HttpBody + Send + Sync + 'static,
{
    method_router_chain_method!(delete, delete_with);
    method_router_chain_method!(get, get_with);
    method_router_chain_method!(head, head_with);
    method_router_chain_method!(options, options_with);
    method_router_chain_method!(patch, patch_with);
    method_router_chain_method!(post, post_with);
    method_router_chain_method!(put, put_with);
    method_router_chain_method!(trace, trace_with);

    /// This method wraps a layer around the [`ApiMethodRouter`]
    /// For further information see [`axum::routing::method_routing::MethodRouter::layer`]
    pub fn layer<L, NewReqBody, NewResBody, NewError>(
        self,
        layer: L,
    ) -> ApiMethodRouter<S, NewReqBody, NewError>
    where
        L: Layer<Route<B, Infallible>> + Clone + Send + 'static,
        L::Service: Service<
                Request<NewReqBody>,
                Response = http::response::Response<NewResBody>,
                Error = NewError,
            > + Clone
            + Send
            + 'static,
        <L::Service as Service<Request<NewReqBody>>>::Future: Send + 'static,
        NewResBody: 'static,
        NewReqBody: HttpBody + 'static,
        NewError: 'static,
        NewResBody: HttpBody<Data = Bytes> + Send + 'static,
        NewResBody::Error: Into<BoxError>,
    {
        ApiMethodRouter {
            router: self.router.layer(layer),
            operations: self.operations,
        }
    }

    /// This method wraps a layer around the [`ApiMethodRouter`]
    /// For further information see [`axum::routing::method_routing::MethodRouter::route_layer`]
    pub fn route_layer<L>(self, layer: L) -> Self
    where
        L: Layer<Route<B, Infallible>> + Clone + Send + 'static,
        L::Service: Service<Request<B>, Error = Infallible> + Clone + Send + 'static,
        <L::Service as Service<Request<B>>>::Response: IntoResponse + 'static,
        <L::Service as Service<Request<B>>>::Future: Send + 'static,
    {
        ApiMethodRouter {
            router: self.router.route_layer(layer),
            operations: self.operations,
        }
    }
}

impl<S, B, E> ApiMethodRouter<S, B, E>
where
    B: HttpBody + Send + 'static,
    S: Clone,
{
    /// Create a new, clean [`ApiMethodRouter`] based on [`MethodRouter::new()`](axum::routing::MethodRouter).
    pub fn new() -> Self {
        Self {
            operations: IndexMap::default(),
            router: MethodRouter::<S, B, E>::new(),
        }
    }
    /// See [`axum::routing::MethodRouter`] and [`axum::extract::State`] for more information.
    pub fn with_state<S2>(self, state: S) -> ApiMethodRouter<S2, B, E> {
        let router = self.router.with_state(state);
        ApiMethodRouter::<S2, B, E> {
            operations: self.operations,
            router,
        }
    }

    /// See [`axum::routing::MethodRouter::merge`] for more information.
    pub fn merge<M>(mut self, other: M) -> Self
    where
        M: Into<ApiMethodRouter<S, B, E>>,
    {
        let other = other.into();
        self.operations.extend(other.operations);
        self.router = self.router.merge(other.router);
        self
    }
}

impl<S, B, E> Default for ApiMethodRouter<S, B, E>
where
    B: HttpBody + Send + 'static,
    S: Clone,
{
    fn default() -> Self {
        Self::new()
    }
}

method_router_top_level!(delete, delete_with);
method_router_top_level!(get, get_with);
method_router_top_level!(head, head_with);
method_router_top_level!(options, options_with);
method_router_top_level!(patch, patch_with);
method_router_top_level!(post, post_with);
method_router_top_level!(put, put_with);
method_router_top_level!(trace, trace_with);

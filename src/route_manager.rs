pub use crate::route_handler::RouteHandler;

pub use crate::route_manager::route_mngr::RouteManagerInner;
pub use route_config::{BodyStorage, DataManagement, RouteConfig};
pub use route_mngr::{
    ErrorType, H3Method, RequestType, RouteForm, RouteFormBuilder, RouteManager,
    RouteManagerBuilder,
};
mod inflight_streams_path_verifier;

mod route_config {
    use std::default;

    #[derive(Clone, Copy, Debug)]
    pub enum DataManagement {
        Stream,
        Storage(BodyStorage),
    }
    impl DataManagement {
        pub fn is_body_storage(&self) -> Option<&BodyStorage> {
            if let Self::Storage(storage) = self {
                Some(storage)
            } else {
                None
            }
        }
    }

    impl Default for DataManagement {
        fn default() -> Self {
            Self::Storage(BodyStorage::default())
        }
    }

    #[derive(Clone, Default, Copy, Debug)]
    pub enum BodyStorage {
        #[default]
        InMemory,
        File,
    }
    #[derive(Debug)]
    pub struct RouteConfig {
        data_management: DataManagement,
    }

    impl Default for RouteConfig {
        fn default() -> Self {
            Self {
                data_management: Default::default(),
            }
        }
    }
    impl RouteConfig {
        pub fn new(data_management: DataManagement) -> RouteConfig {
            Self { data_management }
        }
        pub fn data_management(&self) -> DataManagement {
            self.data_management
        }
    }
}

mod route_mngr {
    use std::{
        any::Any,
        borrow::BorrowMut,
        collections::{hash_map, HashMap, HashSet},
        fmt::Debug,
        hash::Hash,
        process::Output,
        sync::{Arc, Mutex},
    };

    use quiche::h3::{self, Header};

    use crate::{
        event_listener,
        handler_dispatcher::{RouteEventDispatcher, RouteHandle},
        middleware::MiddleWare,
        request_response::{BodyType, RequestResponse},
        route_events::RouteEvent,
        route_handler::{self, ReqArgs, RequestsTable},
        route_manager::inflight_streams_path_verifier::InFlightStreamsPathVerifier,
        server_config,
        stream_sessions::{
            self, StreamBuilder, StreamHandle, StreamHandleCallback, StreamSessions, StreamType,
            UserSessions,
        },
        ErrorResponse, FinishedEvent, HeadersColl, MiddleWareFlow, MiddleWareResult, Response,
        RouteEventListener, RouteResponse, ServerConfig,
    };

    use self::route_config::DataManagement;

    use super::*;
    use crate::StreamCreation;

    type ReqPath = &'static str;

    pub struct RouteManager<S: Send + Sync + 'static, T: UserSessions<Output = T>> {
        inner: Arc<RouteManagerInner<S, T>>,
    }
    impl<S: Send + Sync + 'static, T: UserSessions<Output = T>> Clone for RouteManager<S, T> {
        fn clone(&self) -> Self {
            Self {
                inner: self.inner.clone(),
            }
        }
    }
    impl<S: Send + Sync + 'static + Clone, T: UserSessions<Output = T>> RouteManager<S, T> {
        ///
        ///___________________________
        ///Create a new Router with a concrete S as generic for an app state.
        ///Use S for MiddleWare trait implementation.
        ///
        pub fn new_with_app_state(app_state: S) -> RouteManagerBuilder<S, T> {
            RouteManagerBuilder {
                routes_formats: HashMap::new(),
                stream_sessions: Some(StreamSessions::<T>::new()),
                error_formats: HashMap::new(),
                app_state: Some(app_state),
                global_middlewares: vec![],
            }
        }

        pub fn get_routes_from_path(
            &self,
            path: &str,
            cb: impl FnOnce(Option<&Vec<Arc<RouteForm<S>>>>),
        ) {
            let guard = &*self.inner;

            cb(guard.get_routes_from_path(path));
        }

        pub fn in_flight_streams_path_verification(&self) -> &InFlightStreamsPathVerifier {
            &self.inner.in_flight_streams_path_verification
        }
        pub fn stream_sessions(&self) -> Option<StreamSessions<T>> {
            let guard = &*self.inner;

            if let Some(stream_sessions) = guard.stream_sessions() {
                Some(stream_sessions.clone())
            } else {
                None
            }
        }
        pub fn get_routes_from_path_and_method_and_request_type(
            &self,
            path: &str,
            methode: H3Method,
            request_type: RequestType,
            cb: impl FnOnce(Option<&RouteForm<S>>),
        ) {
            let guard = &*self.inner;

            cb(guard.get_routes_from_path_and_method_b(path, methode));
        }
        pub fn routes_handler(&self) -> RouteHandler<S, T> {
            RouteHandler::new(self.inner.clone())
        }
        pub fn is_request_set_in_table(&self, stream_id: u64, conn_id: &str) -> bool {
            let guard = &self.inner;

            guard
                .routes_states()
                .is_entry_partial_reponse_set(stream_id, conn_id)
        }
        pub fn set_request_in_table(
            &self,
            stream_id: u64,
            conn_id: &str,
            scid: &[u8],
            server_config: &Arc<ServerConfig>,
            file_writer_channel: &crate::file_writer::FileWriterChannel,
        ) {
            let route_handler = self.routes_handler();
            let mut data_management: Option<DataManagement> = Some(DataManagement::default());
            let mut event_subscriber: Option<Arc<dyn RouteEventListener + Send + Sync + 'static>> =
                None;

            route_handler
                .routes_states()
                .add_partial_request_before_header_treatment(
                    server_config,
                    conn_id.to_string(),
                    stream_id,
                    data_management,
                    event_subscriber.clone(),
                    !true,
                    file_writer_channel.clone(),
                );
        }
    }

    pub struct RouteManagerInner<S: Send + Sync + 'static, T: UserSessions<Output = T>> {
        routes_formats: HashMap<ReqPath, Vec<Arc<RouteForm<S>>>>,
        in_flight_streams_path_verification: InFlightStreamsPathVerifier,
        stream_sessions: StreamSessions<T>,
        error_formats: HashMap<ErrorType, ErrorForm>,
        app_state: S,
        route_event_dispatcher: RouteEventDispatcher<S>,
        route_states: RequestsTable, //trace_id of the Connection as
                                     //key value is HashMap
                                     //for stream_id u64
    }
    impl<S: Send + Sync + 'static + Clone, T: UserSessions<Output = T>> RouteManagerInner<S, T> {
        ///
        ///Init the request manager builder.
        ///You can add new request forms with add_new_request_form();
        ///
        ///
        ///
        ///
        /*
        pub fn new() -> RouteManagerBuilder<S, T> {
            RouteManagerBuilder {
                routes_formats: HashMap::new(),
                stream_sessions: StreamSessions::<S, T>::new(),
                error_formats: HashMap::new(),
                app_state: None,
                global_middlewares: vec![],
            }
        }*/
        pub fn app_state(&self) -> &S {
            &self.app_state
        }
        pub fn in_flight_streams_path_verification(&self) -> &InFlightStreamsPathVerifier {
            &self.in_flight_streams_path_verification
        }
        pub fn is_request_set_in_table(&self, stream_id: u64, conn_id: &str) -> bool {
            self.routes_states()
                .is_entry_partial_reponse_set(stream_id, conn_id)
        }
        pub fn stream_sessions(&self) -> Option<&StreamSessions<T>> {
            Some(&self.stream_sessions)
        }

        pub fn routes_states(&self) -> &RequestsTable {
            &self.route_states
        }
        pub fn route_event_dispatcher(&self) -> &RouteEventDispatcher<S> {
            &self.route_event_dispatcher
        }
        pub fn get_error_response(
            &self,
            error: ErrorType,
        ) -> Option<(Vec<h3::Header>, Option<BodyType>)> {
            if let Some(entry) = self.error_formats.get(&error) {
                return Some((entry.headers(), entry.body()));
            }
            None
        }
        pub fn routes_formats(&self) -> &HashMap<ReqPath, Vec<Arc<RouteForm<S>>>> {
            &self.routes_formats
        }

        pub fn get_routes_from_path(&self, path: &str) -> Option<&Vec<Arc<RouteForm<S>>>> {
            self.routes_formats.get(path)
        }
        pub fn get_routes_from_path_and_method_b(
            &self,
            path: &str,
            methode: H3Method,
        ) -> Option<&RouteForm<S>> {
            if let Some(request_coll) = self.get_routes_from_path(path) {
                if let Some(found_route) =
                    request_coll.iter().find(|item| item.method() == &methode)
                {
                    return Some(found_route);
                }
                None
            } else {
                None
            }
        }
        /// Search for the corresponding request format that contains the
        /// associated callback.
        ///
        /// # If no route found with path
        ///
        /// Then return a 400 error Status !
        pub fn get_routes_from_path_and_method<'b>(
            &self,
            path: &'b str,
            methode: H3Method,
        ) -> Option<(Arc<RouteForm<S>>, Option<ReqArgs>)> {
            //if param in path
            //

            let (path, req_args) = ReqArgs::parse_args(&path);

            if let Some(route_coll) = self.get_routes_from_path(path.as_str()) {
                if let Some(found_route) = route_coll.iter().find(|item| item.method() == &methode)
                {
                    return Some((found_route.clone(), req_args));
                }
                None
            } else {
                None
            }
        }
    }

    pub struct RouteManagerBuilder<S: Send + Sync + 'static, T: UserSessions<Output = T>> {
        routes_formats: HashMap<ReqPath, Vec<Arc<RouteForm<S>>>>,
        stream_sessions: Option<StreamSessions<T>>,
        error_formats: HashMap<ErrorType, ErrorForm>,
        app_state: Option<S>,
        global_middlewares: Vec<Arc<dyn MiddleWare + Send + Sync + 'static>>,
    }
    impl<S: Send + Sync + 'static + Clone, T: UserSessions<Output = T>> RouteManagerBuilder<S, T> {
        pub fn with_stream_sessions(mut self, stream_sessions: &StreamSessions<T>) -> Self {
            self.stream_sessions = Some(stream_sessions.clone());
            self
        }
        pub fn build(&mut self) -> RouteManager<S, T> {
            let handle_dispatcher = self.build_route_event_dispatcher();

            let routes_formats = std::mem::replace(&mut self.routes_formats, HashMap::new());
            let path_set: HashSet<&'static str> = routes_formats
                .keys()
                .map(|it| *it)
                .collect::<HashSet<&'static str>>();
            let in_flight_streams_path_verification = InFlightStreamsPathVerifier::new(path_set);
            let request_manager_inner = RouteManagerInner {
                stream_sessions: self.stream_sessions.take().unwrap(),
                in_flight_streams_path_verification,
                routes_formats,
                error_formats: std::mem::replace(&mut self.error_formats, HashMap::new()),
                app_state: self.app_state.take().unwrap(),
                route_states: RequestsTable::new(),
                route_event_dispatcher: handle_dispatcher,
            };

            RouteManager {
                inner: Arc::new(request_manager_inner),
            }
        }
        pub fn app_state(&self) -> Option<S> {
            self.app_state.clone()
        }
        pub fn to_stream_handler<
            F: Fn(FinishedEvent, &StreamSessions<T>, &S) -> Result<(), ()> + Sync + Send + 'static,
        >(
            &self,
            cb: &'static F,
        ) -> Arc<dyn StreamHandle<T> + Send + Sync + 'static> {
            #[derive(Clone)]
            pub struct Anon<S: 'static, T: UserSessions<Output = T>>(
                Arc<
                    &'static (dyn Fn(FinishedEvent, &StreamSessions<T>, &S) -> Result<(), ()>
                                  + Sync
                                  + Send
                                  + 'static),
                >,
            );

            impl<S: Send + Sync + 'static + Any, T: UserSessions<Output = T>>
                StreamHandleCallback<T> for Anon<S, T>
            {
                type State = S;

                fn callback(
                    &self,
                    event: FinishedEvent,
                    user_session: &StreamSessions<T>,
                    app_state: &Self::State,
                ) -> Result<(), ()> {
                    (*self.0)(event, user_session, app_state)
                }
            }
            /*
            impl<S: 'static + Send + Sync, T: UserSessions<Output = T>> StreamHandle<T> for Anon<S, T> {
                type State = S;
                fn call(
                    &self,
                    event: FinishedEvent,
                    user_session: &mut T,
                    app_state: &Self::State,
                ) -> Result<(), ()> {
                    (self.0)(event, user_session, app_state)
                }
            }

            */
            let anon: Arc<dyn StreamHandle<T> + Send + Sync + 'static> =
                Arc::new(Anon(Arc::new(cb))) as Arc<dyn StreamHandle<T> + Send + Sync + 'static>;
            anon
        }
        pub fn handler<
            F: Fn(FinishedEvent, &S, RouteResponse) -> Response + Sync + Send + 'static,
        >(
            &self,
            cb: &'static F,
        ) -> Arc<dyn RouteHandle<S> + Send + Sync> {
            #[derive(Clone)]
            pub struct Anon<S: 'static>(
                Arc<
                    &'static (dyn Fn(FinishedEvent, &S, RouteResponse) -> Response
                                  + Sync
                                  + Send
                                  + 'static),
                >,
            );

            impl<S: 'static + Send + Sync> RouteHandle<S> for Anon<S> {
                fn call(
                    &self,
                    event: FinishedEvent,
                    state: &S,
                    current_status_response: RouteResponse,
                ) -> Response {
                    (*self.0)(event, state, current_status_response)
                }
            }

            let anon: Arc<dyn RouteHandle<S> + Send + Sync + 'static> =
                Arc::new(Anon(Arc::new(cb))) as Arc<dyn RouteHandle<S> + Send + Sync + 'static>;
            anon
        }
        pub fn middleware<F: Fn(Vec<h3::Header>, &S) -> MiddleWareFlow + Send + Sync + 'static>(
            &self,
            cb: &'static F,
        ) -> Arc<dyn MiddleWare + Send + Sync + 'static> {
            struct Anon<S: 'static>(
                Arc<
                    &'static (dyn Fn(Vec<h3::Header>, &S) -> MiddleWareFlow
                                  + Send
                                  + Sync
                                  + 'static),
                >,
            );

            use crate::middleware::MiddleWareErased;

            impl<S: Send + Sync + 'static + Any> MiddleWareErased for Anon<S> {
                type State = S;

                fn call_inner(
                    &self,
                    headers: Vec<h3::Header>,
                    state: &Self::State,
                ) -> MiddleWareFlow {
                    (*self.0)(headers, state)
                }
            }

            let anon: Arc<dyn MiddleWare + Send + Sync + 'static> = Arc::new(Anon(Arc::new(cb)));
            anon
        }
        pub fn build_route_event_dispatcher(&self) -> RouteEventDispatcher<S> {
            let mut handler_dispatcher = RouteEventDispatcher::new();
            handler_dispatcher.set_handles(&self.routes_formats);
            handler_dispatcher
        }
        pub fn attach_event_loop(
            &mut self,
            event_loop: Arc<dyn RouteEventListener + Send + Sync + 'static>,
        ) {
            /*
            for route_coll in self.routes_formats.values_mut() {
                for route in route_coll {
                    route.set_event_subscriber(&event_loop);
                }
            }*/
        }
        ///___________________________
        ///Add a middleware that will affect all routes.
        ///It will be process before the route specifics middlewares.
        pub fn global_middleware(
            &mut self,
            entry: Arc<dyn MiddleWare + Send + Sync + 'static>,
        ) -> &mut Self {
            self.global_middlewares.push(entry);
            self
        }
        pub fn set_error_handler(
            &mut self,
            error_type: ErrorType,
            cb: impl FnOnce(&mut ErrorFormBuilder),
        ) {
            let mut error_form_builder = ErrorForm::new(error_type);
            cb(&mut error_form_builder);
            let error_form: ErrorForm = error_form_builder.build();
            self.error_formats
                .entry(error_type)
                .insert_entry(error_form);
        }
        pub fn route_post(
            &mut self,
            path: &'static str,
            route_configuration: RouteConfig,
            route: impl FnOnce(&mut RouteFormBuilder<S>),
        ) -> &mut Self {
            self.add_route(path, H3Method::POST, route_configuration, route);
            self
        }
        pub fn down_stream(
            &mut self,
            path: &'static str,
            route_configuration: (),
            stream_builder_cb: impl FnOnce(&mut StreamBuilder<T>),
        ) -> &mut Self {
            if let Some(stream_session) = &mut self.stream_sessions {
                stream_session.create_stream(
                    path,
                    StreamType::Down,
                    route_configuration,
                    stream_builder_cb,
                );
            }
            self
        }
        pub fn route_get(
            &mut self,
            path: &'static str,
            route_configuration: RouteConfig,
            route: impl FnOnce(&mut RouteFormBuilder<S>),
        ) -> &mut Self {
            self.add_route(path, H3Method::GET, route_configuration, route);
            self
        }
        pub fn route_delete(
            &mut self,
            path: &'static str,
            route_configuration: RouteConfig,
            route: impl FnOnce(&mut RouteFormBuilder<S>),
        ) -> &mut Self {
            self.add_route(path, H3Method::DELETE, route_configuration, route);
            self
        }
        fn add_route(
            &mut self,
            path: &'static str,
            method: H3Method,
            route_configuration: RouteConfig,
            route: impl FnOnce(&mut RouteFormBuilder<S>),
        ) -> &mut Self {
            let mut route_form = RouteForm::new(path, method, route_configuration);

            route(&mut route_form);

            route_form.add_global_middlewares(self.global_middlewares.clone());

            let route = route_form.build();
            self.add_new_route(route);
            self
        }
        ///
        ///Add a new RouteForm to the server.
        ///
        fn add_new_route(&mut self, route_form: RouteForm<S>) -> &mut Self {
            let path = route_form.path();

            let route_form_boxed = Arc::new(route_form);
            if !self.routes_formats.contains_key(path) {
                self.routes_formats.insert(path, vec![route_form_boxed]);
            } else {
                assert!(!self
                    .routes_formats
                    .get(path)
                    .as_ref()
                    .unwrap()
                    .contains(&route_form_boxed));
                self.routes_formats
                    .get_mut(path)
                    .expect(&format!("can't access value for {:?}", path))
                    .push(route_form_boxed);
            }
            self
        }
    }

    #[derive(Clone, Debug, Copy, PartialEq, Hash, Eq)]
    pub enum H3Method {
        GET,
        POST,
        PUT,
        DELETE,
    }

    impl H3Method {
        ///
        ///Parse method name from raw bytes.
        ///
        ///
        pub fn parse(input: &[u8]) -> Result<H3Method, ()> {
            match &String::from_utf8_lossy(input)[..] {
                "GET" => Ok(H3Method::GET),
                "POST" => Ok(H3Method::POST),
                "PUT" => Ok(H3Method::PUT),
                "DELETE" => Ok(H3Method::DELETE),
                &_ => Err(()),
            }
        }
        pub fn get_headers_for_middleware<'a>(
            &self,
            headers: &'a mut [h3::Header],
        ) -> HeadersColl<'a> {
            match self {
                Self::POST => HeadersColl::HeadersPost(headers),
                Self::GET => HeadersColl::HeadersGet(headers),
                Self::PUT => HeadersColl::HeadersPost(headers),
                Self::DELETE => HeadersColl::HeadersPost(headers),
            }
        }
    }

    #[derive(PartialEq)]
    pub enum RequestType {
        Ping,
        Message(String),
        File(String),
    }

    #[derive(Eq, Hash, Clone, Copy, Debug, PartialEq)]
    pub enum ErrorType {
        Error404,
    }
    impl ErrorType {
        pub fn to_header(&self) -> h3::Header {
            match self {
                Self::Error404 => h3::Header::new(b":status", b"404"),
            }
        }
    }
    pub struct ErrorForm {
        error_type: ErrorType,
        scheme: &'static str,
        authority: Option<&'static str>,
        headers: Vec<h3::Header>,
        body: Option<BodyType>,
    }
    impl Debug for ErrorForm {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(
                f,
                "error  [{:?}] path : [{:?}] , route_configuration [{:?}]",
                self.error_type,
                self.headers,
                self.body.is_some()
            )
        }
    }

    impl PartialEq for ErrorForm {
        fn eq(&self, other: &Self) -> bool {
            self.error_type() == other.error_type()
        }
    }
    impl ErrorForm {
        pub fn error_type(&self) -> ErrorType {
            self.error_type
        }
        pub fn new(error_type: ErrorType) -> ErrorFormBuilder {
            let mut builder = ErrorFormBuilder::new();
            builder.set_error_type(error_type);
            builder.set_scheme("https");
            builder.response_header = Some(vec![error_type.to_header()]);
            builder
        }
        pub fn headers(&self) -> Vec<h3::Header> {
            self.headers.clone()
        }
        pub fn body(&self) -> Option<BodyType> {
            self.body.clone()
        }
    }

    pub struct RouteForm<S> {
        method: H3Method,
        event_subscriber: Option<Arc<dyn RouteEventListener + 'static + Send + Sync>>,
        handler_subscriber: Vec<Arc<dyn RouteHandle<S> + Send + Sync + 'static>>,
        middlewares: Vec<Arc<dyn MiddleWare + Send + Sync + 'static>>,
        path: &'static str,
        route_configuration: Option<RouteConfig>,
        scheme: &'static str,
        authority: Option<&'static str>,
        body_cb:
            Option<Box<dyn Fn(RouteEvent) -> Result<RequestResponse, ()> + Send + Sync + 'static>>,
    }
    impl<S: Send + Sync + 'static + Clone> Debug for RouteForm<S> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(
                f,
                "method [{:?}] path : [{:?}] , route_configuration [{:?}]",
                self.method, self.path, self.route_configuration
            )
        }
    }

    impl<S: Send + Sync + 'static + Clone> PartialEq for RouteForm<S> {
        fn eq(&self, other: &Self) -> bool {
            self.method() == other.method()
                && self.path() == self.path()
                && self.scheme == self.scheme
                && self.authority == other.authority
        }
    }

    impl<S: Send + Sync + 'static + Clone> RouteForm<S> {
        pub fn new(
            path: &'static str,
            method: H3Method,
            route_config: RouteConfig,
        ) -> RouteFormBuilder<S> {
            let mut builder = RouteFormBuilder::new();
            builder.set_path(path);
            builder.set_method(method);
            builder.set_route_config(route_config);
            builder.set_scheme("https");
            builder
        }
        pub fn path(&self) -> &'static str {
            self.path
        }
        pub fn to_middleware_coll(&self) -> Vec<Arc<dyn MiddleWare + Send + Sync>> {
            let mut vec: Vec<Arc<dyn MiddleWare + Send + Sync>> = vec![];

            let cloned_coll = self.middlewares.clone();

            for mdw in cloned_coll.into_iter() {
                vec.push(mdw)
            }
            //vec
            self.middlewares.clone()
        }
        pub fn method(&self) -> &H3Method {
            &self.method
        }
        pub fn handles(&self) -> Vec<Arc<dyn RouteHandle<S> + Sync + Send + 'static>> {
            self.handler_subscriber.clone()
        }
        pub fn set_event_subscriber(
            &mut self,
            event_listener: &Arc<dyn RouteEventListener + Send + Sync + 'static>,
        ) {
            self.event_subscriber = Some(event_listener.clone());
        }
        pub fn event_subscriber(
            &self,
        ) -> Option<Arc<dyn RouteEventListener + 'static + Send + Sync>> {
            self.event_subscriber.clone()
        }

        pub fn data_management_type(&self) -> Option<DataManagement> {
            if let Some(config) = &self.route_configuration {
                Some(config.data_management())
            } else {
                None
            }
        }
        pub fn build_response(
            &self,
            stream_id: u64,
            scid: &[u8],
            conn_id: &str,
            response: Option<RequestResponse>,
        ) -> Result<(Vec<h3::Header>, Option<BodyType>), ()> {
            if let Some(mut request_response) = response {
                let headers = request_response.get_headers();
                let body = request_response.take_body();
                if let BodyType::None = body {
                    return Ok((headers, None));
                };
                return Ok((headers, Some(body)));
            }
            let default = RequestResponse::new_ok_200(stream_id, scid, conn_id);
            let headers = default.get_headers();
            Ok((headers, None))
        }
    }

    pub struct ErrorFormBuilder {
        error_type: Option<ErrorType>,
        scheme: Option<&'static str>,
        authority: Option<&'static str>,
        response_header: Option<Vec<h3::Header>>,
        body: Option<BodyType>,
    }
    impl ErrorFormBuilder {
        pub fn build(&mut self) -> ErrorForm {
            ErrorForm {
                error_type: self.error_type.take().unwrap(),
                scheme: self.scheme.take().unwrap(),
                authority: self.authority.take(),
                headers: self.response_header.take().unwrap(),
                body: self.body.take(),
            }
        }
        pub fn new() -> Self {
            Self {
                error_type: None,
                scheme: None,
                authority: None,
                response_header: None,
                body: None,
            }
        }
        pub fn set_error_type(&mut self, error_type: ErrorType) -> &mut Self {
            self.error_type = Some(error_type);
            self
        }
        pub fn set_scheme(&mut self, scheme: &'static str) -> &mut Self {
            self.scheme = Some(scheme);
            self
        }
        pub fn header(&mut self, headers: &[h3::Header]) -> &mut Self {
            self.response_header
                .as_mut()
                .unwrap()
                .extend_from_slice(headers);
            self
        }
    }

    pub struct RouteFormBuilder<S> {
        method: Option<H3Method>,
        event_subscriber: Option<Arc<dyn RouteEventListener + 'static + Send + Sync>>,
        handler_subscriber: Vec<Arc<dyn RouteHandle<S> + Send + Sync + 'static>>,
        middlewares: Vec<Arc<dyn MiddleWare + Sync + Send + 'static>>,
        route_configuration: Option<RouteConfig>,
        path: Option<&'static str>,
        scheme: Option<&'static str>,
        authority: Option<&'static str>,
        body_cb: Option<
            Box<
                dyn Fn(RouteEvent) -> Result<(RequestResponse), ()>
                    /*body, content-type*/
                    + Sync
                    + Send
                    + 'static,
            >,
        >,
    }

    impl<S: Send + Sync + 'static + Clone> RouteFormBuilder<S> {
        pub fn new() -> Self {
            Self {
                method: None,
                event_subscriber: None,
                handler_subscriber: vec![],
                path: None,
                middlewares: vec![],
                route_configuration: None,
                scheme: None,
                authority: None,
                body_cb: None,
            }
        }
        ///____________________________
        ///# Registration of a new handle for this route
        ///
        ///RouteHandle trait implementation is required, as well as an Arc encapsulation.
        pub fn handler(
            &mut self,
            handler: &Arc<dyn RouteHandle<S> + Send + Sync + 'static>,
        ) -> &mut Self {
            self.handler_subscriber.push(handler.clone());
            self
        }

        pub fn subscribe_event(
            &mut self,
            event_listener: Arc<dyn RouteEventListener + 'static + Send + Sync>,
        ) -> &mut Self {
            self.event_subscriber = Some(event_listener);
            self
        }
        ///___________________________
        ///
        ///
        ///# Add a Middleware in the collection.
        ///
        ///The chaining order here reflects the order of the middleware collection that will be processed by
        ///the server.
        ///
        ///# Trait implementation
        ///
        ///Your middleware has to implement MiddleWare<S> trait. Then to be accepted by this method, it
        ///has to be
        ///wrapped it in an Arc.
        ///
        ///# Where is it called and what does it do
        ///
        ///A middleware is called on a new header reception, extracts it and procedes to compute some logic with it as argument.
        ///(for auth verification,or any other condition checking).
        ///It returns an enum that tells if the request is accepted here and can continue or if the
        ///request does'nt meet the condition and has to abort.
        ///
        ///# In case of abortion
        ///
        ///The abort variant encapsulates the error response that will be returned to the peer.
        ///
        pub fn middleware(
            &mut self,
            middleware: &Arc<dyn MiddleWare + Send + Sync + 'static>,
        ) -> &mut Self {
            self.middlewares.push(middleware.clone());
            self
        }
        fn add_global_middlewares(
            &mut self,
            entries: Vec<Arc<dyn MiddleWare + Send + Sync + 'static>>,
        ) -> &mut Self {
            let specific_mdw = std::mem::replace(&mut self.middlewares, entries);
            self.middlewares.extend(specific_mdw);
            self
        }
        pub fn build(&mut self) -> RouteForm<S> {
            RouteForm {
                method: self.method.take().unwrap(),
                event_subscriber: self.event_subscriber.take(),
                handler_subscriber: std::mem::replace(&mut self.handler_subscriber, vec![]),
                middlewares: self.middlewares.clone(),
                path: self.path.take().unwrap(),
                route_configuration: self.route_configuration.take(),
                scheme: self.scheme.take().expect("expected scheme"),
                authority: self.authority.clone(),
                body_cb: self.body_cb.take(),
            }
        }

        ///
        /// Set the callback for the response. It exposes in parameters 0 = the path of the request,
        /// parameters 1 = the list of args if any "/path?id=foo&name=bar&other=etc"
        ///
        ///The callback returns (body, value of content-type field as bytes)
        ///
        pub fn on_finished_callback(
            &mut self,
            body_cb: impl Fn(RouteEvent) -> Result<(RequestResponse), ()> + Sync + Send + 'static,
        ) -> &mut Self {
            self.body_cb = Some(Box::new(body_cb));
            self
        }
        pub fn set_route_config(&mut self, route_config: RouteConfig) -> &mut Self {
            self.route_configuration = Some(route_config);
            self
        }
        ///
        ///Set the http method. H3Method::GET, ::POST, ::PUT, ::DELETE
        ///
        ///
        fn set_method(&mut self, method: H3Method) -> &mut Self {
            self.method = Some(method);
            self
        }
        ///
        /// Set the request path as "/home"
        ///
        ///
        fn set_path(&mut self, path: &'static str) -> &mut Self {
            self.path = Some(path);
            self
        }
        ///
        /// Set the connexion type : https here
        ///
        fn set_scheme(&mut self, scheme: &'static str) -> &mut Self {
            self.scheme = Some(scheme);
            self
        }
    }
}

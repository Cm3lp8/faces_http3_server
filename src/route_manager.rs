pub use crate::route_handler::RouteHandler;
pub use crate::route_manager::route_mngr::RouteManagerInner;
pub use route_config::{BodyStorage, DataManagement, RouteConfig};
pub use route_mngr::{
    H3Method, RequestType, RouteForm, RouteFormBuilder, RouteManager, RouteManagerBuilder,
};

mod route_config {

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

    #[derive(Clone, Copy, Debug)]
    pub enum BodyStorage {
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
                data_management: DataManagement::Storage(BodyStorage::InMemory),
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
        collections::HashMap,
        fmt::Debug,
        hash::Hash,
        sync::{Arc, Mutex},
    };

    use quiche::h3;

    use crate::{
        event_listener,
        request_response::{BodyType, RequestResponse},
        route_events::RouteEvent,
        route_handler::RequestsTable,
        RouteEventListener,
    };

    use self::route_config::DataManagement;

    use super::*;

    type ReqPath = &'static str;

    pub struct RouteManager {
        inner: Arc<Mutex<RouteManagerInner>>,
    }
    impl Clone for RouteManager {
        fn clone(&self) -> Self {
            Self {
                inner: self.inner.clone(),
            }
        }
    }
    impl RouteManager {
        pub fn new() -> RouteManagerBuilder {
            RouteManagerBuilder {
                routes_formats: HashMap::new(),
            }
        }
        pub fn get_routes_from_path(&self, path: &str, cb: impl FnOnce(Option<&Vec<RouteForm>>)) {
            let guard = &*self.inner.lock().unwrap();

            cb(guard.get_routes_from_path(path));
        }
        pub fn get_routes_from_path_and_method_and_request_type(
            &self,
            path: &str,
            methode: H3Method,
            request_type: RequestType,
            cb: impl FnOnce(Option<&RouteForm>),
        ) {
            let guard = &*self.inner.lock().unwrap();

            cb(guard.get_routes_from_path_and_method_b(path, methode));
        }
        pub fn routes_handler(&self) -> RouteHandler {
            RouteHandler::new(self.inner.clone())
        }
    }

    pub struct RouteManagerInner {
        routes_formats: HashMap<ReqPath, Vec<RouteForm>>,
        route_states: RequestsTable, //trace_id of the Connection as
                                     //key value is HashMap
                                     //for stream_id u64
    }
    impl RouteManagerInner {
        ///
        ///Init the request manager builder.
        ///You can add new request forms with add_new_request_form();
        ///
        ///
        ///
        ///
        pub fn new() -> RouteManagerBuilder {
            RouteManagerBuilder {
                routes_formats: HashMap::new(),
            }
        }
        pub fn routes_states(&self) -> &RequestsTable {
            &self.route_states
        }
        pub fn routes_formats(&self) -> &HashMap<ReqPath, Vec<RouteForm>> {
            &self.routes_formats
        }

        pub fn get_routes_from_path(&self, path: &str) -> Option<&Vec<RouteForm>> {
            self.routes_formats.get(path)
        }
        pub fn get_routes_from_path_and_method_b(
            &self,
            path: &str,
            methode: H3Method,
        ) -> Option<&RouteForm> {
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
        pub fn get_routes_from_path_and_method<'b>(
            &self,
            path: &'b str,
            methode: H3Method,
        ) -> Option<(&RouteForm, Option<Vec<&'b str>>)> {
            //if param in path

            let mut path_s = path.to_string();
            let mut param_trail: Option<Vec<&str>> = None;

            if let Some((path, id)) = path.split_once("?") {
                path_s = path.to_string();

                let args_it: Vec<&str> = id.split("&").collect();
                param_trail = Some(args_it);
            }

            if let Some(route_coll) = self.get_routes_from_path(path_s.as_str()) {
                if let Some(found_route) = route_coll.iter().find(|item| item.method() == &methode)
                {
                    return Some((found_route, param_trail));
                }
                None
            } else {
                None
            }
        }
    }

    pub struct RouteManagerBuilder {
        routes_formats: HashMap<ReqPath, Vec<RouteForm>>,
    }
    impl RouteManagerBuilder {
        pub fn build(&mut self) -> RouteManager {
            let request_manager_inner = RouteManagerInner {
                routes_formats: std::mem::replace(&mut self.routes_formats, HashMap::new()),
                route_states: RequestsTable::new(),
            };

            RouteManager {
                inner: Arc::new(Mutex::new(request_manager_inner)),
            }
        }
        pub fn route_post(
            &mut self,
            path: &'static str,
            route_configuration: RouteConfig,
            route: impl FnOnce(&mut RouteFormBuilder),
        ) -> &mut Self {
            self.add_route(path, H3Method::POST, route_configuration, route);
            self
        }
        pub fn route_get(
            &mut self,
            path: &'static str,
            route_configuration: RouteConfig,
            route: impl FnOnce(&mut RouteFormBuilder),
        ) -> &mut Self {
            self.add_route(path, H3Method::GET, route_configuration, route);
            self
        }
        fn add_route(
            &mut self,
            path: &'static str,
            method: H3Method,
            route_configuration: RouteConfig,
            route: impl FnOnce(&mut RouteFormBuilder),
        ) -> &mut Self {
            let mut route_form = RouteForm::new(path, method, route_configuration);

            route(&mut route_form);

            let route = route_form.build();
            self.add_new_route(route);
            self
        }
        ///
        ///Add a new RouteForm to the server.
        ///
        fn add_new_route(&mut self, route_form: RouteForm) -> &mut Self {
            let path = route_form.path();

            if !self.routes_formats.contains_key(path) {
                self.routes_formats.insert(path, vec![route_form]);
            } else {
                assert!(!self
                    .routes_formats
                    .get(path)
                    .as_ref()
                    .unwrap()
                    .contains(&route_form));
                self.routes_formats
                    .get_mut(path)
                    .expect(&format!("can't access value for {:?}", path))
                    .push(route_form);
            }
            self
        }
    }

    #[derive(Clone, Debug, Copy, PartialEq)]
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
    }

    #[derive(PartialEq)]
    pub enum RequestType {
        Ping,
        Message(String),
        File(String),
    }

    pub struct RouteForm {
        method: H3Method,
        event_subscriber: Option<Arc<dyn RouteEventListener + 'static + Send + Sync>>,
        path: &'static str,
        route_configuration: Option<RouteConfig>,
        scheme: &'static str,
        authority: Option<&'static str>,
        body_cb:
            Option<Box<dyn Fn(RouteEvent) -> Result<RequestResponse, ()> + Send + Sync + 'static>>,
    }
    impl Debug for RouteForm {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(
                f,
                "method [{:?}] path : [{:?}] , route_configuration [{:?}]",
                self.method, self.path, self.route_configuration
            )
        }
    }

    impl PartialEq for RouteForm {
        fn eq(&self, other: &Self) -> bool {
            self.method() == other.method()
                && self.path() == self.path()
                && self.scheme == self.scheme
                && self.authority == other.authority
        }
    }

    impl RouteForm {
        pub fn new(
            path: &'static str,
            method: H3Method,
            route_config: RouteConfig,
        ) -> RouteFormBuilder {
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
        pub fn method(&self) -> &H3Method {
            &self.method
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

    pub struct RouteFormBuilder {
        method: Option<H3Method>,
        event_subscriber: Option<Arc<dyn RouteEventListener + 'static + Send + Sync>>,
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

    impl RouteFormBuilder {
        pub fn new() -> Self {
            Self {
                method: None,
                event_subscriber: None,
                path: None,
                route_configuration: None,
                scheme: None,
                authority: None,
                body_cb: None,
            }
        }

        pub fn subscribe_event(
            &mut self,
            event_listener: Arc<dyn RouteEventListener + 'static + Send + Sync>,
        ) -> &mut Self {
            self.event_subscriber = Some(event_listener);
            self
        }
        pub fn build(&mut self) -> RouteForm {
            RouteForm {
                method: self.method.take().unwrap(),
                event_subscriber: self.event_subscriber.take(),
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

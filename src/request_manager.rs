use crate::request_handler::RequestHandler;
pub use crate::request_manager::request_mngr::RequestManagerInner;
pub use request_mngr::{
    H3Method, RequestForm, RequestFormBuilder, RequestManager, RequestManagerBuilder, RequestType,
};

mod request_mngr {
    use std::{
        collections::HashMap,
        hash::Hash,
        sync::{Arc, Mutex},
    };

    use quiche::h3;

    use crate::{
        request_events::RequestEvent, request_handler::RequestsTable,
        request_response::RequestResponse,
    };

    use super::*;

    type ReqPath = &'static str;

    pub struct RequestManager {
        inner: Arc<Mutex<RequestManagerInner>>,
    }
    impl RequestManager {
        pub fn new() -> RequestManagerBuilder {
            RequestManagerBuilder {
                requests_formats: HashMap::new(),
            }
        }
        pub fn get_requests_from_path(
            &self,
            path: &str,
            cb: impl FnOnce(Option<&Vec<RequestForm>>),
        ) {
            let guard = &*self.inner.lock().unwrap();

            cb(guard.get_requests_from_path(path));
        }
        pub fn get_requests_from_path_and_method_and_request_type(
            &self,
            path: &str,
            methode: H3Method,
            request_type: RequestType,
            cb: impl FnOnce(Option<&RequestForm>),
        ) {
            let guard = &*self.inner.lock().unwrap();

            cb(guard.get_requests_from_path_and_method_and_request_type(
                path,
                methode,
                request_type,
            ));
        }
        pub fn request_handler(&self) -> RequestHandler {
            RequestHandler::new(self.inner.clone())
        }
    }

    pub struct RequestManagerInner {
        requests_formats: HashMap<ReqPath, Vec<RequestForm>>,
        request_states: RequestsTable, //trace_id of the Connection as
                                       //key value is HashMap
                                       //for stream_id u64
    }
    impl RequestManagerInner {
        ///
        ///Init the request manager builder.
        ///You can add new request forms with add_new_request_form();
        ///
        ///
        ///
        ///
        pub fn new() -> RequestManagerBuilder {
            RequestManagerBuilder {
                requests_formats: HashMap::new(),
            }
        }
        pub fn request_states(&self) -> &RequestsTable {
            &self.request_states
        }
        pub fn request_formats(&self) -> &HashMap<ReqPath, Vec<RequestForm>> {
            &self.requests_formats
        }

        pub fn get_requests_from_path(&self, path: &str) -> Option<&Vec<RequestForm>> {
            self.requests_formats.get(path)
        }
        pub fn get_requests_from_path_and_method_and_request_type(
            &self,
            path: &str,
            methode: H3Method,
            request_type: RequestType,
        ) -> Option<&RequestForm> {
            if let Some(request_coll) = self.get_requests_from_path(path) {
                if let Some(found_request) = request_coll
                    .iter()
                    .find(|item| item.method() == &methode && item.request_type() == &request_type)
                {
                    return Some(found_request);
                }
                None
            } else {
                None
            }
        }
        /// Search for the corresponding request format to build the response and send it by triggering the
        /// associated callback.
        pub fn get_requests_from_path_and_method<'b>(
            &self,
            path: &'b str,
            methode: H3Method,
        ) -> Option<(&RequestForm, Option<Vec<&'b str>>)> {
            //if param in path

            let mut path_s = path.to_string();
            let mut param_trail: Option<Vec<&str>> = None;

            if let Some((path, id)) = path.split_once("?") {
                path_s = path.to_string();

                let args_it: Vec<&str> = id.split("&").collect();
                param_trail = Some(args_it);
            }

            if let Some(request_coll) = self.get_requests_from_path(path_s.as_str()) {
                if let Some(found_request) =
                    request_coll.iter().find(|item| item.method() == &methode)
                {
                    return Some((found_request, param_trail));
                }
                None
            } else {
                None
            }
        }
    }

    pub struct RequestManagerBuilder {
        requests_formats: HashMap<ReqPath, Vec<RequestForm>>,
    }
    impl RequestManagerBuilder {
        pub fn build(&mut self) -> RequestManager {
            let request_manager_inner = RequestManagerInner {
                requests_formats: std::mem::replace(&mut self.requests_formats, HashMap::new()),
                request_states: RequestsTable::new(),
            };

            RequestManager {
                inner: Arc::new(Mutex::new(request_manager_inner)),
            }
        }
        ///
        ///Add a new RequestForm to the server.
        ///
        pub fn add_new_request_form(&mut self, request_form: RequestForm) -> &mut Self {
            let path = request_form.path();

            if !self.requests_formats.contains_key(path) {
                self.requests_formats.insert(path, vec![request_form]);
            } else {
                assert!(!self
                    .requests_formats
                    .get(path)
                    .as_ref()
                    .unwrap()
                    .contains(&request_form));
                self.requests_formats
                    .get_mut(path)
                    .expect(&format!("can't access value for {:?}", path))
                    .push(request_form);
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

    pub struct RequestForm {
        method: H3Method,
        path: &'static str,
        scheme: &'static str,
        authority: Option<&'static str>,
        body_cb: Option<
            Box<dyn Fn(RequestEvent) -> Result<RequestResponse, ()> + Send + Sync + 'static>,
        >,
        request_type: RequestType,
    }

    impl PartialEq for RequestForm {
        fn eq(&self, other: &Self) -> bool {
            self.method() == other.method()
                && self.path() == self.path()
                && self.scheme == self.scheme
                && self.authority == other.authority
                && self.request_type() == other.request_type()
        }
    }

    impl RequestForm {
        pub fn new() -> RequestFormBuilder {
            RequestFormBuilder::new()
        }
        pub fn path(&self) -> &'static str {
            self.path
        }
        pub fn method(&self) -> &H3Method {
            &self.method
        }
        pub fn request_type(&self) -> &RequestType {
            &self.request_type
        }

        pub fn build_response(
            &self,
            request_event: RequestEvent,
        ) -> Result<(Vec<h3::Header>, Vec<u8>), ()> {
            if let Some(request_cb) = &self.body_cb {
                if let Ok(mut request_response) = request_cb(request_event) {
                    let headers = request_response.get_headers();
                    let body = request_response.take_body();
                    return Ok((headers, body));
                }
            }
            //to do
            Err(())
        }
    }

    pub struct RequestFormBuilder {
        method: Option<H3Method>,
        path: Option<&'static str>,
        scheme: Option<&'static str>,
        authority: Option<&'static str>,
        body_cb: Option<
            Box<
                dyn Fn(RequestEvent) -> Result<(RequestResponse), ()>
                    /*body, content-type*/
                    + Sync
                    + Send
                    + 'static,
            >,
        >,
        request_type: Option<RequestType>,
    }

    impl RequestFormBuilder {
        pub fn new() -> Self {
            Self {
                method: None,
                path: None,
                scheme: None,
                authority: None,
                body_cb: None,
                request_type: None,
            }
        }

        pub fn build(&mut self) -> RequestForm {
            RequestForm {
                method: self.method.take().unwrap(),
                path: self.path.take().unwrap(),
                scheme: self.scheme.take().expect("expected scheme"),
                authority: self.authority.clone(),
                body_cb: self.body_cb.take(),
                request_type: self.request_type.take().unwrap(),
            }
        }

        ///
        /// Set the callback for the response. It exposes in parameters 0 = the path of the request,
        /// parameters 1 = the list of args if any "/path?id=foo&name=bar&other=etc"
        ///
        ///The callback returns (body, value of content-type field as bytes)
        ///
        pub fn set_request_callback(
            &mut self,
            body_cb: impl Fn(RequestEvent) -> Result<(RequestResponse), ()> + Sync + Send + 'static,
        ) -> &mut Self {
            self.body_cb = Some(Box::new(body_cb));
            self
        }
        ///
        ///Set the http method. H3Method::GET, ::POST, ::PUT, ::DELETE
        ///
        ///
        pub fn set_method(&mut self, method: H3Method) -> &mut Self {
            self.method = Some(method);
            self
        }
        ///
        /// Set the request path as "/home"
        ///
        ///
        pub fn set_path(&mut self, path: &'static str) -> &mut Self {
            self.path = Some(path);
            self
        }
        ///
        /// Set the connexion type : https here
        ///
        pub fn set_scheme(&mut self, scheme: &'static str) -> &mut Self {
            self.scheme = Some(scheme);
            self
        }
        ///
        ///set the request type, in the context of the faces app
        ///
        pub fn set_request_type(&mut self, request_type: RequestType) -> &mut Self {
            self.request_type = Some(request_type);
            self
        }
    }
}

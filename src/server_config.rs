#![allow(warnings)]
pub use request_handler::RequestHandler;
pub use request_manager::{
    H3Method, RequestForm, RequestFormBuilder, RequestManager, RequestManagerBuilder, RequestType,
};
pub use server_config_builder::ServerConfig;
mod server_config_builder {
    use std::{net::SocketAddr, sync::Arc};

    use super::*;

    pub struct ServerConfig {
        server_socket_address: SocketAddr,
        request_manager: Arc<RequestManager>,
        cert: &'static str,
        key: &'static str,
    }

    impl ServerConfig {
        pub fn new() -> ServerConfigBuilder {
            ServerConfigBuilder::new()
        }
        pub fn server_address(&self) -> &SocketAddr {
            &self.server_socket_address
        }
        pub fn cert_path(&self) -> &str {
            self.cert
        }
        pub fn key_path(&self) -> &str {
            self.key
        }
        pub fn request_handler(&self) -> RequestHandler {
            self.request_manager.request_handler()
        }
    }

    pub struct ServerConfigBuilder {
        server_socket_address: Option<&'static str>,
        request_manager: Option<RequestManager>,
        cert: Option<&'static str>,
        key: Option<&'static str>,
    }
    impl ServerConfigBuilder {
        fn new() -> Self {
            ServerConfigBuilder {
                server_socket_address: None,
                request_manager: None,
                cert: None,
                key: None,
            }
        }
        ///
        ///set the previously created RequestManager
        ///
        pub fn set_request_manager(&mut self, request_manager: RequestManager) -> &mut Self {
            self.request_manager = Some(request_manager);
            self
        }
        ///
        ///Set the server the server ip + port. Example "127.0.0.1:3000"
        ///
        pub fn set_address(&mut self, address: &'static str) -> &mut Self {
            self.server_socket_address = Some(address);
            self
        }
        ///
        ///path to the cert. Try absolute path if problems
        ///
        pub fn set_cert_path(&mut self, path: &'static str) -> &mut Self {
            self.cert = Some(path);
            self
        }
        ///
        ///path to the key. Try absolute path if problems
        ///
        pub fn set_key_path(&mut self, path: &'static str) -> &mut Self {
            self.key = Some(path);
            self
        }
        pub fn build(&mut self) -> ServerConfig {
            ServerConfig {
                server_socket_address: self
                    .server_socket_address
                    .as_ref()
                    .unwrap()
                    .parse()
                    .expect("can't get the socket address"),
                request_manager: Arc::new(self.request_manager.take().unwrap()),
                cert: self.cert.take().unwrap_or("no_path_found"),
                key: self.key.take().unwrap_or("no_path_found"),
            }
        }
    }
}

mod request_handler {
    use std::{
        collections::HashMap,
        env::args,
        sync::{Arc, Mutex},
    };

    use crate::server_init::quiche_http3_server;
    use quiche::{
        h3::{self, NameValue},
        Connection,
    };

    use self::request_manager::H3Method;

    use super::*;

    pub struct RequestHandler<'a> {
        request_formats: &'a HashMap<&'static str, Vec<RequestForm>>,
        state: Arc<Mutex<HashMap<String, HashMap<u64, CurrentRequest>>>>, //trace_id of the Connection as
                                                                          //key value is HashMap
                                                                          //for stream_id u64
    }

    pub struct CurrentRequest;

    impl<'a> RequestHandler<'a> {
        pub fn new(request_formats: &'a HashMap<&'static str, Vec<RequestForm>>) -> Self {
            RequestHandler {
                request_formats,
                state: Arc::new(Mutex::new(HashMap::new())),
            }
        }
        pub fn print_entries(&self) {
            for i in self.request_formats.iter() {
                println!("entrie [{}]", i.0);
            }
        }
        pub fn handle_finished_stream(
            &self,
            stream_id: u64,
            connexion: &mut quiche_http3_server::QClient,
        ) {

            /*
             *
             * Send received data to the output queue,
             *
             * clean stream_id in RequestHandler state
             *
             * send response to client
             *
             * */
        }
        pub fn fetch_data_stream(&self, stream_id: u64, connexion: &Connection) {}
        fn get_requests_from_path(&self, path: &str) -> Option<&Vec<RequestForm>> {
            self.request_formats.get(path)
        }
        pub fn parse_headers(
            &self,
            headers: &[h3::Header],
            stream_id: u64,
            client: &mut quiche_http3_server::QClient,
        ) {
            let mut method: Option<&[u8]> = None;
            let mut path: Option<&str> = None;

            for hdr in headers {
                match hdr.name() {
                    b":method" => method = Some(hdr.value()),
                    b":path" => path = Some(std::str::from_utf8(hdr.value()).unwrap()),
                    _ => {}
                }
            }

            if let Some(path) = path {
                if let Some(method) = method {
                    if let Ok(method) = H3Method::parse(method) {
                        if let Some((request_form, sub_path_args)) =
                            self.get_requests_from_path_and_method(path, method)
                        {
                            match request_form.method() {
                                H3Method::GET => {
                                    /*stream shutdown send response*/

                                    client
                                        .conn()
                                        .stream_shutdown(stream_id, quiche::Shutdown::Read, 0)
                                        .unwrap();
                                    if let Ok((headers, body)) =
                                        request_form.build_response(sub_path_args)
                                    {
                                        quiche_http3_server::send_response(
                                            client, stream_id, headers, body,
                                        )
                                    }
                                }
                                H3Method::POST => { /*create entry in handler state*/ }
                                _ => {}
                            }
                        } else {
                            /*Send bad request response*/
                        }
                    };
                }
            }
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
}
mod request_manager {
    use std::collections::HashMap;

    use quiche::h3;

    use super::*;

    type ReqPath = &'static str;

    pub struct RequestManager {
        requests_formats: HashMap<ReqPath, Vec<RequestForm>>,
    }
    impl RequestManager {
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

        pub fn request_handler(&self) -> RequestHandler {
            RequestHandler::new(&self.requests_formats)
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
    }

    pub struct RequestManagerBuilder {
        requests_formats: HashMap<ReqPath, Vec<RequestForm>>,
    }
    impl RequestManagerBuilder {
        pub fn build(&mut self) -> RequestManager {
            RequestManager {
                requests_formats: std::mem::replace(&mut self.requests_formats, HashMap::new()),
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

    #[derive(PartialEq)]
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
        body_cb: Box<
            dyn Fn(&str, Option<Vec<&str>>) -> Result<(Vec<u8>, Vec<u8>), ()>
                + Send
                + Sync
                + 'static,
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
            args: Option<Vec<&str>>,
        ) -> Result<(Vec<h3::Header>, Vec<u8>), ()> {
            if let Ok((body, content_type)) = (self.body_cb)(self.path(), args) {
                let headers = vec![
                    h3::Header::new(b":status", b"200"),
                    h3::Header::new(b"alt-svc", b"h3=\":3000\""),
                    h3::Header::new(b"content-type", &content_type),
                    h3::Header::new(b"content-lenght", body.len().to_string().as_bytes()),
                ];
                return Ok((headers, body));
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
                dyn Fn(&str, Option<Vec<&str>>) -> Result<(Vec<u8>, Vec<u8>), ()>
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
                body_cb: self.body_cb.take().expect("No callback cb set"),
                request_type: self.request_type.take().unwrap(),
            }
        }

        ///
        /// Set the callback for the response. It exposes in parameters 0 = the path of the request,
        /// parameters 1 = the list of args if any "/path?id=foo&name=bar&other=etc"
        ///
        ///The callback returns (body, value of content-type field as bytes)
        ///
        pub fn set_body_callback(
            &mut self,
            body_cb: impl Fn(&str, Option<Vec<&str>>) -> Result<(Vec<u8>, Vec<u8>), ()>
                + Sync
                + Send
                + 'static,
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

mod test {
    use std::{net::SocketAddr, str::FromStr};

    use super::*;

    #[test]
    fn server_config() {
        let address = "127.0.0.1:3000";
        let mut request_manager_builder = RequestManager::new();
        let request_manager = request_manager_builder.build();

        let server_config = ServerConfig::new()
            .set_cert_path("../cert.pem")
            .set_key_path("../key.pem")
            .set_address(address)
            .set_request_manager(request_manager)
            .build();

        assert!(server_config.server_address() == &SocketAddr::from_str("127.0.0.1:3000").unwrap());
    }

    #[test]
    fn request_manager_setup() {
        let mut request_manager_builder = RequestManager::new();

        let new_request = RequestForm::new()
            .set_method(request_manager::H3Method::GET)
            .set_path("/upload")
            .set_scheme("https")
            .set_body_callback(|path, args| Ok((vec![0, 1, 2, 3], b"data".to_vec())))
            .set_request_type(RequestType::Ping)
            .build();

        let new_request_1 = RequestForm::new()
            .set_method(request_manager::H3Method::GET)
            .set_path("/upload")
            .set_scheme("https")
            .set_body_callback(|path, args| Ok((vec![0, 1, 2, 3], b"data".to_vec())))
            .set_request_type(RequestType::Message("salut !".to_string()))
            .build();
        let new_request_2 = RequestForm::new()
            .set_method(request_manager::H3Method::GET)
            .set_path("/")
            .set_body_callback(|path, args| Ok((vec![0, 1, 2, 3], b"data".to_vec())))
            .set_scheme("https")
            .set_request_type(RequestType::Ping)
            .build();
        let new_request_3 = RequestForm::new()
            .set_method(request_manager::H3Method::GET)
            .set_path("/time")
            .set_body_callback(|path, args| Ok((vec![0, 1, 2, 3], b"data".to_vec())))
            .set_scheme("https")
            .set_request_type(RequestType::Ping)
            .build();
        request_manager_builder.add_new_request_form(new_request);
        request_manager_builder.add_new_request_form(new_request_1);
        request_manager_builder.add_new_request_form(new_request_2);
        request_manager_builder.add_new_request_form(new_request_3);

        let request_manager = request_manager_builder.build();

        let found_request = request_manager.get_requests_from_path("upload");
        let found_request_2 = request_manager.get_requests_from_path_and_method_and_request_type(
            "/",
            request_manager::H3Method::GET,
            RequestType::Ping,
        );
        let found_request_3 = request_manager.get_requests_from_path("/upload");

        assert!(found_request.is_none());
        assert!(found_request_3.is_some());
        assert!(found_request_2.is_some());

        let request_handler = request_manager.request_handler();

        let found_request_4 =
            request_handler.get_requests_from_path_and_method("/", request_manager::H3Method::GET);

        assert!(found_request_4.is_some());
        let found_request_5 = request_handler.get_requests_from_path_and_method(
            "/time?id=42&name=Jon",
            request_manager::H3Method::GET,
        );

        assert!(found_request_5.is_some());
    }
}

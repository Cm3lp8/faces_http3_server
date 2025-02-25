#![allow(warnings)]
pub use crate::request_handler::RequestHandler;

pub use crate::request_manager::{
    H3Method, RequestForm, RequestFormBuilder, RequestManager, RequestManagerBuilder, RequestType,
};
pub use crate::server_init::QClient;
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
        ///Set the server the server ip + port. Example "127.0.0.1:300gg0"
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

mod test {
    use std::{net::SocketAddr, str::FromStr};

    use crate::request_response::RequestResponse;

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
            .set_method(H3Method::GET)
            .set_path("/upload")
            .set_scheme("https")
            .set_request_callback(|req_event| Ok(RequestResponse::new_ok_200()))
            .set_request_type(RequestType::Ping)
            .build();

        let new_request_1 = RequestForm::new()
            .set_method(H3Method::GET)
            .set_path("/upload")
            .set_scheme("https")
            .set_request_callback(|req_event| Ok(RequestResponse::new_ok_200()))
            .set_request_type(RequestType::Message("salut !".to_string()))
            .build();
        let new_request_2 = RequestForm::new()
            .set_method(H3Method::GET)
            .set_path("/")
            .set_request_callback(|req_event| Ok(RequestResponse::new_ok_200()))
            .set_scheme("https")
            .set_request_type(RequestType::Ping)
            .build();
        let new_request_3 = RequestForm::new()
            .set_method(H3Method::GET)
            .set_path("/time")
            .set_request_callback(|req_event| Ok(RequestResponse::new_ok_200()))
            .set_scheme("https")
            .set_request_type(RequestType::Ping)
            .build();
        request_manager_builder.add_new_request_form(new_request);
        request_manager_builder.add_new_request_form(new_request_1);
        request_manager_builder.add_new_request_form(new_request_2);
        request_manager_builder.add_new_request_form(new_request_3);

        let request_manager = request_manager_builder.build();

        /*
                let found_request = request_manager.get_requests_from_path("upload");
                let found_request_2 = request_manager.get_requests_from_path_and_method_and_request_type(
                    "/",
                    H3Method::GET,
                    RequestType::Ping,
                );
                let found_request_3 = request_manager.get_requests_from_path("/upload");

                assert!(found_request.is_none());
                assert!(found_request_3.is_some());
                assert!(found_request_2.is_some());
        */
        let request_handler = request_manager.request_handler();

        let found_request_4 = request_handler.get_requests_from_path_and_method("/", H3Method::GET);

        //  assert!(found_request_4.is_some());
        let found_request_5 = request_handler
            .get_requests_from_path_and_method("/time?id=42&name=Jon", H3Method::GET);

        // assert!(found_request_5.is_some());
    }
}

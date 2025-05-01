#![allow(warnings)]

pub use crate::route_manager::{
    BodyStorage, H3Method, RequestType, RouteConfig, RouteForm, RouteFormBuilder, RouteHandler,
    RouteManager, RouteManagerBuilder,
};
pub use crate::server_init::QClient;
pub use server_config_builder::{ServerConfig, ServerConfigBuilder};
mod server_config_builder {
    use std::{
        net::SocketAddr,
        path::{Path, PathBuf},
        sync::Arc,
    };

    use crate::route_handler::RouteHandler;

    use super::*;

    pub struct ServerConfig {
        server_socket_address: SocketAddr,
        file_storage_path: PathBuf,
        cert: String,
        key: String,
    }

    impl ServerConfig {
        pub fn new() -> ServerConfigBuilder {
            ServerConfigBuilder::new()
        }
        pub fn server_address(&self) -> &SocketAddr {
            &self.server_socket_address
        }
        pub fn cert_path(&self) -> &str {
            self.cert.as_str()
        }
        pub fn key_path(&self) -> &str {
            self.key.as_str()
        }
        pub fn get_storage_path(&self) -> PathBuf {
            self.file_storage_path.to_path_buf()
        }
    }

    pub struct ServerConfigBuilder {
        server_socket_address: Option<String>,
        file_storage_path: Option<PathBuf>,
        cert: Option<PathBuf>,
        key: Option<PathBuf>,
    }
    impl ServerConfigBuilder {
        fn new() -> Self {
            ServerConfigBuilder {
                server_socket_address: None,
                file_storage_path: None,
                cert: None,
                key: None,
            }
        }
        ///Set the server the server ip + port. Example "127.0.0.1:300gg0"
        ///
        pub fn set_address(&mut self, address: &str) -> &mut Self {
            self.server_socket_address = Some(address.to_owned());
            self
        }
        ///
        ///path to the cert. Try absolute path if problems
        ///
        pub fn set_cert_path(&mut self, path: impl AsRef<Path>) -> &mut Self {
            self.cert = Some(path.as_ref().to_path_buf());
            self
        }
        pub fn set_file_storage_path(&mut self, path: impl AsRef<Path>) -> &mut Self {
            self.file_storage_path = Some(path.as_ref().to_path_buf());
            self
        }
        ///
        ///path to the key. Try absolute path if problems
        ///
        pub fn set_key_path(&mut self, path: impl AsRef<Path>) -> &mut Self {
            self.key = Some(path.as_ref().to_path_buf());
            self
        }
        pub fn build(&mut self) -> ServerConfig {
            if let Err(_) = std::fs::create_dir_all(self.file_storage_path.as_ref().unwrap()) {
                error!("failed creating path");
            }
            ServerConfig {
                server_socket_address: self
                    .server_socket_address
                    .as_ref()
                    .unwrap()
                    .parse()
                    .expect("can't get the socket address"),
                file_storage_path: self
                    .file_storage_path
                    .as_ref()
                    .unwrap_or_else(|| {
                        error!("No file storage path set ");
                        panic!()
                    })
                    .clone(),
                cert: self
                    .cert
                    .as_ref()
                    .unwrap_or_else(|| {
                        error!("No certification file set ");
                        panic!()
                    })
                    .to_string_lossy()
                    .to_string(),
                key: self
                    .key
                    .as_ref()
                    .unwrap_or_else(|| {
                        error!("No key file set ");
                        panic!()
                    })
                    .to_string_lossy()
                    .to_string(),
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
        let mut request_manager_builder = RouteManager::new_with_app_state(());
        let request_manager = request_manager_builder.build();

        let server_config = ServerConfig::new()
            .set_cert_path("../cert.pem")
            .set_key_path("../key.pem")
            .set_address(address)
            .build();

        assert!(server_config.server_address() == &SocketAddr::from_str("127.0.0.1:3000").unwrap());
    }

    #[test]
    fn request_manager_setup() {
        /*
                let mut request_manager_builder = RouteManager::new();

                let new_request = RouteForm::new("/upload", H3Method::GET, RouteConfig::default())
                    .set_route_callback(|req_event| Ok(RequestResponse::new_ok_200()))
                    .set_request_type(RequestType::Ping)
                    .build();

                let new_request_1 = RouteForm::new("/upload", H3Method::GET, RouteConfig::default())
                    .set_route_callback(|req_event| Ok(RequestResponse::new_ok_200()))
                    .set_request_type(RequestType::Message("salut !".to_string()))
                    .build();
                let new_request_2 = RouteForm::new("/", H3Method::GET, RouteConfig::default())
                    .set_route_callback(|req_event| Ok(RequestResponse::new_ok_200()))
                    .set_request_type(RequestType::Ping)
                    .build();
                let new_request_3 = RouteForm::new("/", H3Method::GET, RouteConfig::default())
                    .set_route_callback(|req_event| Ok(RequestResponse::new_ok_200()))
                    .set_request_type(RequestType::Ping)
                    .build();
                request_manager_builder.add_new_route(new_request);
                request_manager_builder.add_new_route(new_request_1);
                request_manager_builder.add_new_route(new_request_2);
                request_manager_builder.add_new_route(new_request_3);

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
                let request_handler = request_manager.routes_handler();

                let found_request_4 = request_handler.get_routes_from_path_and_method("/", H3Method::GET);

                //  assert!(found_request_4.is_some());
                let found_request_5 =
                    request_handler.get_routes_from_path_and_method("/time?id=42&name=Jon", H3Method::GET);

                // assert!(found_request_5.is_some());
        */
    }
}

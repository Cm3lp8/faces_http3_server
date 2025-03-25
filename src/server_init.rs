pub mod quiche_http3_server;
pub use quiche_http3_server::QClient;
pub use server_initiation::Http3Server;

mod server_initiation {
    use std::{path::Path, sync::Arc, thread};

    use crate::{
        route_manager::{RouteForm, RouteFormBuilder},
        server_config::ServerConfigBuilder,
        H3Method, RouteConfig, RouteManager, RouteManagerBuilder, ServerConfig,
    };

    use super::*;

    pub struct Http3Server {
        server_config: Arc<ServerConfig>,
    }

    impl Http3Server {
        pub fn new(socket: &'static str) -> Http3ServerBuilder {
            let mut server_config_builder = ServerConfig::new();
            server_config_builder.set_address(socket);
            Http3ServerBuilder {
                server_config: server_config_builder,
            }
        }
    }

    pub struct Http3ServerBuilder {
        server_config: ServerConfigBuilder,
    }

    impl Http3ServerBuilder {
        pub fn add_cert_path(&mut self, path: impl AsRef<Path> + 'static) -> &mut Self {
            if let Some(cert_path) = path.as_ref().to_str() {
                self.server_config.set_cert_path(cert_path);
            }
            self
        }
        pub fn set_file_storage_path(&mut self, path: impl AsRef<Path>) -> &mut Self {
            self.server_config.set_file_storage_path(path);
            self
        }
        pub fn add_key_path(&mut self, path: impl AsRef<Path> + 'static) -> &mut Self {
            if let Some(key_path) = path.as_ref().to_str() {
                self.server_config.set_key_path(key_path);
            }
            self
        }
        /*
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
                    self.add_route(path, H3Method::POST, route_configuration, route);
                    self
                }
        */
        /*
                pub fn add_route(
                    &mut self,
                    path: &'static str,
                    method: H3Method,
                    route_configuration: RouteConfig,
                    route: impl FnOnce(&mut RouteFormBuilder),
                ) -> &mut Self {
                    let mut route_form = RouteForm::new(path, method, route_configuration);

                    route(&mut route_form);

                    let route = route_form.build();
                    self.route_manager.add_new_route(route);
                    self
                }
        */
        pub fn run_blocking<S: 'static + Send + Sync>(
            &mut self,
            route_manager: RouteManager<S>,
        ) -> Http3Server {
            let config_clone = self.server_config.build();
            let config_clone = Arc::new(config_clone);
            let server = Http3Server {
                server_config: config_clone.clone(),
            };
            std::thread::spawn(move || {
                quiche_http3_server::run(config_clone, route_manager);
            });
            thread::park();
            server
        }
        pub fn run<S: 'static + Send + Sync>(
            &mut self,
            route_manager: RouteManager<S>,
        ) -> Http3Server {
            let config_clone = self.server_config.build();
            let config_clone = Arc::new(config_clone);
            let server = Http3Server {
                server_config: config_clone.clone(),
            };
            std::thread::spawn(move || {
                quiche_http3_server::run(config_clone, route_manager);
            });
            server
        }
    }
}

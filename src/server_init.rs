pub mod quiche_http3_server;
pub use quiche_http3_server::QClient;
pub use server_initiation::Http3Server;

mod server_initiation {
    use std::{path::Path, sync::Arc, thread};

    use crate::{
        handler_dispatcher,
        route_manager::{RouteForm, RouteFormBuilder},
        server_config::ServerConfigBuilder,
        stream_sessions::UserSessions,
        EventLoop, H3Method, RouteConfig, RouteEvent, RouteManager, RouteManagerBuilder,
        ServerConfig,
    };

    use super::*;

    pub struct Http3Server {
        server_config: Arc<ServerConfig>,
    }

    impl Http3Server {
        pub fn new(socket: &str) -> Http3ServerBuilder {
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
        pub fn run_blocking<S: 'static + Send + Sync + Clone, T: UserSessions<Output = T>>(
            &mut self,
            mut route_manager_builder: RouteManagerBuilder<S, T>,
        ) -> Http3Server {
            let config_clone = self.server_config.build();
            let config_clone = Arc::new(config_clone);
            let server = Http3Server {
                server_config: config_clone.clone(),
            };

            let app_state = route_manager_builder.app_state().expect("no app_state set");
            let route_event_dispatcher = route_manager_builder.build_route_event_dispatcher();
            let event_loop = EventLoop::new(route_event_dispatcher, app_state);
            route_manager_builder.attach_event_loop(event_loop.clone());

            let route_manager = route_manager_builder.build();

            std::thread::spawn(move || {
                quiche_http3_server::run(config_clone, route_manager);
            });

            event_loop.run(
                |event, app_state, handler_dispatcher, response_builder| match event {
                    RouteEvent::OnFinished(event) => {
                        let handler_dispatcher = handler_dispatcher.clone();
                        let app_state = app_state.clone();
                        std::thread::spawn(move || {
                            let response: crate::RouteResponse =
                                handler_dispatcher.dispatch_finished(event, &app_state);

                            response_builder.build_response(response);
                        });
                    }
                    RouteEvent::OnData(_data) => {}
                    _ => {}
                },
            );
            thread::park();
            server
        }
        pub fn run<S: 'static + Send + Sync + Clone, T: UserSessions<Output = T>>(
            &mut self,
            route_manager: RouteManager<S, T>,
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

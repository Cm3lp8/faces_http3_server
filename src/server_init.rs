pub mod quiche_http3_server;
mod retry_token;
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
        ServerConfig, ServerRunError,
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
            self.server_config.set_cert_path(path);
            self
        }
        pub fn set_file_storage_path(&mut self, path: impl AsRef<Path>) -> &mut Self {
            self.server_config.set_file_storage_path(path);
            self
        }
        pub fn add_key_path(&mut self, path: impl AsRef<Path> + 'static) -> &mut Self {
            self.server_config.set_key_path(path);
            self
        }
        /// Waits for the actual network worker. Any worker termination is an error;
        /// this API does not implement graceful shutdown or supervise secondary workers.
        pub fn try_run_blocking<S: 'static + Send + Sync + Clone, T: UserSessions<Output = T>>(
            &mut self,
            mut route_manager_builder: RouteManagerBuilder<S, T>,
        ) -> Result<(), ServerRunError> {
            let config = Arc::new(self.server_config.try_build()?);
            let worker = thread::Builder::new()
                .name("faces-network".to_owned())
                .spawn(move || {
                    let route_manager = route_manager_builder.build();
                    quiche_http3_server::run(config, route_manager)
                })
                .map_err(|error| ServerRunError::io("worker_spawn", error))?;
            match worker.join() {
                Ok(Err(error)) => Err(error),
                Ok(Ok(())) => Err(ServerRunError::WorkerStopped),
                Err(_) => Err(ServerRunError::WorkerPanicked),
            }
        }

        /// Source-compatible blocking facade: worker failure panics on the caller.
        /// A consumer may catch that panic; process exit is the consumer's responsibility.
        /// Prefer `try_run_blocking` to propagate the error explicitly.
        pub fn run_blocking<S: 'static + Send + Sync + Clone, T: UserSessions<Output = T>>(
            &mut self,
            route_manager_builder: RouteManagerBuilder<S, T>,
        ) -> Http3Server {
            match self.try_run_blocking(route_manager_builder) {
                Err(error) => panic!("blocking server failed: {error}"),
                Ok(()) => panic!("blocking server failed: {}", ServerRunError::WorkerStopped),
            }
        }
        /// Starts a detached worker. Unlike `try_run_blocking`, this legacy API does
        /// not report worker termination to its caller.
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
                if let Err(error) = quiche_http3_server::run(config_clone, route_manager) {
                    error!("detached server failed: {error}");
                }
            });
            server
        }
    }
}

#[cfg(test)]
mod retry_loopback;

#[cfg(test)]
mod startup_tests;

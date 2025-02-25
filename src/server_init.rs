pub mod quiche_http3_server;
pub use quiche_http3_server::QClient;
pub use server_initiation::Http3Server;

mod server_initiation {
    use std::sync::Arc;

    use crate::ServerConfig;

    use super::*;

    pub struct Http3Server {
        server_config: Arc<ServerConfig>,
    }

    impl Http3Server {
        pub fn new(config: ServerConfig) -> Http3Server {
            Self {
                server_config: Arc::new(config),
            }
        }

        pub fn run(&self) {
            let config_clone = self.server_config.clone();
            std::thread::spawn(move || {
                quiche_http3_server::run(config_clone);
            });
        }
    }
}

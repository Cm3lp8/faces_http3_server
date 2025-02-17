#![allow(warnings)]
pub mod quiche_http3_server;

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

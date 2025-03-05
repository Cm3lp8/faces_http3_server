#[macro_use]
extern crate log;

pub use event_listener::RouteEventListener;
pub use request_events::{DataEvent, RouteEvent};
pub use request_response::{ContentType, RequestResponse, Status};
pub use route_manager::DataManagement;
pub use server_config::{
    BodyStorage, H3Method, RequestType, RouteConfig, RouteForm, RouteHandler, RouteManager,
    RouteManagerBuilder, ServerConfig,
};
mod event_listener;
mod request_events;
mod request_response;
mod route_handler;
mod route_manager;
mod server_config;
mod server_init;
pub use server_init::Http3Server;
#[cfg(test)]
mod tests {
    use super::*;

    /*
    #[test]
    fn it_works() {
        let result = add(2, 2);
        assert_eq!(result, 4);
    }
    */
}

#[macro_use]
extern crate log;

pub use event_listener::{EventLoop, RouteEventListener};
use file_writer::FileWriter;
pub use handler_dispatcher::RouteHandle;
pub use middleware::{HeadersColl, MiddleWare, MiddleWareResult};
pub use request_response::{ContentType, RequestResponse, Status};
pub use route_events::{
    DataEvent, EventResponseChannel, EventResponseWaiter, ResponseBuilderSender, RouteEvent,
};
pub use route_manager::DataManagement;
pub use server_config::{
    BodyStorage, H3Method, RequestType, RouteConfig, RouteForm, RouteHandler, RouteManager,
    RouteManagerBuilder, ServerConfig,
};
use server_init::quiche_http3_server::{BodyReqQueue, QueueTrackableItem};
mod event_listener;
mod file_writer;
mod middleware;
mod request_response;
mod route_events;
mod route_handler;
mod route_manager;
mod server_config;
mod server_init;
pub use server_init::Http3Server;
mod conn_statistics;
mod handler_dispatcher;
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

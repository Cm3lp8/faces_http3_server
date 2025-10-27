#[macro_use]
extern crate log;

pub use event_listener::{EventLoop, RouteEventListener};
use file_writer::FileWriter;
pub use handler_dispatcher::{ErrorResponse, Response, RouteHandle, RouteResponse};
pub use middleware::{HeadersColl, MiddleWare, MiddleWareFlow, MiddleWareResult};
pub use quiche::h3;
pub use request_response::{ContentType, RequestResponse, Status};
pub use route_events::{
    DataEvent, EventResponseChannel, EventResponseWaiter, FinishedEvent, ResponseBuilderSender,
    RouteEvent,
};
pub use route_manager::{DataManagement, ErrorType};
pub use server_config::{
    BodyStorage, H3Method, RequestType, RouteConfig, RouteForm, RouteHandler, RouteManager,
    RouteManagerBuilder, ServerConfig,
};

use stream_sessions::StreamCreation;

pub use stream_sessions::StreamBridge;
pub use stream_sessions::StreamBridgeOps;
pub use stream_sessions::StreamHandle;
pub use stream_sessions::StreamSessions;
pub use stream_sessions::UserSessions;
pub use stream_sessions::{StreamIdent, StreamManagement, ToStreamIdent};

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
mod stream_sessions;
pub use server_init::Http3Server;
mod conn_statistics;
mod handler_dispatcher;
mod header_queue_processing;
mod response_queue_processing;
mod routes_macros;

pub mod prelude {

    pub use crate::handler_dispatcher::{ErrorResponse, Response, RouteHandle, RouteResponse};
    pub use crate::middleware::{HeadersColl, MiddleWare, MiddleWareFlow, MiddleWareResult};
    pub use crate::request_response::{ContentType, RequestResponse, Status};
    pub use crate::route_events::{
        DataEvent, EventResponseChannel, EventResponseWaiter, FinishedEvent, ResponseBuilderSender,
        RouteEvent,
    };
    pub use crate::route_handle;
    pub use crate::route_manager::{DataManagement, ErrorType};
    pub use crate::server_config::{
        BodyStorage, H3Method, RequestType, RouteConfig, RouteForm, RouteHandler, RouteManager,
        RouteManagerBuilder, ServerConfig,
    };
    pub use crate::server_init::Http3Server;
    pub use crate::stream_sessions::StreamMessageCapsule;
    pub use crate::StreamBridge;
    pub use crate::StreamHandle;
    pub use crate::StreamIdent;
    pub use crate::StreamSessions;
    pub use crate::ToStreamIdent;
    pub use crate::UserSessions;
    pub use quiche::h3;
}

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

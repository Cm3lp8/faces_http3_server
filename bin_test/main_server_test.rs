#![allow(warnings)]
use std::io::{BufRead, BufReader, Cursor};
use std::ops::Add;
use std::sync::Arc;
use std::{thread, usize};

use faces_quic_server::{
    BodyStorage, ContentType, DataEvent, DataManagement, EventLoop, EventResponseChannel,
    FinishedEvent, H3Method, HeadersColl, Http3Server, MiddleWare, MiddleWareResult,
    RequestResponse, Response, ResponseBuilderSender, RouteConfig, RouteEvent, RouteEventListener,
    RouteForm, RouteHandle, RouteResponse,
};
use faces_quic_server::{RequestType, RouteManager, RouteManagerBuilder, ServerConfig};
use log::{error, info, warn};
fn main() {
    env_logger::init();
    let addr = "192.168.1.22:3000";

    let app_state = ();

    struct HandlerTest;
    impl RouteHandle for HandlerTest {
        fn call(&self, event: FinishedEvent, current_status_response: RouteResponse) -> Response {
            info!(
                "Received Data on file path [{}] on [{:?}] ",
                event.bytes_written(),
                event.path()
            );

            Response::ok_200_with_data(event, vec![0; 135_000_000])
        }
    }

    struct HandlerTestMini;
    impl RouteHandle for HandlerTestMini {
        fn call(&self, event: FinishedEvent, current_status_response: RouteResponse) -> Response {
            info!(
                "Received Data on file path [{}] on [{:?}] ",
                event.bytes_written(),
                event.path()
            );

            Response::ok_200_with_data(event, vec![0; 135])
        }
    }
    pub struct AppStateTest;
    let mut router = RouteManager::new_with_app_state(AppStateTest);

    router.global_middleware(Arc::new(MyGlobalMiddleWare));

    struct MyGlobalMiddleWare;
    impl MyGlobalMiddleWare {
        fn inform(headers: &HeadersColl) {
            //  info!("[This is a global_middleware]");
        }
    }
    struct MyMiddleWareTest;
    impl MyMiddleWareTest {
        fn inform(headers: &HeadersColl) {
            info!(
                "\n\n### Incoming request: \n\n|||\n{}|||\n",
                headers.display()
            )
        }
    }
    impl MiddleWare<AppStateTest> for MyGlobalMiddleWare {
        fn on_header(&self, headers: &HeadersColl, app_stat: &AppStateTest) -> MiddleWareResult {
            Self::inform(headers);
            MiddleWareResult::Continue
        }
    }

    impl MiddleWare<AppStateTest> for MyMiddleWareTest {
        fn on_header(&self, headers: &HeadersColl, app_stat: &AppStateTest) -> MiddleWareResult {
            Self::inform(headers);
            MiddleWareResult::Continue
        }
    }

    router.route_post(
        "/large_data",
        RouteConfig::new(DataManagement::Storage(BodyStorage::File)),
        |route_builder| {
            route_builder.middleware(Arc::new(MyMiddleWareTest));
            route_builder.handler(Arc::new(HandlerTest));
        },
    );
    router.route_get("/test", RouteConfig::default(), |route_builder| {
        route_builder.handler(Arc::new(HandlerTest));
        route_builder.middleware(Arc::new(MyMiddleWareTest));
    });
    router.route_get("/test_mini", RouteConfig::default(), |route_builder| {
        route_builder.handler(Arc::new(HandlerTestMini));
        route_builder.middleware(Arc::new(MyMiddleWareTest));
    });

    let _server = Http3Server::new(addr)
        .add_key_path("/home/camille/Documents/rust/faces_http3_server/key.pem")
        .add_cert_path("/home/camille/Documents/rust/faces_http3_server/cert.pem")
        .set_file_storage_path("/home/camille/.temp_server/")
        .run_blocking(router);
}

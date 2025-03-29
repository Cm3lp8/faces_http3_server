#![allow(warnings)]
use std::io::{BufRead, BufReader, Cursor};
use std::ops::Add;
use std::sync::Arc;
use std::{thread, usize};

use faces_quic_server::{
    BodyStorage, ContentType, DataEvent, DataManagement, EventLoop, EventResponseChannel, H3Method,
    HeadersColl, Http3Server, MiddleWare, MiddleWareResult, RequestResponse, ResponseBuilderSender,
    RouteConfig, RouteEvent, RouteEventListener, RouteForm, RouteHandle,
};
use faces_quic_server::{RequestType, RouteManager, RouteManagerBuilder, ServerConfig};
use log::{error, info, warn};
fn main() {
    env_logger::init();
    let addr = "192.168.1.22:3000";

    let app_state = ();

    struct HandlerTest;
    impl RouteHandle for HandlerTest {}

    pub struct AppStateTest;
    let mut router = RouteManager::new_with_app_state(AppStateTest);

    router.global_middleware(Arc::new(MyGlobalMiddleWare));

    struct MyGlobalMiddleWare;
    impl MyGlobalMiddleWare {
        fn inform(headers: &HeadersColl) {
            info!("[This is a global_middleware]");
        }
    }
    struct MyMiddleWareTest;
    impl MyMiddleWareTest {
        fn inform(headers: &HeadersColl) {
            warn!("|||\n{}|||", headers.display())
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
        },
    );
    router.route_get("/test", RouteConfig::default(), |route_builder| {
        route_builder.handler(Arc::new(HandlerTest));
        route_builder.middleware(Arc::new(MyMiddleWareTest));
    });

    let _server = Http3Server::new(addr)
        .add_key_path("/home/camille/Documents/rust/faces_http3_server/key.pem")
        .add_cert_path("/home/camille/Documents/rust/faces_http3_server/cert.pem")
        .set_file_storage_path("/home/camille/.temp_server/")
        .run_blocking(router);
}

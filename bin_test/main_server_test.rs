#![allow(warnings)]
use std::io::{BufRead, BufReader, Cursor};
use std::ops::Add;
use std::sync::Arc;
use std::{thread, usize};

use faces_quic_server::{
    BodyStorage, ContentType, DataEvent, DataManagement, EventLoop, EventResponseChannel, H3Method,
    HeadersColl, Http3Server, MiddleWare, MiddleWareResult, RequestResponse, ResponseBuilderSender,
    RouteConfig, RouteEvent, RouteEventListener, RouteForm,
};
use faces_quic_server::{RequestType, RouteManager, RouteManagerBuilder, ServerConfig};
use log::{error, info, warn};
fn main() {
    env_logger::init();
    let addr = "192.168.1.22:3000";

    let app_state = ();

    pub struct AppStateTest;
    let mut router = RouteManager::new_with_app_state(AppStateTest);

    router.global_middleware(Arc::new(MyGlobalMiddleWare));

    let event_loop = EventLoop::new();

    event_loop.run(|event, response_builder| match event {
        RouteEvent::OnHeader(event) => match event.path() {
            "/large_data" => {}
            "/test" => {}
            _ => {}
        },
        RouteEvent::OnFinished(event) => {
            match event.path() {
                "/large_data" => {
                    if let Some(file_path) = event.get_file_path() {
                        info!(
                            "[{}] bytes writtent on Le chemin : \n{:#?}",
                            event.bytes_written(),
                            file_path
                        );
                    }
                    if let Err(e) =
                        response_builder.send_ok_200_with_file("/home/camille/Vidéos/vid2.mp4")
                    {
                        log::error!("Failed to send response");
                    }
                }
                "/test" => {
                    if let Err(e) =
                        response_builder.send_ok_200_with_file("/home/camille/Vidéos/vid2.mp4")
                    {
                        log::error!("Failed to send response");
                    }
                }
                _ => {}
            };
        }
        RouteEvent::OnData(data) => {
            response_builder.send_ok_200();
        }
        _ => {}
    });

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
            route_builder.subscribe_event(event_loop.clone());
            route_builder.middleware(Arc::new(MyMiddleWareTest));
        },
    );
    router.route_get("/test", RouteConfig::default(), |route_builder| {
        route_builder.subscribe_event(event_loop.clone());
        route_builder.middleware(Arc::new(MyMiddleWareTest));
    });

    let _server = Http3Server::new(addr)
        .add_key_path("/home/camille/Documents/rust/faces_http3_server/key.pem")
        .add_cert_path("/home/camille/Documents/rust/faces_http3_server/cert.pem")
        .set_file_storage_path("/home/camille/.temp_server/")
        .run_blocking(router.build());
}

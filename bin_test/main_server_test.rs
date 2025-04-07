#![allow(warnings)]
use std::io::{BufRead, BufReader, Cursor};
use std::ops::Add;
use std::sync::Arc;
use std::{thread, usize};

use faces_quic_server::{
    BodyStorage, ContentType, DataEvent, DataManagement, ErrorType, EventLoop,
    EventResponseChannel, FinishedEvent, H3Method, HeadersColl, Http3Server, MiddleWare,
    MiddleWareFlow, MiddleWareResult, RequestResponse, Response, ResponseBuilderSender,
    RouteConfig, RouteEvent, RouteEventListener, RouteForm, RouteHandle, RouteResponse,
};
use faces_quic_server::{RequestType, RouteManager, RouteManagerBuilder, ServerConfig};
use log::{error, info, warn};
use quiche::h3;
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

            Response::ok_200_with_data(event, vec![0; 1888835])
            //Response::ok_200(event)
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

            Response::ok_200_with_data(event, vec![0; 10000000])
            // Response::ok_200(event)
        }
    }
    #[derive(Clone)]
    pub struct AppStateTest;
    let mut router = RouteManager::new_with_app_state(AppStateTest);

    router.global_middleware(Arc::new(MyGlobalMiddleWare));

    struct MyGlobalMiddleWare;
    impl MyGlobalMiddleWare {
        fn inform(headers: &HeadersColl) {
            //  info!("[This is a global_middleware]");
        }
    }
    struct MiddleWareForcedError;
    impl MiddleWare<AppStateTest> for MiddleWareForcedError {
        fn on_header<'a>(
            &self,
            headers: &HeadersColl<'a>,
            app_stat: &AppStateTest,
        ) -> MiddleWareFlow {
            MiddleWareFlow::Abort(faces_quic_server::ErrorResponse::Error401(None))
        }
        fn callback(
            &self,
        ) -> Box<
            dyn FnMut(&mut [h3::Header], &AppStateTest) -> MiddleWareFlow + Send + Sync + 'static,
        > {
            let cb = Box::new(|headers: &mut [h3::Header], app_state: &AppStateTest| {
                MiddleWareFlow::Continue
            });
            cb
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
        fn on_header(&self, headers: &HeadersColl, app_stat: &AppStateTest) -> MiddleWareFlow {
            Self::inform(headers);
            MiddleWareFlow::Continue
        }
        fn callback(
            &self,
        ) -> Box<
            dyn FnMut(&mut [h3::Header], &AppStateTest) -> MiddleWareFlow + Send + Sync + 'static,
        > {
            let cb = Box::new(|headers: &mut [h3::Header], app_state: &AppStateTest| {
                MiddleWareFlow::Continue
            });
            cb
        }
    }

    impl MiddleWare<AppStateTest> for MyMiddleWareTest {
        fn on_header(&self, headers: &HeadersColl, app_stat: &AppStateTest) -> MiddleWareFlow {
            Self::inform(headers);
            MiddleWareFlow::Continue
        }
        fn callback(
            &self,
        ) -> Box<
            dyn FnMut(&mut [h3::Header], &AppStateTest) -> MiddleWareFlow + Send + Sync + 'static,
        > {
            let cb = Box::new(|headers: &mut [h3::Header], app_state: &AppStateTest| {
                MiddleWareFlow::Continue
            });
            cb
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
        route_builder
            .middleware(Arc::new(MyMiddleWareTest))
            .middleware(Arc::new(MiddleWareForcedError));
    });

    router.set_error_handler(ErrorType::Error404, |error_buidler| {
        error_buidler.header(&[h3::Header::new(b"type", b"bad request")]);
    });
    let _server = Http3Server::new(addr)
        .add_key_path("/home/camille/Documents/rust/faces_http3_server/key.pem")
        .add_cert_path("/home/camille/Documents/rust/faces_http3_server/cert.pem")
        .set_file_storage_path("/home/camille/.temp_server/")
        .run_blocking(router);
}

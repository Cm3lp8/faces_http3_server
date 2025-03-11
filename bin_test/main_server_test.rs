#![allow(warnings)]
use std::io::{BufRead, BufReader, Cursor};
use std::ops::Add;
use std::sync::Arc;
use std::thread;

use faces_quic_server::{
    BodyStorage, ContentType, DataEvent, DataManagement, EventLoop, EventResponseChannel, H3Method,
    Http3Server, RequestResponse, ResponseBuilderSender, RouteConfig, RouteEvent,
    RouteEventListener, RouteForm,
};
use faces_quic_server::{RequestType, RouteManager, RouteManagerBuilder, ServerConfig};
use log::{info, warn};
fn main() {
    env_logger::init();
    let addr = "192.168.1.22:3000";

    let mut router = RouteManager::new();

    let event_loop = EventLoop::new();

    event_loop.run(|event, response_builder| match event {
        RouteEvent::OnFinished(event) => {
            if let Some(file_path) = event.get_file_path() {
                info!(
                    "[{}] bytes writtent on Le chemin : \n{:#?}",
                    event.bytes_written(),
                    file_path
                );
            }
            if let Err(e) = response_builder.send_ok_200_with_file("/home/camille/Vidéos/vid2.mp4")
            {
                log::error!("Failed to send response");
            }
        }
        RouteEvent::OnData(data) => {
            response_builder.send_ok_200();
        }
        _ => {}
    });

    router.route_post(
        "/large_data",
        RouteConfig::new(DataManagement::Storage(BodyStorage::File)),
        |route_builder| {
            route_builder.subscribe_event(event_loop.clone());
        },
    );
    router.route_get("/test", RouteConfig::default(), |route_builder| {
        route_builder.subscribe_event(event_loop.clone());
    });

    let _server = Http3Server::new(addr)
        .add_key_path("/home/camille/Documents/rust/faces_http3_server/key.pem")
        .add_cert_path("/home/camille/Documents/rust/faces_http3_server/cert.pem")
        .set_file_storage_path("/home/camille/.temp_server/")
        .run_blocking(router.build());
}

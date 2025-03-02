use std::ops::Add;
use std::thread;

use faces_quic_server::{ContentType, H3Method, Http3Server, RequestForm, RequestResponse};
use faces_quic_server::{RequestManager, RequestManagerBuilder, RequestType, ServerConfig};
use log::info;
fn main() {
    env_logger::init();
    let addr = "127.0.0.1:3000";
    let mut request_manager = RequestManager::new();

    let request_form_0 = RequestForm::new()
        .set_path("/")
        .set_method(H3Method::GET)
        .set_scheme("https")
        .set_request_callback(|mut req_event| {
            let args = req_event.args();
            let body = req_event.take_body();

            Ok(RequestResponse::new_ok_200())
        })
        .set_request_type(RequestType::Ping)
        .build();
    let request_form_1 = RequestForm::new()
        .set_path("/large_data")
        .set_method(H3Method::POST)
        .set_scheme("https")
        .set_request_callback(|req_event| {
            println!("Large data received len [{:?}]", req_event.as_body().len());
            let len = req_event.as_body().len();
            info!("body extract [{:?}]", &req_event.as_body()[len - 10..len]);

            let extract = &req_event.as_body()[len - 5..len].to_vec();
            let mut s = String::new();
            for it in extract {
                s = s.add(it.to_string().as_str());
                s = s.add(", ");
            }
            let resp = format!("Hello this is the trail : {}", s);
            let response = RequestResponse::new()
                .set_status(faces_quic_server::Status::Ok(200))
                .set_body(vec![9; 2_000_000])
                .set_content_type(ContentType::Text)
                .build();
            response
        })
        .set_request_type(RequestType::Ping)
        .build();

    request_manager.add_new_request_form(request_form_0);
    request_manager.add_new_request_form(request_form_1);

    let server_config = ServerConfig::new()
        .set_address(addr)
        .set_cert_path("/home/camille/Documents/rust/faces_quic_server/cert.pem")
        .set_key_path("/home/camille/Documents/rust/faces_quic_server/key.pem")
        .set_request_manager(request_manager.build())
        .build();

    Http3Server::new(server_config).run();

    info!("[Server is listening at [{:?}] ]", addr);
    thread::park();
}

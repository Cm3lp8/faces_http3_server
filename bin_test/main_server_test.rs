use std::thread;

use faces_quic_server::{H3Method, Http3Server, RequestForm};
use faces_quic_server::{RequestManager, RequestManagerBuilder, RequestType, ServerConfig};
fn main() {
    let addr = "127.0.0.1:3000";
    let mut request_manager = RequestManager::new();

    let request_form_0 = RequestForm::new()
        .set_path("/")
        .set_method(H3Method::GET)
        .set_scheme("https")
        .set_request_callback(|mut req_event| {
            let args = req_event.args();
            let body = req_event.take_body();
            Err(())
        })
        .set_request_type(RequestType::Ping)
        .build();
    let request_form_1 = RequestForm::new()
        .set_path("/large_data")
        .set_method(H3Method::POST)
        .set_scheme("https")
        .set_request_callback(|req_event| {
            /*
                        let mut name: Option<&str> = None;
                        if let Some(args) = args {
                            args.iter().find(|item| {
                                if let Some((field, value)) = item.split_once("=") {
                                    if field == "name" {
                                        name = Some(value);
                                    }
                                    true
                                } else {
                                    false
                                }
                            });
                        }

                        let mut body: Vec<u8> = vec![];
                        if let Some(name) = name {
                            body = format!(
                                "Hello {}, I made a dream last night! However, It wasn't about Tibet",
                                name
                            )
                            .as_str()
                            .as_bytes()
                            .to_vec();
                            return Ok((body, b"text/plain".to_vec()));
                        }
                        body =
                            b"Hello unknown person, I made a dream last night! However, It wasn't about Tibet"
                                .to_vec();
            */
            Err(())
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

    println!("[Server is listening at [{:?}] ]", addr);
    thread::park();
}

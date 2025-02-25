pub use request_reponse_builder::RequestResponse;
mod request_reponse_builder {
    use quiche::h3;

    use super::*;

    pub struct RequestResponse;

    impl RequestResponse {
        pub fn get_headers(&self) -> Vec<h3::Header> {
            vec![]
        }
        pub fn get_body(&self) -> Vec<u8> {
            vec![]
        }
    }
}

/*
*
*
                    let headers = vec![
                        h3::Header::new(b":status", b"200"),
                        h3::Header::new(b"alt-svc", b"h3=\":3000\""),
                        h3::Header::new(b"content-type", &content_type),
                        h3::Header::new(b"content-lenght", body.len().to_string().as_bytes()),
                    ];
*
* */

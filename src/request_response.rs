pub use request_reponse_builder::RequestResponse;
pub use response_elements::ContentType;
pub use response_elements::Status;
mod request_reponse_builder {
    use quiche::h3::{self, Header, HeaderRef};

    use super::*;

    pub struct RequestResponse {
        status: h3::Header,
        content_length: Option<h3::Header>,
        content_type: Option<h3::Header>,
        body: Vec<u8>,
    }

    impl RequestResponse {
        pub fn new() -> ResponseBuilder {
            ResponseBuilder::new()
        }
        pub fn new_ok_200() -> RequestResponse {
            ResponseBuilder::new()
                .set_status(Status::Ok(200))
                .build()
                .unwrap()
        }
        pub fn get_headers(&self) -> Vec<h3::Header> {
            let mut headers: Vec<h3::Header> = vec![self.status.clone()];
            if let Some(content_type) = &self.content_type {
                headers.push(content_type.clone());
            }

            if let Some(content_length) = &self.content_length {
                headers.push(content_length.clone());
            }

            headers
        }
        pub fn take_body(&mut self) -> Vec<u8> {
            std::mem::replace(&mut self.body, Vec::with_capacity(1))
        }

        fn new_response(&self) -> ResponseBuilder {
            ResponseBuilder::new()
        }
    }

    pub struct ResponseBuilder {
        status: Option<Status>,
        content_length: Option<usize>,
        content_type: Option<ContentType>,
        body: Option<Vec<u8>>,
    }
    impl ResponseBuilder {
        pub fn new() -> Self {
            Self {
                status: None,
                content_length: None,
                content_type: None,
                body: None,
            }
        }
        pub fn set_status(&mut self, status: Status) -> &mut Self {
            self.status = Some(status);
            self
        }
        pub fn set_body(&mut self, body: Vec<u8>) -> &mut Self {
            self.content_length = Some(body.len());
            self.body = Some(body);
            self
        }
        pub fn set_content_type(&mut self, content_type: ContentType) -> &mut Self {
            self.content_type = Some(content_type);
            self
        }

        pub fn build(&mut self) -> Result<RequestResponse, ()> {
            if self.status.is_none() {
                return Err(());
            }

            let request_response = match self.body.as_mut() {
                Some(body) => {
                    let status = h3::Header::new(
                        b"status",
                        self.status
                            .take()
                            .unwrap()
                            .get_status_to_string()
                            .as_bytes(),
                    );

                    let body: Vec<u8> = std::mem::replace(body, vec![]);

                    let content_length = Some(h3::Header::new(
                        b"content-length",
                        body.len().to_string().as_bytes(),
                    ));

                    let content_type = if let Some(content_type) = &self.content_type {
                        let content_type =
                            h3::Header::new(b"content-type", content_type.to_string().as_bytes());
                        Some(content_type)
                    } else {
                        None
                    };
                    Ok(RequestResponse {
                        status,
                        content_type,
                        content_length,
                        body,
                    })
                }
                None => Ok(RequestResponse {
                    status: h3::Header::new(
                        b"status",
                        self.status
                            .take()
                            .unwrap()
                            .get_status_to_string()
                            .as_bytes(),
                    ),
                    content_type: None,
                    content_length: None,
                    body: vec![],
                }),
            };

            request_response
        }
    }
}

mod response_elements {
    use super::*;

    #[derive(Clone, Copy, Debug)]
    pub enum Status {
        Ok(usize),
        Err(usize),
    }

    impl Status {
        pub fn get_status_to_string(&self) -> String {
            let status = match self {
                Status::Ok(value) => value.to_string(),
                Status::Err(value) => value.to_string(),
            };

            status
        }
        pub fn get_status(self) -> Self {
            self
        }
    }

    ///
    ///
    /// Enum for Building the response. The tuple variants let the user specify a custom type
    /// definition that will be recognized on the client side.
    ///
    ///
    pub enum ContentType {
        Text,
        JSon,
        Audio(String),
        Custom(String),
    }
    impl ContentType {
        pub fn to_string(&self) -> String {
            match self {
                Self::Text => "text".to_string(),
                Self::JSon => "application/json".to_string(),
                ContentType::Audio(format) => format!("audio/{:?}", format),
                Self::Custom(format) => format!("custom/{:?}", format),
            }
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

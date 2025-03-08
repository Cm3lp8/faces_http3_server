pub use request_reponse_builder::{BodyType, RequestResponse};
pub use response_elements::ContentType;
pub use response_elements::Status;
mod request_reponse_builder {
    use std::path::{Path, PathBuf};

    use quiche::h3::{self, Header, HeaderRef, NameValue};

    use super::*;

    #[derive(Debug)]
    pub enum BodyType {
        Data {
            stream_id: u64,
            conn_id: String,
            data: Vec<u8>,
        },
        FilePath {
            stream_id: u64,
            conn_id: String,
            file_path: PathBuf,
        },
        None,
    }

    impl BodyType {
        pub fn new_data(stream_id: u64, conn_id: String, data: Vec<u8>) -> Self {
            Self::Data {
                stream_id,
                conn_id,
                data,
            }
        }
        pub fn new_file(stream_id: u64, conn_id: String, file_path: impl AsRef<PathBuf>) -> Self {
            Self::FilePath {
                stream_id,
                conn_id,
                file_path: file_path.as_ref().to_path_buf(),
            }
        }
    }
    #[derive(Debug)]
    pub struct RequestResponse {
        status: h3::Header,
        content_length: Option<h3::Header>,
        content_type: Option<h3::Header>,
        custom: Vec<h3::Header>,
        body: BodyType,
    }

    impl RequestResponse {
        pub fn new() -> ResponseBuilder {
            ResponseBuilder::new()
        }
        pub fn new_ok_200(stream_id: u64, conn_id: &str) -> RequestResponse {
            ResponseBuilder::new()
                .set_stream_id(stream_id)
                .set_conn_id(conn_id)
                .set_status(Status::Ok(200))
                .build()
                .unwrap()
        }
        pub fn header(mut self, name: &str, value: &str) -> Self {
            self.custom
                .push(h3::Header::new(name.as_bytes(), value.as_bytes()));
            self
        }
        ///
        /// Respond to client with a file from fs.
        ///
        pub fn new_200_with_file(
            stream_id: u64,
            conn_id: &str,
            path: impl AsRef<Path>,
        ) -> RequestResponse {
            ResponseBuilder::new()
                .set_stream_id(stream_id)
                .set_conn_id(conn_id)
                .set_status(Status::Ok(200))
                .set_path(path)
                .build()
                .unwrap()
        }
        pub fn from_header_200(
            stream_id: u64,
            conn_id: &str,
            headers: Vec<h3::Header>,
        ) -> RequestResponse {
            ResponseBuilder::new()
                .set_stream_id(stream_id)
                .set_conn_id(conn_id)
                .set_status(Status::Ok(200))
                .extends_headers(headers)
                .set_body(vec![1])
                .build()
                .unwrap()
        }
        ///
        /// Respond to client with in memory data
        ///
        pub fn new_200_with_data(stream_id: u64, conn_id: &str, data: Vec<u8>) -> RequestResponse {
            ResponseBuilder::new()
                .set_stream_id(stream_id)
                .set_conn_id(conn_id)
                .set_status(Status::Ok(200))
                .set_body(data)
                .build()
                .unwrap()
        }
        pub fn content_length(&self) -> String {
            if let Some(content_len) = &self.content_length {
                String::from_utf8_lossy(content_len.value()).to_string()
            } else {
                "0".to_owned()
            }
        }
        pub fn with_custom_headers(
            &self,
            custom_headers: Option<impl FnOnce() -> Vec<h3::Header>>,
        ) -> Vec<h3::Header> {
            let mut headers: Vec<h3::Header> = vec![self.status.clone()];
            if let Some(content_type) = &self.content_type {
                headers.push(content_type.clone());
            }

            if let Some(content_length) = &self.content_length {
                headers.push(content_length.clone());
            }

            if !self.custom.is_empty() {
                headers.extend(self.custom.clone());
            }

            if let Some(custom_hdr_cb) = custom_headers {
                headers.extend(custom_hdr_cb());
            }

            headers
        }
        pub fn get_headers(&self) -> Vec<h3::Header> {
            let mut headers: Vec<h3::Header> = vec![self.status.clone()];
            if let Some(content_type) = &self.content_type {
                headers.push(content_type.clone());
            }
            if !self.custom.is_empty() {
                headers.extend(self.custom.clone());
            }

            if let Some(content_length) = &self.content_length {
                headers.push(content_length.clone());
            }

            headers
        }

        pub fn take_body(&mut self) -> BodyType {
            std::mem::replace(&mut self.body, BodyType::None)
        }

        fn new_response(&self) -> ResponseBuilder {
            ResponseBuilder::new()
        }
    }

    pub struct ResponseBuilder {
        stream_id: Option<u64>,
        conn_id: Option<String>,
        status: Option<Status>,
        content_length: Option<usize>,
        content_type: Option<ContentType>,
        custom: Vec<h3::Header>,
        body: Option<Vec<u8>>,
        path: Option<PathBuf>,
        body_type: Option<BodyType>,
    }
    impl ResponseBuilder {
        pub fn new() -> Self {
            Self {
                stream_id: None,
                conn_id: None,
                status: None,
                content_length: None,
                content_type: None,
                custom: vec![],
                body: None,
                path: None,
                body_type: None,
            }
        }
        pub fn extends_headers(&mut self, headers: Vec<h3::Header>) -> &mut Self {
            let (status, others): (Vec<Header>, Vec<Header>) = headers
                .clone()
                .into_iter()
                .partition(|hdr| hdr.name() == b"status");
            if status.len() > 0 {
                self.status = Some(Status::from_bytes(status[0].value()));
                self.custom.extend(others);
            } else {
                self.custom.extend(headers);
            }

            self
        }
        pub fn set_stream_id(&mut self, stream_id: u64) -> &mut Self {
            self.stream_id = Some(stream_id);
            self
        }
        pub fn set_conn_id(&mut self, conn_id: &str) -> &mut Self {
            self.conn_id = Some(conn_id.to_owned());
            self
        }
        pub fn set_status(&mut self, status: Status) -> &mut Self {
            self.status = Some(status);
            self
        }

        pub fn set_path(&mut self, path: impl AsRef<Path>) -> &mut Self {
            self.path = Some(path.as_ref().to_path_buf());
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
                    if let Some(stream_id) = self.stream_id {
                        if let Some(conn_id) = self.conn_id.take() {
                            Ok(RequestResponse {
                                status,
                                content_type,
                                content_length,
                                custom: std::mem::replace(&mut self.custom, vec![]),
                                body: BodyType::new_data(stream_id, conn_id, body),
                            })
                        } else {
                            Err(())
                        }
                    } else {
                        Err(())
                    }
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
                    custom: vec![],
                    body: BodyType::None,
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
        pub fn from_bytes(bytes: &[u8]) -> Self {
            let code = String::from_utf8_lossy(bytes).parse::<usize>().unwrap();

            if code == 200 {
                Self::Ok(code)
            } else {
                Self::Err(code)
            }
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
        pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
            match bytes {
                b"text" => Some(Self::Text),
                &_ => {
                    let val = String::from_utf8_lossy(bytes).to_string();

                    if let Some((pre, suf)) = val.split_once("/") {
                        if pre == "custom" {
                            return Some(Self::Custom(suf.to_string()));
                        }
                        return None;
                    };
                    None
                }
            };
            None
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

pub use event_response_channel::{
    EventResponseChannel, EventResponseWaiter, ResponseBuilderSender,
};
pub use request_event::{DataEvent, EventType, FinishedEvent, HeaderEvent, RouteEvent};
mod event_response_channel {
    use std::path::Path;

    use crossbeam_channel::Sender;
    use mio::event;
    use quiche::h3;

    use crate::{RequestResponse, RouteEvent, RouteResponse};

    pub struct EventResponseChannel;
    impl EventResponseChannel {
        pub fn new(event: &RouteEvent) -> (ResponseBuilderSender, EventResponseWaiter) {
            let channel = crossbeam_channel::bounded(1);

            (
                ResponseBuilderSender {
                    bytes_written: event.bytes_written(),
                    stream_id: event.stream_id() as u64,
                    scid: event.scid().to_vec(),
                    conn_id: event.conn_id().to_owned(),
                    sender: channel.0,
                },
                EventResponseWaiter {
                    receiver: channel.1,
                },
            )
        }
    }
    pub struct ResponseBuilderSender {
        bytes_written: usize,
        stream_id: u64,
        conn_id: String,
        scid: Vec<u8>,
        sender: crossbeam_channel::Sender<RequestResponse>,
    }
    impl ResponseBuilderSender {
        pub fn send_ok_200(&self) -> Result<(), crossbeam_channel::SendError<RequestResponse>> {
            let stream_id = self.stream_id;
            let conn_id = &self.conn_id;
            let scid = &self.scid;
            self.sender.send(
                RequestResponse::new_ok_200(stream_id, scid, conn_id)
                    .header("x-received-data", self.bytes_written.to_string().as_str()),
            )
        }
        pub fn build_response(
            &self,
            route_response: RouteResponse,
        ) -> Result<(), crossbeam_channel::SendError<RequestResponse>> {
            match route_response {
                RouteResponse::OK200 => self.send_ok_200(),
                RouteResponse::OK200_DATA(data) => self.send_ok_200_with_data(data),
                RouteResponse::OK200_FILE(file_path) => self.send_ok_200_with_file(file_path),
                RouteResponse::OK200_JSON(data) => self.send_ok_200_with_json_data(data),
                RouteResponse::ERROR409(data) => self.send_error_409(data),
                RouteResponse::ERROR503(data) => self.send_error_503(data),
                RouteResponse::ERROR401(data) => self.send_error_401(data),
            }
        }
        pub fn send_ok_200_with_data(
            &self,
            data: Vec<u8>,
        ) -> Result<(), crossbeam_channel::SendError<RequestResponse>> {
            let stream_id = self.stream_id;
            let conn_id = &self.conn_id;
            let scid = &self.scid;
            self.sender.send(
                RequestResponse::new_200_with_data(stream_id, scid, conn_id, data)
                    .header("x-received-data", self.bytes_written.to_string().as_str()),
            )
        }
        pub fn send_ok_200_with_json_data(
            &self,
            data: Vec<u8>,
        ) -> Result<(), crossbeam_channel::SendError<RequestResponse>> {
            let stream_id = self.stream_id;
            let conn_id = &self.conn_id;
            let scid = &self.scid;
            self.sender.send(
                RequestResponse::new_200_with_json(stream_id, scid, conn_id, data)
                    .header("x-received-data", self.bytes_written.to_string().as_str()),
            )
        }
        pub fn send_ok_200_with_file(
            &self,
            path: impl AsRef<Path>,
        ) -> Result<(), crossbeam_channel::SendError<RequestResponse>> {
            let stream_id = self.stream_id;
            let conn_id = &self.conn_id;
            let scid = &self.scid;
            self.sender.send(
                RequestResponse::new_200_with_file(stream_id, scid, conn_id, path)
                    .header("x-received-data", self.bytes_written.to_string().as_str()),
            )
        }
        pub fn send_error_401(
            &self,
            data: Vec<u8>,
        ) -> Result<(), crossbeam_channel::SendError<RequestResponse>> {
            let stream_id = self.stream_id;
            let conn_id = &self.conn_id;
            let scid = &self.scid;
            self.sender.send(RequestResponse::new_401_with_data(
                stream_id, scid, conn_id, data,
            ))
        }
        pub fn send_error_409(
            &self,
            data: Vec<u8>,
        ) -> Result<(), crossbeam_channel::SendError<RequestResponse>> {
            let stream_id = self.stream_id;
            let conn_id = &self.conn_id;
            let scid = &self.scid;
            self.sender.send(RequestResponse::new_409_with_data(
                stream_id, scid, conn_id, data,
            ))
        }
        pub fn send_error_503(
            &self,
            data: Vec<u8>,
        ) -> Result<(), crossbeam_channel::SendError<RequestResponse>> {
            let stream_id = self.stream_id;
            let conn_id = &self.conn_id;
            let scid = &self.scid;
            self.sender.send(RequestResponse::new_503_with_data(
                stream_id, scid, conn_id, data,
            ))
        }
    }
    pub struct EventResponseWaiter {
        receiver: crossbeam_channel::Receiver<RequestResponse>,
    }

    impl EventResponseWaiter {
        pub fn wait(&self) -> Result<RequestResponse, crossbeam_channel::RecvError> {
            self.receiver.recv()
        }
    }
}
mod request_event {
    use std::path::PathBuf;

    use mio::event;
    use quiche::h3;

    use crate::{route_handler::ReqArgs, H3Method};

    use super::*;

    pub enum EventType {
        OnFinished,
        OnData,
        OnHeader,
    }

    #[derive(Debug)]
    pub struct HeaderEvent {
        stream_id: u64,
        conn_id: String,
        scid: Vec<u8>,
        path: String,
        headers: Vec<h3::Header>,
        method: H3Method,
        args: Option<ReqArgs>,
    }
    impl HeaderEvent {
        pub fn new(
            stream_id: u64,
            conn_id: &str,
            scid: &[u8],
            path: &str,
            method: H3Method,
            headers: Vec<h3::Header>,
            args: Option<ReqArgs>,
        ) -> HeaderEvent {
            HeaderEvent {
                stream_id,
                conn_id: conn_id.to_owned(),
                scid: scid.to_vec(),
                path: path.to_owned(),
                headers,
                method,
                args,
            }
        }
        pub fn path(&self) -> &str {
            self.path.as_str()
        }
        pub fn method(&self) -> H3Method {
            self.method
        }
        pub fn args(&self) -> Option<&ReqArgs> {
            self.args.as_ref()
        }
    }

    #[derive(Debug)]
    pub struct FinishedEvent {
        stream_id: u64,
        conn_id: String,
        scid: Vec<u8>,
        path: String,
        headers: Vec<h3::Header>,
        method: H3Method,
        args: Option<ReqArgs>,
        file_path: Option<PathBuf>,
        bytes_written: usize,
        body: Vec<u8>,
        is_end: bool,
    }

    impl FinishedEvent {
        pub fn new(
            stream_id: u64,
            conn_id: &str,
            scid: &[u8],
            path: &str,
            method: H3Method,
            headers: Vec<h3::Header>,
            args: Option<ReqArgs>,
            file_path: Option<PathBuf>,
            bytes_written: usize,
            body: Option<Vec<u8>>,
            is_end: bool,
        ) -> Self {
            Self {
                stream_id,
                conn_id: conn_id.to_owned(),
                scid: scid.to_vec(),
                path: path.to_owned(),
                method,
                headers,
                args,
                file_path,
                bytes_written,
                body: body.unwrap_or(vec![]),
                is_end,
            }
        }
        pub fn bytes_written(&self) -> usize {
            self.bytes_written
        }

        pub fn scid(&self) -> &[u8] {
            &self.scid
        }
        pub fn stream_id(&self) -> u64 {
            self.stream_id
        }
        pub fn get_file_path(&self) -> Option<&PathBuf> {
            self.file_path.as_ref()
        }
        pub fn path(&self) -> &str {
            self.path.as_str()
        }
        pub fn method(&self) -> H3Method {
            self.method
        }
        pub fn headers(&self) -> &Vec<h3::Header> {
            &self.headers
        }
        pub fn is_end(&self) -> bool {
            self.is_end
        }
        pub fn body_size(&self) -> usize {
            self.body.len()
        }
        pub fn extend_body_data(&mut self, data: &[u8]) {
            self.body.extend_from_slice(data);
        }
        pub fn args(&self) -> Option<&ReqArgs> {
            self.args.as_ref()
        }
        pub fn to_args(&self) -> Option<ReqArgs> {
            self.args.clone()
        }
        pub fn take_body(&mut self) -> Vec<u8> {
            std::mem::replace(&mut self.body, Vec::with_capacity(1))
        }
        pub fn as_body(&self) -> &Vec<u8> {
            &self.body
        }
    }

    #[derive(Debug)]
    pub struct DataEvent {
        stream_id: u64,
        conn_id: String,
        scid: Vec<u8>,
        packet: Vec<u8>,
        is_end: bool,
    }
    impl DataEvent {
        pub fn new(
            stream_id: u64,
            scid: &[u8],
            conn_id: &str,
            packet: Vec<u8>,
            is_end: bool,
        ) -> Self {
            Self {
                stream_id,
                conn_id: conn_id.to_owned(),
                scid: scid.to_vec(),
                packet,
                is_end,
            }
        }
        pub fn packet_len(&self) -> usize {
            self.packet.len()
        }
    }
    #[derive(Debug)]
    pub enum RouteEvent {
        OnData(DataEvent),
        OnFinished(FinishedEvent),
        OnHeader(HeaderEvent),
    }
    impl RouteEvent {
        pub fn new_data(data_event: DataEvent) -> RouteEvent {
            Self::OnData(data_event)
        }
        pub fn new_finished(finised_event: FinishedEvent) -> RouteEvent {
            Self::OnFinished(finised_event)
        }
        pub fn new_header(header_event: HeaderEvent) -> RouteEvent {
            Self::OnHeader(header_event)
        }
        pub fn headers(&self) -> Option<Vec<h3::Header>> {
            match self {
                Self::OnData(_event) => None,
                Self::OnFinished(event) => Some(event.headers.clone()),
                Self::OnHeader(event) => Some(event.headers.clone()),
            }
        }
        pub fn scid(&self) -> &[u8] {
            match self {
                Self::OnData(event) => &event.scid,
                Self::OnFinished(event) => &event.scid,
                Self::OnHeader(event) => &event.scid,
            }
        }
        pub fn get_file_path(&self) -> Option<&PathBuf> {
            match self {
                Self::OnFinished(event) => event.get_file_path(),
                _ => None,
            }
        }
        pub fn as_body(&self) -> Option<&Vec<u8>> {
            match self {
                Self::OnFinished(event) => Some(event.as_body()),
                _ => None,
            }
        }
        pub fn body_size(&self) -> usize {
            match self {
                Self::OnData(event) => event.packet_len(),
                Self::OnFinished(event) => event.body_size(),
                Self::OnHeader(_) => 0,
            }
        }
        pub fn stream_id(&self) -> u64 {
            match self {
                Self::OnData(event) => event.stream_id,
                Self::OnFinished(event) => event.stream_id,
                Self::OnHeader(event) => event.stream_id,
            }
        }
        pub fn conn_id(&self) -> &str {
            match self {
                Self::OnData(event) => event.conn_id.as_str(),
                Self::OnFinished(event) => event.conn_id.as_str(),
                Self::OnHeader(event) => event.conn_id.as_str(),
            }
        }
        pub fn bytes_written(&self) -> usize {
            match self {
                Self::OnData(event) => event.packet_len(),
                Self::OnFinished(event) => event.bytes_written(),
                Self::OnHeader(_) => 0,
            }
        }
        pub fn is_end(&self) -> bool {
            match self {
                Self::OnData(event) => event.is_end,
                Self::OnFinished(event) => event.is_end,
                Self::OnHeader(_) => true,
            }
        }
        pub fn method(&self) -> H3Method {
            match self {
                Self::OnData(_event) => H3Method::POST,
                Self::OnFinished(event) => event.method(),
                Self::OnHeader(event) => event.method(),
            }
        }
        pub fn path(&self) -> &str {
            match self {
                Self::OnData(_event) => "",
                Self::OnFinished(event) => event.path(),
                Self::OnHeader(event) => event.path(),
            }
        }
    }
}

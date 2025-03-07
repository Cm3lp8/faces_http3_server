pub use event_response_channel::{
    EventResponseChannel, EventResponseWaiter, ResponseBuilderSender,
};
pub use request_event::{DataEvent, EventType, FinishedEvent, HeaderEvent, RouteEvent};
mod event_response_channel {
    use crossbeam_channel::Sender;

    use crate::RequestResponse;

    pub struct EventResponseChannel;
    impl EventResponseChannel {
        pub fn new() -> (ResponseBuilderSender, EventResponseWaiter) {
            let channel = crossbeam_channel::bounded(1);

            (
                ResponseBuilderSender { sender: channel.0 },
                EventResponseWaiter {
                    receiver: channel.1,
                },
            )
        }
    }
    pub struct ResponseBuilderSender {
        sender: crossbeam_channel::Sender<RequestResponse>,
    }
    impl ResponseBuilderSender {
        pub fn send_ok_200(&self) -> Result<(), crossbeam_channel::SendError<RequestResponse>> {
            self.sender
                .send(RequestResponse::new_200_with_data(vec![0; 2]))
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
        path: String,
        headers: Vec<h3::Header>,
        method: H3Method,
        args: Option<Vec<ReqArgs>>,
    }
    impl HeaderEvent {
        pub fn new(
            path: &str,
            method: H3Method,
            headers: Vec<h3::Header>,
            args: Option<Vec<ReqArgs>>,
        ) -> HeaderEvent {
            HeaderEvent {
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
        pub fn args(&self) -> Option<&Vec<ReqArgs>> {
            self.args.as_ref()
        }
    }

    #[derive(Debug)]
    pub struct FinishedEvent {
        path: String,
        headers: Vec<h3::Header>,
        method: H3Method,
        args: Option<Vec<ReqArgs>>,
        file_path: Option<PathBuf>,
        bytes_written: usize,
        body: Vec<u8>,
        is_end: bool,
    }

    impl FinishedEvent {
        pub fn new(
            path: &str,
            method: H3Method,
            headers: Vec<h3::Header>,
            args: Option<Vec<ReqArgs>>,
            file_path: Option<PathBuf>,
            bytes_written: usize,
            body: Option<Vec<u8>>,
            is_end: bool,
        ) -> Self {
            Self {
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
        pub fn args(&self) -> Option<&Vec<ReqArgs>> {
            self.args.as_ref()
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
        packet: Vec<u8>,
        is_end: bool,
    }
    impl DataEvent {
        pub fn new(packet: Vec<u8>, is_end: bool) -> Self {
            Self { packet, is_end }
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

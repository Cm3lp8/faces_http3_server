pub use req_temp_table::RequestsTable;
pub use request_argument_parser::ReqArgs;
mod reception_status {
    use super::*;

    pub struct ReceptionStatus {
        percentage_written: Option<f32>,
        written: Option<usize>,
        total: Option<usize>,
    }
    impl ReceptionStatus {
        pub fn new(
            percentage_written: Option<f32>,
            written: Option<usize>,
            total: Option<usize>,
        ) -> Self {
            ReceptionStatus {
                percentage_written,
                written,
                total,
            }
        }
        pub fn has_something_to_update(&self) -> bool {
            if let Some(p_w) = self.percentage_written {
                if p_w < 0.98 {
                    true
                } else {
                    false
                }
            } else {
                false
            }
        }
        pub fn get_percentage_written_to_string(&self) -> Option<String> {
            if let Some(perc) = self.percentage_written {
                Some(perc.to_string())
            } else {
                None
            }
        }
        pub fn is_end(&self) -> bool {
            if let Some(progress) = self.percentage_written {
                progress == 1.0
            } else {
                false
            }
        }
        ///
        /// return the progress body
        ///
        /// # Example
        /// let status = ReceptionStatus::new(5);
        ///
        /// let body = status.body();
        ///
        /// assert!(&body == b"progress=5");
        ///
        pub fn body(&self) -> Option<Vec<u8>> {
            if let Some(progress) = self.percentage_written {
                let written = self.written.unwrap_or(0);
                let total = self.total.unwrap_or(0);
                Some(
                    format!(
                        "s??%progress={}%&written={}%&total={}",
                        progress, written, total
                    )
                    .as_bytes()
                    .to_vec(),
                )
            } else {
                None
            }
        }
    }
}
mod req_temp_table {
    use dashmap::DashMap;
    use quiche::h3::{self, NameValue};
    use request_argument_parser::ReqArgs;
    use std::{
        collections::HashMap,
        fmt::{Debug, Formatter},
        fs::{File, Permissions},
        io::{BufReader, BufWriter, Write},
        path::PathBuf,
        sync::{Arc, Mutex},
        time::{Duration, SystemTimeError},
    };
    use uuid::Uuid;

    use crate::{
        file_writer::{FileWritable, FileWriterChannel, FileWriterHandle, WritableItem},
        route_events::{self, DataEvent, EventType, FinishedEvent, HeaderEvent, RouteEvent},
        route_manager::DataManagement,
        server_config, BodyStorage, H3Method, RouteEventListener, ServerConfig,
    };

    use self::reception_status::ReceptionStatus;

    use super::*;

    type ReqId = (String, u64);
    pub struct RequestsTable {
        table: Arc<DashMap<ReqId, PartialReq>>,
    }

    impl RequestsTable {
        pub fn new() -> Self {
            Self {
                table: Arc::new(DashMap::new()),
            }
        }
        pub fn set_intermediate_headers_send(&self, stream_id: u64, conn_id: String) {
            if let Some(mut entry) = self.table.get_mut(&(conn_id, stream_id)) {
                entry.progress_header_sent = true;
            }
        }
        pub fn complete_request_entry_in_table(
            &self,
            stream_id: u64,
            conn_id: &str,
            method: H3Method,
            path: &str,
            headers: &[h3::Header],
            content_length: Option<usize>,
            data_management_type: Option<DataManagement>,
            event_subscriber: Option<Arc<dyn RouteEventListener + 'static + Send + Sync>>,
            storage_path: Option<PathBuf>,
            file_open: Option<FileWriterHandle<File>>,
        ) {
            self.table
                .entry((conn_id.to_string(), stream_id))
                .and_modify(|entry| {
                    entry.method = Some(method);
                    entry.path = Some(path.to_string());
                    entry.event_subscriber = event_subscriber;
                    entry.data_management_type = data_management_type;
                    entry.storage_path = storage_path;
                    entry.file_opened = file_open;
                    entry.content_length = content_length;
                    entry.headers = Some(headers.to_vec());
                });
        }
        pub fn is_entry_partial_reponse_set(&self, stream_id: u64, conn_id: &str) -> bool {
            if let Some(_entry) = self.table.get(&(conn_id.to_string(), stream_id)) {
                true
            } else {
                false
            }
        }
        pub fn get_path_and_method_and_content_length(
            &self,
            stream_id: u64,
            conn_id: &str,
        ) -> Option<(String, H3Method, Vec<h3::Header>, Option<usize>)> {
            let mut path_and_method: Option<(String, H3Method, Vec<h3::Header>, Option<usize>)> =
                None;
            if let Some(entry) = self.table.get_mut(&(conn_id.to_string(), stream_id)) {
                if entry.path.is_none() || entry.headers.is_none() || entry.method.is_none() {
                    return None;
                }
                if let Some(headers) = &entry.headers {
                    path_and_method = Some((
                        entry.path.as_ref().unwrap().clone(),
                        entry.method.unwrap(),
                        headers.to_vec(),
                        entry.content_length(),
                    ));
                }
            };
            path_and_method
        }
        pub fn get_reception_status_infos(
            &self,
            stream_id: u64,
            conn_id: String,
        ) -> Option<(ReceptionStatus, bool)> {
            if let Some(mut entry) = self.table.get_mut(&(conn_id, stream_id)) {
                let headers_send = entry.progress_header_sent;
                if let Some(content_length) = entry.content_length {
                    let written = entry.written();
                    let percentage_written = written as f32 / content_length as f32;

                    if let Some(prec_value) = entry.precedent_percentage_written {
                        if (prec_value * 100.0) as usize == (percentage_written * 100.0) as usize {
                            entry.precedent_percentage_written = Some(percentage_written);
                            return Some((ReceptionStatus::new(None, None, None), headers_send));
                        }
                        entry.precedent_percentage_written = Some(percentage_written);
                        return Some((
                            ReceptionStatus::new(
                                Some(percentage_written),
                                Some(written),
                                Some(content_length),
                            ),
                            headers_send,
                        ));
                    }
                    entry.precedent_percentage_written = Some(percentage_written);
                    return Some((
                        ReceptionStatus::new(
                            Some(percentage_written),
                            Some(written),
                            Some(content_length),
                        ),
                        headers_send,
                    ));
                }
                return None;
            }
            None
        }
        /// Once all the data is received by the server, this takes infos from it, builds the request event  that trigger a reponse for the client.
        pub fn build_route_event(
            &self,
            conn_id: &str,
            scid: &[u8],
            stream_id: u64,
            event_type: EventType,
        ) -> Result<RouteEvent, ()> {
            let mut can_clean = false;
            let res = if let Some(mut partial_req) =
                self.table.get_mut(&(conn_id.to_string(), stream_id))
            {
                let request_event =
                    partial_req.to_route_event(stream_id, scid, conn_id, event_type);

                can_clean = true;
                if let Some(req_event) = request_event {
                    Ok(req_event)
                } else {
                    Err(())
                }
            } else {
                log::error!("was not able to build route event");
                Err(())
            };

            if can_clean {
                self.table.remove(&(conn_id.to_string(), stream_id));
            }

            res
        }

        pub fn write_body_packet(
            &self,
            conn_id: String,
            scid: &[u8],
            stream_id: u64,
            packet: &[u8],
            is_end: bool,
        ) -> Result<usize, ()> {
            let mut total_written: Result<usize, ()> = Err(());
            if let Some(mut entry) = self.table.get_mut(&(conn_id.clone(), stream_id)) {
                if let Some(data_mngmt) = entry.data_management_type() {
                    match data_mngmt {
                        DataManagement::Stream => {
                            if let Some(suscriber) = entry.event_subscriber() {
                                suscriber.on_data(RouteEvent::new_data(DataEvent::new(
                                    stream_id,
                                    scid,
                                    conn_id.as_str(),
                                    packet.to_vec(),
                                    is_end,
                                )));
                            }
                        }
                        DataManagement::Storage(body_storage) => match body_storage {
                            BodyStorage::InMemory => {
                                entry.extend_data(packet, is_end);
                                total_written = Ok(entry.written());
                            }
                            BodyStorage::File => {
                                if let Some(suscriber) = entry.event_subscriber() {
                                    suscriber.on_data(RouteEvent::new_data(DataEvent::new(
                                        stream_id,
                                        scid,
                                        conn_id.as_str(),
                                        packet.to_vec(),
                                        is_end,
                                    )));
                                }
                                entry.write_file(packet, 0, is_end);
                                total_written = Ok(entry.written());
                            }
                        },
                    }
                }
            }
            total_written
        }
        pub fn add_partial_request_before_header_treatment(
            &self,
            server_config: &Arc<ServerConfig>,
            conn_id: String,
            stream_id: u64,
            data_management_type: Option<DataManagement>,
            event_subscriber: Option<Arc<dyn RouteEventListener + 'static + Send + Sync>>,
            is_end: bool,
            file_writer_channel: FileWriterChannel,
        ) {
            if let Some(_entry) = self.table.get(&(conn_id.to_string(), stream_id)) {
                return;
            }
            let mut file_opened: Option<FileWriterHandle<File>> = None;
            let storage_path = if let Some(data_management_type) = data_management_type.as_ref() {
                if let Some(body_storage) = data_management_type.is_body_storage() {
                    if let BodyStorage::File = body_storage {
                        let mut path = server_config.get_storage_path();

                        let mut extension: Option<String> = None;

                        let uuid = Uuid::new_v4();
                        let mut uuid = uuid.to_string();

                        if let Some(ext) = extension {
                            uuid = format!("{}{}", uuid, ext);
                        }

                        path.push(uuid);

                        if let Ok(file) = File::create(path.clone()) {
                            file_opened = Some(FileWriterHandle::new(file));
                        } else {
                            error!("Failed creating [{:?}] file", path);
                        }

                        Some(path)
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };

            let partial_request = PartialReq::new(
                conn_id.clone(),
                stream_id,
                None, //method
                data_management_type,
                event_subscriber,
                storage_path,
                file_opened,
                file_writer_channel,
                None, //header
                None, //path
                None, //content_length
                is_end,
            );
            self.table.insert((conn_id, stream_id), partial_request);
        }
        /// Keep track of a client request based on unique connexion_id and stream_id
        /// If the entry already exists, does nothing.
        pub fn add_partial_request(
            &self,
            server_config: &Arc<ServerConfig>,
            conn_id: String,
            stream_id: u64,
            method: H3Method,
            data_management_type: Option<DataManagement>,
            event_subscriber: Option<Arc<dyn RouteEventListener + 'static + Send + Sync>>,
            headers: &[h3::Header],
            path: &str,
            content_length: Option<usize>,
            is_end: bool,
            file_writer_channel: FileWriterChannel,
        ) {
            if let Some(_entry) = self.table.get(&(conn_id.to_string(), stream_id)) {
                return;
            }
            let mut file_opened: Option<FileWriterHandle<File>> = None;
            let storage_path = if let Some(data_management_type) = data_management_type.as_ref() {
                if let Some(body_storage) = data_management_type.is_body_storage() {
                    match body_storage {
                        BodyStorage::File => {
                            let mut path = server_config.get_storage_path();

                            let mut extension: Option<String> = None;

                            if let Some(found_content_type) =
                                headers.iter().find(|hdr| hdr.name() == b"content-type")
                            {
                                match found_content_type.value() {
                                    b"text/plain" => {
                                        extension = Some(String::from(".txt"));
                                    }
                                    _ => {}
                                }
                            };

                            let uuid = Uuid::new_v4();
                            let mut uuid = uuid.to_string();

                            if let Some(ext) = extension {
                                uuid = format!("{}{}", uuid, ext);
                            }

                            path.push(uuid);

                            if let Ok(file) = File::create(path.clone()) {
                                file_opened = Some(FileWriterHandle::new(file));
                            } else {
                                error!("Failed creating [{:?}] file", path);
                            }

                            Some(path)
                        }
                        BodyStorage::InMemory => None,
                    }
                } else {
                    None
                }
            } else {
                None
            };

            let partial_request = PartialReq::new(
                conn_id.clone(),
                stream_id,
                Some(method),
                data_management_type,
                event_subscriber,
                storage_path,
                file_opened,
                file_writer_channel,
                Some(headers),
                Some(path),
                content_length,
                is_end,
            );
            self.table.insert((conn_id, stream_id), partial_request);
        }
    }
    struct PartialReq {
        conn_id: String,
        stream_id: u64,
        headers: Option<Vec<h3::Header>>,
        method: Option<H3Method>,
        data_management_type: Option<DataManagement>,
        event_subscriber: Option<Arc<dyn RouteEventListener + 'static + Send + Sync>>,
        storage_path: Option<PathBuf>,
        file_opened: Option<FileWriterHandle<File>>,
        path: Option<String>,
        args: Option<ReqArgs>,
        body_written_size: usize,
        content_length: Option<usize>,
        precedent_percentage_written: Option<f32>,
        file_writer_channel: FileWriterChannel,
        progress_header_sent: bool,
        body: Vec<u8>,
        is_end: bool,
    }
    impl Debug for PartialReq {
        fn fmt(&self, f: &mut Formatter) -> Result<(), std::fmt::Error> {
            write!(
                f,
                "partial req entry [{:?}] [{:#?}]",
                self.conn_id, self.headers
            )
        }
    }
    impl PartialReq {
        pub fn new(
            conn_id: String,
            stream_id: u64,
            method: Option<H3Method>,
            data_management_type: Option<DataManagement>,
            event_subscriber: Option<Arc<dyn RouteEventListener + Send + 'static + Sync>>,
            storage_path: Option<PathBuf>,
            file_opened: Option<FileWriterHandle<File>>,
            file_writer_channel: FileWriterChannel,
            headers: Option<&[h3::Header]>,
            path: Option<&str>,
            content_length: Option<usize>,
            is_end: bool,
        ) -> Self {
            if let Some(path) = path {
                let (path, args) = ReqArgs::parse_args(path);
                Self {
                    conn_id,
                    stream_id,
                    headers: Some(headers.unwrap().to_vec()),
                    method,
                    data_management_type,
                    event_subscriber,
                    storage_path,
                    file_opened,
                    path: Some(path),
                    args,
                    body_written_size: 0,
                    content_length,
                    precedent_percentage_written: None,
                    progress_header_sent: false,
                    body: vec![],
                    file_writer_channel,
                    is_end,
                }
            } else {
                Self {
                    conn_id,
                    stream_id,
                    headers: None,
                    method: None,
                    data_management_type,
                    event_subscriber,
                    storage_path,
                    file_opened,
                    path: None,
                    args: None,
                    body_written_size: 0,
                    content_length,
                    precedent_percentage_written: None,
                    progress_header_sent: false,
                    body: vec![],
                    file_writer_channel,
                    is_end,
                }
            }
        }
        pub fn content_length(&self) -> Option<usize> {
            self.content_length.clone()
        }
        pub fn write_file(&mut self, packet: &[u8], packet_id: usize, is_end: bool) {
            self.is_end = is_end;

            if let Some(file_writer) = &self.file_opened {
                if let Err(_) = self.file_writer_channel.send(WritableItem::new(
                    packet.to_vec(),
                    packet_id,
                    file_writer.clone(),
                    //self.body_written_size
                    //self.content_length
                )) {
                    error!("failed to send writable item to file worker thread");
                };
                self.body_written_size += packet.len();
                println!("send to write [{:?}]", self.body_written_size);
            }
        }
        pub fn extend_data(&mut self, packet: &[u8], is_end: bool) {
            self.is_end = is_end;
            self.body_written_size += packet.len();
            self.body.extend_from_slice(packet);
        }
        pub fn written(&self) -> usize {
            self.body_written_size
        }
        ///
        ///Drop the BufWriter<file> to close it and return the file path.
        pub fn path_storage(&mut self) -> Option<PathBuf> {
            self.storage_path.clone()
        }
        pub fn data_management_type(&self) -> Option<DataManagement> {
            self.data_management_type
        }
        pub fn event_subscriber(
            &mut self,
        ) -> Option<&Arc<dyn RouteEventListener + 'static + Send + Sync>> {
            self.event_subscriber.as_ref()
        }
        pub fn to_route_event(
            &mut self,
            stream_id: u64,
            scid: &[u8],
            conn_id: &str,
            event_type: EventType,
        ) -> Option<RouteEvent> {
            if self.method.is_none() || self.headers.is_none() || self.path.is_none() {
                return None;
            }
            let file_path = self.path_storage();
            if let Some(headers) = self.headers.as_ref() {
                match event_type {
                    EventType::OnHeader => Some(RouteEvent::new_header(HeaderEvent::new(
                        stream_id,
                        conn_id,
                        scid,
                        self.path.as_ref().unwrap().as_str(),
                        self.method.unwrap(),
                        headers.clone(),
                        self.args.take(),
                    ))),
                    EventType::OnFinished => Some(RouteEvent::new_finished(FinishedEvent::new(
                        stream_id,
                        conn_id,
                        scid,
                        self.path.as_ref().unwrap().as_str(),
                        self.method.unwrap(),
                        headers.clone(),
                        self.args.take(),
                        file_path,
                        self.file_opened.take(),
                        self.body_written_size,
                        Some(std::mem::replace(&mut self.body, vec![])),
                        self.is_end,
                    ))),
                    _ => None,
                }
            } else {
                None
            }
        }
    }
}

mod request_argument_parser {
    use std::collections::HashMap;

    type Parameter = String;
    type Value = String;

    use super::*;
    #[derive(Debug)]
    /// Table (k=Parameter v=Value )* of the query string if any
    #[derive(Clone)]
    pub struct ReqArgs {
        map: HashMap<Parameter, Value>,
    }

    impl ReqArgs {
        pub fn new() -> Self {
            Self {
                map: HashMap::new(),
            }
        }
        pub fn build_query_string(query_string: &str) -> ReqArgs {
            let mut req_args = ReqArgs::new();

            for s in query_string.split("&") {
                if let Some((param, value)) = s.split_once("=") {
                    req_args.map.insert(param.to_string(), value.to_string());
                }
            }
            req_args
        }
        // Parsing the request path string and split it if query-string is present = (path, vec<ReqArgs>).
        pub fn parse_args(path: &str) -> (String, Option<ReqArgs>) {
            match path.split_once("?") {
                Some((path_split, query_string)) => {
                    let req_args = ReqArgs::build_query_string(query_string);
                    (path_split.to_string(), Some(req_args))
                }
                None => (path.to_string(), None),
            }
        }

        pub fn get(&self, name: &str) -> Option<&str> {
            if let Some(args) = self.map.get(name) {
                Some(args.as_str())
            } else {
                None
            }
        }
    }
}
mod test_request_argument_parser {
    use self::request_argument_parser::ReqArgs;

    use super::*;

    #[test]
    fn test_request_path_parsing() {
        let path = "/path?name=charles&role=golfer&age=30";

        let (path, args) = ReqArgs::parse_args(path);

        assert!(path.as_str() == "/path");
        assert!(args.is_some());

        let mut name: Option<String> = None;
        let mut role: Option<String> = None;
        let mut age: Option<String> = None;

        if let Some(nm) = args.as_ref().unwrap().get("name") {
            name = Some(nm.to_string());
        }
        if let Some(rl) = args.as_ref().unwrap().get("role") {
            role = Some(rl.to_string());
        }
        if let Some(ag) = args.as_ref().unwrap().get("age") {
            role = Some(ag.to_string());
        }

        assert!(name.is_some());
        assert!(age.is_some());
        assert!(role.is_some());

        let name = name.unwrap();
        let role = role.unwrap();
        let age = age.unwrap();

        assert!(name.as_str() == "charles");
        assert!(role.as_str() == "golfer");
        assert!(age.as_str() == "30");
    }
}

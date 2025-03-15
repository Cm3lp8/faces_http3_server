use crate::request_response::{BodyRequest, ResponseQueue};

pub use self::quiche_implementation::{
    send_body, send_header, send_more_header, send_response, send_response_when_finished,
};
pub use fifo_body::{BodyReqQueue, QueueTrackableItem};
use mio::Token;
use quiche::h3::{self, NameValue};
use quiche::Connection;
use ring::rand::*;
use std::borrow::BorrowMut;
use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::hash_map::ValuesMut;
use std::collections::vec_deque::Iter;
use std::collections::{HashMap, VecDeque};
use std::net;
use std::rc::Rc;
mod fifo_body;

pub use Client as QClient;
const MAX_DATAGRAM_SIZE: usize = 4096;
const WAKER_TOKEN: Token = Token(1);
struct PartialResponse {
    headers: Option<Vec<quiche::h3::Header>>,
    body: Vec<u8>,
    written: usize,
}
pub struct Client {
    conn: quiche::Connection,
    http3_conn: Option<quiche::h3::Connection>,
    partial_responses: HashMap<u64, PartialResponse>,
    progress_status_requests: HashMap<u64, u64>,
    headers_sending_tracker: HashMap<u64, bool>,
    body_sending_tracker: HashMap<u64, (usize, usize)>,
    pending_body_queue: BodyReqQueue<BodyRequest>,
}
impl Client {
    pub fn conn(&mut self) -> &mut Connection {
        &mut self.conn
    }

    pub fn h3_conn(&mut self) -> &mut h3::Connection {
        self.http3_conn.as_mut().unwrap()
    }
    pub fn set_headers_send(&mut self, stream_id: u64, value: bool) {
        self.headers_sending_tracker
            .entry(stream_id)
            .insert_entry(value);
    }
    pub fn headers_send(&self, stream_id: u64) -> bool {
        if let Some(entry) = self.headers_sending_tracker.get(&stream_id) {
            *entry
        } else {
            false
        }
    }
    pub fn check_writable_packet_for_stream(&self, stream_id: u64) -> bool {
        if let Some(_entry) = self.partial_responses.get(&stream_id) {
            true
        } else {
            false
        }
    }
    pub fn set_body_size_to_body_sending_tracker(&mut self, stream_id: u64, body_size: usize) {
        self.body_sending_tracker
            .entry(stream_id)
            .insert_entry((0, body_size));
    }
    pub fn set_written_to_body_sending_tracker(&mut self, stream_id: u64, written: usize) {
        self.body_sending_tracker
            .entry(stream_id)
            .and_modify(|e| e.0 += written);
    }
    pub fn get_written(&self, stream_id: u64) -> (usize, usize) {
        if let Some(entry) = self.body_sending_tracker.get(&stream_id) {
            *entry
        } else {
            (0, 0)
        }
    }

    pub fn is_body_totally_written(&self, stream_id: u64) -> bool {
        if let Some(entry) = self.body_sending_tracker.get(&stream_id) {
            if entry.0 == entry.1 {
                true
            } else {
                false
            }
        } else {
            false
        }
    }
    pub fn get_max_pending_queue_size(&self) -> Option<(u64, usize)> {
        self.pending_body_queue.max_pending_queue()
    }
    pub fn clean_client(&mut self, stream_id: u64) {
        self.headers_sending_tracker.remove(&stream_id);
        self.partial_responses.remove(&stream_id);
        self.progress_status_requests.remove(&stream_id);
    }
}
type ClientMap = HashMap<quiche::ConnectionId<'static>, Client>;

trait PriorityUpdate {
    fn priority_value_mut(&mut self, round: usize) -> Vec<&mut Client> {
        let vec = Vec::new();
        vec
    }
    fn has_too_much_pending(&self, thres: usize) -> bool {
        true
    }
}

impl PriorityUpdate for ClientMap {
    fn has_too_much_pending(&self, thres: usize) -> bool {
        for (id, client) in self.iter() {
            if let Some((stream_id, queue_size)) = client.get_max_pending_queue_size() {
                if queue_size >= thres {
                    return true;
                }
            }
        }
        false
    }
    fn priority_value_mut(&mut self, round: usize) -> Vec<&mut Client> {
        let mut priority_queue: VecDeque<&mut Client> = VecDeque::new();

        let mut size_track: Vec<(&[u8], usize, &mut Client)> = vec![]; //conn_id, queue_size,
                                                                       //index in vec

        for (id, client) in self.iter_mut() {
            if let Some((stream_id, queue_size)) = client.get_max_pending_queue_size() {
                size_track.push((id, queue_size, client))
            }
        }

        size_track.sort_by_key(|it| Reverse(it.1));

        let v = size_track.into_iter().map(|it| it.2).collect();

        v
    }
}

pub use quiche_implementation::run;
mod quiche_implementation {
    use core::{error, panic};
    use std::{
        alloc::GlobalAlloc,
        borrow::Borrow,
        sync::{Arc, Mutex},
        time::{Duration, Instant},
    };

    use mio::Waker;
    use quiche::{h3::Priority, ConnectionId};
    use uuid::timestamp::context;

    use crate::{
        request_response::{BodyRequest, ChunkingStation, ResponseQueue},
        server_config::{self, RouteHandler},
        ResponseBuilderSender, RouteManager, ServerConfig,
    };

    use super::*;

    pub fn run(server_config: Arc<ServerConfig>, route_manager: RouteManager) {
        let mut buf = [0; 65535];
        let mut out = [0; MAX_DATAGRAM_SIZE];

        // Setup the event loop.
        let mut poll = mio::Poll::new().unwrap();
        let mut events = mio::Events::with_capacity(8192 * 8);
        // Create the UDP listening socket, and register it with the event loop.
        let mut socket = mio::net::UdpSocket::bind(*server_config.server_address()).unwrap();
        println!("socket [{:?}]", socket);
        poll.registry()
            .register(
                &mut socket,
                mio::Token(0),
                mio::Interest::READABLE | mio::Interest::WRITABLE,
            )
            .unwrap();

        let waker = Arc::new(Waker::new(poll.registry(), WAKER_TOKEN).unwrap());
        let waker_clone = waker.clone();
        // init the request Response queue with mio waker
        let response_queue = ResponseQueue::new(waker_clone.clone());

        let mut last_time_spend = Arc::new(Mutex::new(Duration::from_micros(100)));
        let mut socket_time = Duration::from_micros(33);
        let chunking_station = ChunkingStation::new(waker_clone.clone(), last_time_spend.clone());
        // Create the configuration for the QUIC connections.
        let mut config = quiche::Config::new(quiche::PROTOCOL_VERSION).unwrap();
        config
            .load_cert_chain_from_pem_file(server_config.cert_path())
            .unwrap();
        config
            .load_priv_key_from_pem_file(server_config.key_path())
            .unwrap();
        config
            .set_application_protos(quiche::h3::APPLICATION_PROTOCOL)
            .unwrap();
        config.set_max_idle_timeout(20_000);
        config.set_max_recv_udp_payload_size(MAX_DATAGRAM_SIZE);
        config.set_max_send_udp_payload_size(MAX_DATAGRAM_SIZE);
        config.set_initial_max_data(1_000_000_000);
        config.set_initial_max_stream_data_bidi_local(1_500_000_000);
        config.set_initial_max_stream_data_bidi_remote(1_500_000_000);
        config.set_initial_max_stream_data_uni(10_000_000);
        config.set_initial_max_streams_bidi(100);
        config.set_initial_max_streams_uni(100);
        config.set_disable_active_migration(true);
        config.set_cc_algorithm(quiche::CongestionControlAlgorithm::BBR2);
        config.enable_early_data();
        let h3_config = quiche::h3::Config::new().unwrap();
        let rng = SystemRandom::new();
        let conn_id_seed = ring::hmac::Key::generate(ring::hmac::HMAC_SHA256, &rng).unwrap();
        let mut clients = ClientMap::new();
        let local_addr = socket.local_addr().unwrap();
        let mut total = 0;
        let mut send_packet_time = Instant::now();
        let mut round = 0;
        loop {
            round += 1;
            // Find the shorter timeout from all the active connections.
            //
            // TODO: use event loop that properly supports timers
            let timeout = clients.values().filter_map(|c| c.conn.timeout()).min();
            poll.poll(&mut events, timeout).unwrap();
            // Read incoming UDP packets from the socket and feed them to quiche,
            // until there are no more packets to read.
            'read: loop {
                // If the event loop reported no events, it means that the timeout
                // has expired, so handle it without attempting to read packets. We
                // will then proceed with the send loop.
                if events.is_empty() {
                    clients.values_mut().for_each(|c| c.conn.on_timeout());
                    break 'read;
                }
                let (len, from) = match socket.recv_from(&mut buf) {
                    Ok(v) => v,
                    Err(e) => {
                        // There are no more UDP packets to read, so end the read
                        // loop.
                        if e.kind() == std::io::ErrorKind::WouldBlock {
                            debug!("recv() would block");
                            break 'read;
                        }
                        panic!("recv() failed: {:?}", e);
                    }
                };
                debug!("got {} bytes", len);
                let pkt_buf = &mut buf[..len];
                // Parse the QUIC packet's header.
                let hdr = match quiche::Header::from_slice(pkt_buf, quiche::MAX_CONN_ID_LEN) {
                    Ok(v) => v,
                    Err(e) => {
                        error!("Parsing packet header failed: {:?}", e);
                        continue 'read;
                    }
                };
                trace!("got packet {:?}", hdr);
                let conn_id = ring::hmac::sign(&conn_id_seed, &hdr.dcid);
                let conn_id = &conn_id.as_ref()[..quiche::MAX_CONN_ID_LEN];
                let conn_id = conn_id.to_vec().into();
                // Lookup a connection based on the packet's connection ID. If there
                // is no connection matching, create a new one.
                let client = if !clients.contains_key(&hdr.dcid) && !clients.contains_key(&conn_id)
                {
                    if hdr.ty != quiche::Type::Initial {
                        error!("Packet is not Initial");
                        continue 'read;
                    }
                    if !quiche::version_is_supported(hdr.version) {
                        debug!("Doing version negotiation");
                        let len =
                            quiche::negotiate_version(&hdr.scid, &hdr.dcid, &mut out).unwrap();
                        let out = &out[..len];
                        if let Err(e) = socket.send_to(out, from) {
                            if e.kind() == std::io::ErrorKind::WouldBlock {
                                debug!("send() would block");
                                break;
                            }
                            panic!("send() failed: {:?}", e);
                        }
                        continue 'read;
                    }
                    let mut scid = [0; quiche::MAX_CONN_ID_LEN];
                    scid.copy_from_slice(&conn_id);
                    let scid = quiche::ConnectionId::from_ref(&scid);
                    // Token is always present in Initial packets.
                    let token = hdr.token.as_ref().unwrap();
                    // Do stateless retry if the client didn't send a token.
                    if token.is_empty() {
                        debug!("Doing stateless retry");
                        let new_token = mint_token(&hdr, &from);
                        let len = quiche::retry(
                            &hdr.scid,
                            &hdr.dcid,
                            &scid,
                            &new_token,
                            hdr.version,
                            &mut out,
                        )
                        .unwrap();
                        let out = &out[..len];
                        if let Err(e) = socket.send_to(out, from) {
                            if e.kind() == std::io::ErrorKind::WouldBlock {
                                debug!("send() would block");
                                break;
                            }
                            panic!("send() failed: {:?}", e);
                        }
                        continue 'read;
                    }
                    let odcid = validate_token(&from, token);
                    // The token was not valid, meaning the retry failed, so
                    // drop the packet.
                    if odcid.is_none() {
                        error!("Invalid address validation token");
                        continue 'read;
                    }
                    if scid.len() != hdr.dcid.len() {
                        error!("Invalid destination connection ID");
                        continue 'read;
                    }
                    // Reuse the source connection ID we sent in the Retry packet,
                    // instead of changing it again.
                    let scid = hdr.dcid.clone();
                    //debug!("New connection: dcid={:?} scid={:?}", hdr.dcid, scid);
                    warn!("New connection: dcid={:?} scid={:?}", hdr.dcid, scid);
                    let conn = quiche::accept(&scid, odcid.as_ref(), local_addr, from, &mut config)
                        .unwrap();
                    let client = Client {
                        conn,
                        http3_conn: None,
                        partial_responses: HashMap::new(),
                        progress_status_requests: HashMap::new(),
                        headers_sending_tracker: HashMap::new(),
                        body_sending_tracker: HashMap::new(),
                        pending_body_queue: BodyReqQueue::<BodyRequest>::new(scid.as_ref()),
                    };
                    clients.insert(scid.clone(), client);
                    clients.get_mut(&scid).unwrap()
                } else {
                    match clients.get_mut(&hdr.dcid) {
                        Some(v) => v,
                        None => clients.get_mut(&conn_id).unwrap(),
                    }
                };
                let recv_info = quiche::RecvInfo {
                    to: socket.local_addr().unwrap(),
                    from,
                };
                // Process potentially coalesced packets.
                let read = match client.conn.recv(pkt_buf, recv_info) {
                    Ok(v) => v,
                    Err(e) => {
                        error!("{} recv failed: {:?}", client.conn.trace_id(), e);
                        continue 'read;
                    }
                };
                debug!("{} processed {} bytes", client.conn.trace_id(), read);
                // Create a new HTTP/3 connection as soon as the QUIC connection
                // is established.
                if (client.conn.is_in_early_data() || client.conn.is_established())
                    && client.http3_conn.is_none()
                {
                    debug!(
                        "{} QUIC handshake completed, now trying HTTP/3",
                        client.conn.trace_id()
                    );

                    info!(
                        "{} QUIC handshake completed, now trying HTTP/3",
                        client.conn.trace_id()
                    );
                    let h3_conn = match quiche::h3::Connection::with_transport(
                        &mut client.conn,
                        &h3_config,
                    ) {
                        Ok(v) => v,
                        Err(e) => {
                            error!("failed to create HTTP/3 connection: {}", e);
                            continue 'read;
                        }
                    };
                    // TODO: sanity check h3 connection before adding to map
                    client.http3_conn = Some(h3_conn);
                }
                if client.http3_conn.is_some() {
                    // Process HTTP/3 events.
                    let mut req_recvd = 0;
                    while req_recvd < 10 {
                        let http3_conn = client.http3_conn.as_mut().unwrap();
                        match http3_conn.poll(&mut client.conn) {
                            Ok((stream_id, quiche::h3::Event::Headers { list, more_frames })) => {
                                info!("new req [{:?}]", list);
                                route_manager.routes_handler().parse_headers(
                                    &server_config,
                                    &list,
                                    stream_id,
                                    client,
                                    more_frames,
                                    &chunking_station,
                                    &waker_clone,
                                    &last_time_spend,
                                );
                                req_recvd += 1;
                                /*
                                handle_request(
                                    client,
                                    stream_id,
                                    &list,
                                    &server_config.request_handler(),
                                );
                                */
                            }
                            Ok((stream_id, quiche::h3::Event::Data)) => {
                                while let Ok(read) =
                                    http3_conn.recv_body(&mut client.conn, stream_id, &mut out)
                                {
                                    let scid = client.conn.source_id().as_ref().to_vec();
                                    if let Err(_) =
                                        route_manager.routes_handler().write_body_packet(
                                            stream_id,
                                            &scid,
                                            client.conn.trace_id(),
                                            &out[..read],
                                            false,
                                        )
                                    {
                                        error!(
                                            "Failed writing body packet on stream_id [{stream_id}]"
                                        );
                                    }
                                    total += read;
                                    req_recvd += 1;
                                }
                                let trace_id = client.conn.trace_id().to_string();

                                if let Err(_) = route_manager
                                    .routes_handler()
                                    .send_reception_status(client, stream_id, trace_id.as_str())
                                {
                                    error!("Failed to send progress response status")
                                }
                            }
                            Ok((stream_id, quiche::h3::Event::Finished)) => {
                                debug!("finished ! stream [{}]", stream_id);
                                let trace_id = client.conn.trace_id().to_owned();
                                let scid = client.conn.source_id().as_ref().to_vec();

                                route_manager.routes_handler().handle_finished_stream(
                                    trace_id.as_str(),
                                    &scid,
                                    stream_id,
                                    client,
                                    response_queue.get_sender(),
                                    &chunking_station,
                                    &waker_clone,
                                    &last_time_spend,
                                );
                                ()
                            }
                            Ok((_stream_id, quiche::h3::Event::Reset { .. })) => (),
                            Ok((_prioritized_element_id, quiche::h3::Event::PriorityUpdate)) => {
                                debug!("prioritytupdate");
                                ()
                            }
                            Ok((_goaway_id, quiche::h3::Event::GoAway)) => {
                                debug!("GoAway");
                                ()
                            }
                            Err(quiche::h3::Error::Done) => {
                                break;
                            }
                            Err(e) => {
                                error!("{} HTTP/3 error {:?}", client.conn.trace_id(), e);
                                break;
                            }
                        }
                    }
                }
            }
            // Generate outgoing QUIC packets for all active connections and send
            // them on the UDP socket, until quiche reports that there are no more
            // packets to be sent.

            'manage: {
                if let Ok(mut response_body) = response_queue.pop_request() {
                    //std::thread::sleep(socket_time);
                    let scid = response_body.scid();
                    let connexion_id = ConnectionId::from_vec(scid.to_vec());
                    let stream_id = response_body.stream_id();

                    if let Some(client) = clients.get_mut(&connexion_id) {
                        if !client.pending_body_queue.is_stream_queue_empty(stream_id)
                            | client.partial_responses.get(&stream_id).is_some()
                        {
                            client.pending_body_queue.push_item(response_body);
                        } else {
                            if response_body.len() > client.conn.stream_capacity(stream_id).unwrap()
                            {
                                client.pending_body_queue.push_item(response_body);
                            } else {
                                if let Err(e) = send_body_response(
                                    client,
                                    response_body.stream_id(),
                                    response_body.take_data(),
                                    false,
                                ) {
                                    //       client.pending_body_queue.push_item(response_body);
                                };

                                let is_end = client.is_body_totally_written(stream_id);

                                if is_end {
                                    let b_w = client.get_written(stream_id);
                                    let b_w_0 = client.get_written(0);
                                    send_body_response(client, stream_id, vec![], is_end).unwrap();
                                }
                            }
                        }
                    }
                }
                for client in clients.priority_value_mut(round) {
                    // Handle writable streams.
                    let mut to_repush_in_front: Vec<BodyRequest> = vec![];
                    while let Some((stream_id, queue)) = client.pending_body_queue.next_stream() {
                        if let Some(queue) = queue {
                            if client.partial_responses.get(&stream_id).is_some() {
                                if let Some(_val) = handle_writable(client, stream_id) {}
                                continue;
                            }
                            if queue.len() == 0 {
                                //  if client.partial_responses.get(&stream_id).is_some() {
                                //    if let Some(_val) = handle_writable(client, stream_id) {}
                                //  continue;
                                //}
                                continue;
                            }
                            /*
                            if client.partial_responses.get(&stream_id).is_some() {
                                if let Some(_val) = handle_writable(client, stream_id) {}
                                continue;
                            }*/
                            let mut item = queue.pop_front().unwrap();
                            if item.len() > client.conn.stream_capacity(stream_id).unwrap() {
                                if stream_id == 0 {}
                                queue.push_front(item);
                                continue;
                            }

                            if let Err(_e) = send_body_response(
                                client,
                                item.stream_id(),
                                item.take_data(),
                                false,
                            ) {
                                warn!(
                                    "repushed_front [{:?}] id [{:?}]",
                                    item.packet_id(),
                                    item.stream_id()
                                );
                                to_repush_in_front.push(item);
                                continue;
                            };

                            let is_end = client.is_body_totally_written(item.stream_id());
                            let b_w = client.get_written(stream_id);
                            if b_w.0 > b_w.1 {
                                error!("is superior [{}]", stream_id);
                            }
                            /*
                                                        if stream_id == 0 {
                                                            warn!(" writen {:?}", b_w);
                                                        }
                            */
                            if is_end {
                                if let Err(e) =
                                    send_body_response(client, item.stream_id(), vec![], is_end)
                                {
                                    error!("failed to send final header for stream [{stream_id}]");
                                }
                            }
                        }
                    }
                    for body in to_repush_in_front {
                        if body.stream_id() == 0 {
                            warn!("repushing body [{}]", body.packet_id());
                        }
                        client.pending_body_queue.push_item_on_front(body);
                    }
                }
                if let Err(_e) = waker_clone.wake() {
                    error!("failed to wake poll");
                }
            }

            /*
                        if round % 2000 == 0 {
                            for client in clients.priority_value_mut(round) {
                                let str_v = client.pending_body_queue.stream_vec();
                                if let Some(it) = client.pending_body_queue.for_stream() {
                                    for (stream_id, queue) in it {
                                        info!("bytes written [{:?}] in queue -> [{}] for stream [{stream_id}] conn [{:?}] str_t{:?}", client.get_written(*stream_id), queue.len(), client.conn.trace_id(), str_v);
                                    }
                                }
                                println!("------------------------------------------------------------------");
                            }
                            if !clients.is_empty() {
                                println!("");
                                println!("------------------------------------------------------------------");
                                println!("");
                            }
                        }
            */
            let pacing_delay = Duration::from_micros(124);
            let mut send_duration = Instant::now();
            let mut has_blocked = false;
            for client in clients.values_mut() {
                let mut packet_send = 0;
                loop {
                    let (write, send_info) = match client.conn.send(&mut out) {
                        Ok(v) => v,
                        Err(quiche::Error::Done) => {
                            break;
                        }
                        Err(e) => {
                            error!("{} send failed: {:?}", client.conn.trace_id(), e);
                            warn!("{} send failed: {:?}", client.conn.trace_id(), e);
                            client.conn.close(false, 0x1, b"fail").ok();
                            break;
                        }
                    };
                    if let Err(e) = socket.send_to(&out[..write], send_info.to) {
                        if e.kind() == std::io::ErrorKind::WouldBlock {
                            debug!("send() would block, [{:?}]", e);
                            has_blocked = true;
                            break;
                        }
                        panic!("send() failed: {:?}", e);
                    }
                    debug!("\n{} written {} bytes", client.conn.trace_id(), write);
                    //  println!("{} written {} bytes", client.conn.trace_id(), write);
                    packet_send += 1;
                    if packet_send >= 10 {
                        //        break;
                    }

                    while send_duration.elapsed() < pacing_delay {
                        std::thread::yield_now();
                    }
                }
                //std::thread::sleep(Duration::from_micros(500));
            }
            socket_time = if has_blocked {
                send_duration.elapsed()
            } else {
                socket_time
            };
            *last_time_spend.lock().unwrap() = send_packet_time.elapsed();
            send_packet_time = Instant::now();
            // Garbage collect closed connections.
            clients.retain(|_, ref mut c| {
                if c.conn.is_closed() {
                    error!(
                        "{} connection collected {:?}",
                        c.conn.trace_id(),
                        c.conn.stats()
                    );
                }
                !c.conn.is_closed()
            });
        }
    }
    /// Generate a stateless retry token.
    ///
    /// The token includes the static string `"quiche"` followed by the IP address
    /// of the client and by the original destination connection ID generated by the
    /// client.
    ///
    /// Note that this function is only an example and doesn't do any cryptographic
    /// authenticate of the token. *It should not be used in production system*.
    fn mint_token(hdr: &quiche::Header, src: &net::SocketAddr) -> Vec<u8> {
        let mut token = Vec::new();
        token.extend_from_slice(b"quiche");
        let addr = match src.ip() {
            std::net::IpAddr::V4(a) => a.octets().to_vec(),
            std::net::IpAddr::V6(a) => a.octets().to_vec(),
        };
        token.extend_from_slice(&addr);
        token.extend_from_slice(&hdr.dcid);
        token
    }
    /// Validates a stateless retry token.
    ///
    /// This checks that the ticket includes the `"quiche"` static string, and that
    /// the client IP address matches the address stored in the ticket.
    ///
    /// Note that this function is only an example and doesn't do any cryptographic
    /// authenticate of the token. *It should not be used in production system*.
    fn validate_token<'a>(
        src: &net::SocketAddr,
        token: &'a [u8],
    ) -> Option<quiche::ConnectionId<'a>> {
        if token.len() < 6 {
            return None;
        }
        if &token[..6] != b"quiche" {
            return None;
        }
        let token = &token[6..];
        let addr = match src.ip() {
            std::net::IpAddr::V4(a) => a.octets().to_vec(),
            std::net::IpAddr::V6(a) => a.octets().to_vec(),
        };
        if token.len() < addr.len() || &token[..addr.len()] != addr.as_slice() {
            return None;
        }
        Some(quiche::ConnectionId::from_ref(&token[addr.len()..]))
    }
    /// Handles incoming HTTP/3 requests.
    pub fn send_header(
        client: &mut Client,
        stream_id: u64,
        headers: Vec<quiche::h3::Header>,
        is_end: bool,
    ) -> Result<usize, ()> {
        let http3_conn = &mut client.http3_conn.as_mut().unwrap();
        let conn = &mut client.conn;
        if let Err(e) = http3_conn.send_response_with_priority(
            conn,
            stream_id,
            &headers,
            &Priority::new(7, true),
            is_end,
        ) {
            error!("Failed send intermediate 100 response, [{:?}]", e)
        };
        Ok(1)
    }
    pub fn send_more_header(
        client: &mut Client,
        stream_id: u64,
        headers: Vec<quiche::h3::Header>,
        is_end: bool,
    ) -> Result<usize, ()> {
        let http3_conn = &mut client.http3_conn.as_mut().unwrap();
        let conn = &mut client.conn;
        if let Err(e) = http3_conn.send_additional_headers(conn, stream_id, &headers, false, is_end)
        {
            error!("Failed send intermediate 100 response, [{:?}]", e)
        };
        Ok(1)
    }
    pub fn send_body(
        client: &mut Client,
        stream_id: u64,
        body: Vec<u8>,
        is_end: bool,
    ) -> Result<usize, ()> {
        if body.len() == 0 {
            return Ok(0);
        }
        debug!("sending body [{}] bytes", body.len());
        let http3_conn = &mut client.http3_conn.as_mut().unwrap();
        let conn = &mut client.conn;
        let written = match http3_conn.send_body(conn, stream_id, &body, is_end) {
            Ok(v) => v,
            Err(quiche::h3::Error::Done) => 0,
            Err(e) => {
                error!(
                    "{}-[{}] DStream send failed {:?}",
                    conn.trace_id(),
                    stream_id,
                    e
                );
                return Err(());
            }
        };
        if written < body.len() {
            let response = PartialResponse {
                headers: None,
                body,
                written,
            };
            client.partial_responses.insert(stream_id, response);
        }
        return Ok(written);
        /*
         *
         * Send reponse to the client
         *
         * */
        Err(())
    }
    pub fn send_response_when_finished(
        client: &mut Client,
        stream_id: u64,
        headers: Vec<quiche::h3::Header>,
        body: Vec<u8>,
        is_end: bool,
    ) -> Result<usize, ()> {
        let http3_conn = &mut client.http3_conn.as_mut().unwrap();
        let conn = &mut client.conn;
        match http3_conn.send_additional_headers(
            conn,
            stream_id,
            &headers,
            false,
            if body.len() == 0 { true } else { false },
        ) {
            Ok(v) => v,
            Err(quiche::h3::Error::StreamBlocked) => {
                let response = PartialResponse {
                    headers: Some(headers),
                    body,
                    written: 0,
                };
                client.partial_responses.insert(stream_id, response);
                warn!("streamblocked [{stream_id}]");
                return Ok(0);
            }
            Err(e) => {
                error!(
                    "{}, headers [{:#?}] Estream send failed {:?}",
                    conn.trace_id(),
                    headers,
                    e
                );
                return Err(());
            }
        }
        if body.len() == 0 {
            return Ok(0);
        }
        debug!("sending body [{}] bytes", body.len());
        let written = match http3_conn.send_body(conn, stream_id, &body, true) {
            Ok(v) => v,
            Err(quiche::h3::Error::Done) => 0,
            Err(e) => {
                error!("{} Bstream send failed {:?}", conn.trace_id(), e);
                return Err(());
            }
        };
        if written < body.len() {
            let response = PartialResponse {
                headers: None,
                body,
                written,
            };
            client.partial_responses.insert(stream_id, response);
        }
        /*
         *
         * Send reponse to the client
         *
         * */
        Ok(written)
    }
    pub fn send_response(
        client: &mut Client,
        stream_id: u64,
        headers: Vec<quiche::h3::Header>,
        body: Vec<u8>,
        is_end: bool,
    ) -> Result<usize, ()> {
        let http3_conn = &mut client.http3_conn.as_mut().unwrap();
        let conn = &mut client.conn;
        match http3_conn.send_response_with_priority(
            conn,
            stream_id,
            &headers,
            &Priority::default(),
            is_end,
        ) {
            Ok(v) => v,
            Err(quiche::h3::Error::StreamBlocked) => {
                let response = PartialResponse {
                    headers: Some(headers),
                    body,
                    written: 0,
                };
                client.partial_responses.insert(stream_id, response);
                return Err(());
            }
            Err(e) => {
                error!(
                    "{}, headers [{:#?}] stream send failed {:?}",
                    conn.trace_id(),
                    headers,
                    e
                );
                return Err(());
            }
        }
        if body.len() == 0 {
            return Ok(0);
        }
        debug!("sending body [{}] bytes", body.len());
        let written = match http3_conn.send_body(conn, stream_id, &body, true) {
            Ok(v) => v,
            Err(quiche::h3::Error::Done) => 0,
            Err(e) => {
                error!("{} [{}] Esend failed {:?}", conn.trace_id(), stream_id, e);
                return Err(());
            }
        };
        if written < body.len() {
            let response = PartialResponse {
                headers: None,
                body,
                written,
            };
            client.partial_responses.insert(stream_id, response);
        }
        /*
         *
         * Send reponse to the client
         *
         * */
        Ok(written)
    }
    fn send_body_response(
        client: &mut Client,
        stream_id: u64,
        data: Vec<u8>,
        is_end: bool,
    ) -> Result<usize, ()> {
        debug!("sending body [{}] bytes", data.len());
        let http3_conn = &mut client.http3_conn.as_mut().unwrap();
        let conn = &mut client.conn;

        let written = match http3_conn.send_body(conn, stream_id, &data, is_end) {
            Ok(v) => {
                if is_end {
                    info!(
                        "final send for [{stream_id}] {}/{} bytes written. [{:?}] [{:?}]",
                        v,
                        data.len(),
                        client.get_written(stream_id),
                        client.conn.trace_id()
                    )
                };
                v
            }
            Err(quiche::h3::Error::Done) => 0,
            Err(e) => {
                error!(
                    " stramcap [{:?} ]is_end [{:?}]  ...{} [{}] Dsend failed {:?}",
                    conn.stream_capacity(stream_id),
                    is_end,
                    conn.trace_id(),
                    stream_id,
                    e
                );
                return Err(());
            }
        };
        if stream_id == 0 {
            if data.len() != written {
                error!("no onnono [{written}/[{}]]", data.len());
            }
        }
        if written < data.len() {
            let response = PartialResponse {
                headers: None,
                body: data,
                written,
            };
            client.partial_responses.insert(stream_id, response);
        }
        client.set_written_to_body_sending_tracker(stream_id, written);
        /*
         *
         * Send reponse to the client
         *
         * */
        Ok(written)
    }
    fn handle_request(
        client: &mut Client,
        stream_id: u64,
        headers: &[quiche::h3::Header],
        route_handler: &RouteHandler,
    ) {
        let conn = &mut client.conn;
        let http3_conn = &mut client.http3_conn.as_mut().unwrap();
        debug!(
            "{} got request {:?} on stream id {}",
            conn.trace_id(),
            hdrs_to_strings(headers),
            stream_id
        );
        // We decide the response based on headers alone, so stop reading the
        // request stream so that any body is ignored and pointless Data events
        // are not generated.
        //conn.stream_shutdown(stream_id, quiche::Shutdown::Read, 0)
        //   .unwrap();
        let (headers, body) = build_response("", headers, route_handler);
        match http3_conn.send_response(conn, stream_id, &headers, false) {
            Ok(v) => v,
            Err(quiche::h3::Error::StreamBlocked) => {
                let response = PartialResponse {
                    headers: Some(headers),
                    body,
                    written: 0,
                };
                client.partial_responses.insert(stream_id, response);
                return;
            }
            Err(e) => {
                error!("ici stream send failed {:?}", e);
                return;
            }
        }
        let written = match http3_conn.send_body(conn, stream_id, &body, true) {
            Ok(v) => v,
            Err(quiche::h3::Error::Done) => 0,
            Err(e) => {
                error!("{}aa stream send failed {:?}", conn.trace_id(), e);
                return;
            }
        };
        if written < body.len() {
            let response = PartialResponse {
                headers: None,
                body,
                written,
            };
            client.partial_responses.insert(stream_id, response);
        }
    }
    /// Builds an HTTP/3 response given a request.
    fn build_response(
        root: &str,
        request: &[quiche::h3::Header],
        request_handler: &RouteHandler,
    ) -> (Vec<quiche::h3::Header>, Vec<u8>) {
        let mut file_path = std::path::PathBuf::from(root);
        let mut path = std::path::Path::new("");
        let mut method = None;
        // Look for the request's path and method.
        for hdr in request {
            match hdr.name() {
                b":path" => path = std::path::Path::new(std::str::from_utf8(hdr.value()).unwrap()),
                b":method" => method = Some(hdr.value()),
                _ => (),
            }
        }

        let (status, body) = match method {
            Some(b"GET") => {
                for c in path.components() {
                    if let std::path::Component::Normal(v) = c {
                        file_path.push(v)
                    }
                }
                match std::fs::read(file_path.as_path()) {
                    Ok(data) => (200, data),
                    Err(_) => (404, b"\nHello Error world\n".to_vec()),
                }
            }
            _ => (405, Vec::new()),
        };
        let headers = vec![
            quiche::h3::Header::new(b":status", status.to_string().as_bytes()),
            quiche::h3::Header::new(b"server", b"quiche"),
            quiche::h3::Header::new(b"content-length", body.len().to_string().as_bytes()),
        ];
        (headers, body)
    }
    /// Handles newly writable streams.
    fn handle_writable(client: &mut Client, stream_id: u64) -> Option<bool> {
        let is_end = client.is_body_totally_written(stream_id);
        let amount_written = client.get_written(stream_id);
        let conn = &mut client.conn;
        let http3_conn = &mut client.http3_conn.as_mut().unwrap();
        debug!("{} stream {} is writable", conn.trace_id(), stream_id);
        if !client.partial_responses.contains_key(&stream_id) {
            return None;
        }
        let resp = client.partial_responses.get_mut(&stream_id).unwrap();
        if let Some(ref headers) = resp.headers {
            match http3_conn.send_response(conn, stream_id, headers, false) {
                Ok(_) => (),
                Err(quiche::h3::Error::StreamBlocked) => {
                    return None;
                }
                Err(e) => {
                    error!(
                        "{} handle writabel stream send failed {:?}",
                        conn.trace_id(),
                        e
                    );
                    return None;
                }
            }
        }
        resp.headers = None;

        let body = if !is_end {
            &resp.body[resp.written..]
        } else {
            &resp.body[resp.written..]
        };
        let data_len = body.len();
        let written = match http3_conn.send_body(conn, stream_id, body, is_end) {
            Ok(v) => v,
            Err(quiche::h3::Error::Done) => 0,
            Err(e) => {
                client.partial_responses.remove(&stream_id);
                error!("{} stream send failed {:?}", conn.trace_id(), e);
                return None;
            }
        };
        resp.written += written;

        if resp.written == resp.body.len() {
            let written_total = resp.written;
            client.partial_responses.remove(&stream_id);
            let _o = if written_total + amount_written.0 == amount_written.1 {
                let _ = match http3_conn.send_body(conn, stream_id, &[], true) {
                    Ok(v) => v,
                    Err(quiche::h3::Error::Done) => 0,
                    Err(e) => {
                        error!("{} stream send failed {:?}", conn.trace_id(), e);
                        return None;
                    }
                };
            };
            client.set_written_to_body_sending_tracker(stream_id, written);
            return Some(true);
        }

        if written > 0 {
            client.set_written_to_body_sending_tracker(stream_id, written);
            return Some(false);
        }

        let o = if resp.written + amount_written.0 == amount_written.1 {
            let _ = match http3_conn.send_body(conn, stream_id, &[], true) {
                Ok(v) => v,
                Err(quiche::h3::Error::Done) => 0,
                Err(e) => {
                    client.partial_responses.remove(&stream_id);
                    error!("{} stream send failed {:?}", conn.trace_id(), e);
                    return None;
                }
            };
            client.partial_responses.remove(&stream_id);
            Some(true)
        } else {
            Some(false)
        };
        client.set_written_to_body_sending_tracker(stream_id, written);
        o
    }
    pub fn hdrs_to_strings(hdrs: &[quiche::h3::Header]) -> Vec<(String, String)> {
        hdrs.iter()
            .map(|h| {
                let name = String::from_utf8_lossy(h.name()).to_string();
                let value = String::from_utf8_lossy(h.value()).to_string();
                (name, value)
            })
            .collect()
    }
}

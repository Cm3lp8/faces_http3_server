pub use stream_sessions::StreamSessions;
pub use stream_sessions_traits::*;
pub use stream_types::{StreamBuilder, StreamIdent, StreamMessageCapsule, StreamType};
pub use user_sessions_trait::UserSessions;
use uuid::Uuid;

type UserUuid = Uuid;
mod stream_sessions {
    use std::{
        collections::{HashMap, HashSet},
        sync::{Arc, Mutex},
    };

    use bincode::{Decode, Encode};
    use stream_framer::FrameWriter;

    use crate::{
        prelude::StreamMessageCapsule,
        request_response::{BodyRequest, BodyType, ChunkingStation},
        stream_sessions::UserUuid,
        StreamBridgeOps, StreamIdent,
    };

    use super::{
        stream_types::{stream_cleaning, Stream},
        user_sessions_trait::UserSessions,
        StreamBridge, StreamBuilder, StreamCreation, StreamManagement, ToStreamIdent,
    };

    type StreamPath = String;

    /// # Streams register
    ///
    /// StreamSessions registers sesssions routes opened via the router
    /// and keeps track of the users ids (quic dcid and stream id (u64))
    ///
    /// # Type annotation
    ///
    /// T: The user session type, that has to implement UserSessions<Output = T> interface
    pub struct StreamSessions<T: UserSessions<Output = T>> {
        inner: Arc<Mutex<StreamSessionsInner<T>>>,
    }

    impl<T: UserSessions<Output = T>> Clone for StreamSessions<T> {
        fn clone(&self) -> Self {
            Self {
                inner: self.inner.clone(),
            }
        }
    }
    impl<T: UserSessions<Output = T>> StreamSessions<T> {
        pub fn new() -> Self {
            Self {
                inner: Arc::new(Mutex::new(StreamSessionsInner::new())),
            }
        }
        pub fn stream_path_set(&self) -> HashSet<String> {
            self.inner.lock().unwrap().stream_pathes_set()
        }

        fn mut_access(&self, cb: impl FnOnce(&mut StreamSessionsInner<T>)) {
            let guard = &mut *self.inner.lock().unwrap();
            cb(guard);
        }
        /// Set the ChunkingStation object to send stream data to the connection.
        pub fn set_chunking_station(&self, chunking_station: &ChunkingStation) {
            let guard = &mut *self.inner.lock().unwrap();

            guard.chunking_station = Some(chunking_station.clone());
        }
    }

    impl<T: UserSessions<Output = T>> StreamManagement<T> for StreamSessions<T> {
        fn get_stream_from_path<R>(
            &self,
            path: &str,
            cb: impl FnOnce(&mut Stream<T>) -> R,
        ) -> Result<R, ()> {
            let guard = &mut *self.inner.lock().unwrap();

            if let Some(stream) = guard.sessions.get_mut(path) {
                Ok(cb(stream))
            } else {
                Err(())
            }
        }
        fn get_stream_session<V>(
            &self,
            cb: impl FnOnce(&StreamSessionsInner<T>) -> Result<V, String>,
        ) -> Result<V, String> {
            let guard = &mut *self.inner.lock().unwrap();

            cb(guard)
        }
        fn retry_send_message_to_subscriber_on_path<U: Decode<()> + Encode + Clone>(
            &self,
            user_id: UserUuid,
            message: StreamMessageCapsule<U>,
            path: &str,
        ) -> Result<StreamMessageCapsule<U>, String> {
            self.get_stream_session(|session| {
                let Some(stream) = session.sessions.get(path) else {
                    return Err("Stream on path : No Session found ".to_string());
                };

                let conn_ids = (message.get_scid(), message.stream_id());
                let conn_ids = (&conn_ids.0[..], conn_ids.1);
                match session.send_one_message_on_stream(conn_ids, |_| {
                    let message = message.clone();
                    let enc = bincode::encode_to_vec(&message, bincode::config::standard())
                        .map_err(|e| e.to_string())?;

                    Ok((message, enc))
                }) {
                    Ok(messages) => Ok(messages),
                    Err(e) => {
                        error!("Failed To send message to [{:?}] ", user_id);
                        Err(e)
                    }
                }
            })
            .map_err(|e| String::from("Failed to send message to subscriber"))
        }
        // TODO review the necessity of this
        fn retry_send_message_to_subscriber_on_path_with_filtered_scids<
            U: Decode<()> + Encode + Clone,
        >(
            &self,
            user_id: UserUuid,
            message: StreamMessageCapsule<U>,
            path: &str,
            scids: &[&[u8]],
        ) -> Result<StreamMessageCapsule<U>, String> {
            self.get_stream_session(|session| {
                let Some(stream) = session.sessions.get(path) else {
                    return Err("Stream on path : No Session found ".to_string());
                };

                let conn_ids = (message.get_scid(), message.stream_id());
                let conn_ids = (&conn_ids.0[..], conn_ids.1);
                match session.send_one_message_on_stream(conn_ids, |_| {
                    let message = message.clone();
                    let enc = bincode::encode_to_vec(&message, bincode::config::standard())
                        .map_err(|e| e.to_string())?;

                    Ok((message, enc))
                }) {
                    Ok(messages) => Ok(messages),
                    Err(e) => {
                        error!("Failed To send message to [{:?}] ", user_id);
                        Err(e)
                    }
                }
            })
            .map_err(|e| String::from("Failed to send message to subscriber"))
        }
        fn send_message_to_subscriber_on_path<U: Decode<()> + Encode + Clone>(
            &self,
            user_id: UserUuid,
            message: U,
            path: &str,
        ) -> Result<Vec<StreamMessageCapsule<U>>, String> {
            self.get_stream_session(|session| {
                let Some(stream) = session.sessions.get(path) else {
                    return Err("Stream on path : No Session found ".to_string());
                };
                let ids: Vec<(Vec<u8>, u64)> = stream
                    .registered_sessions()
                    .get_connection_ids_on_user_ids(&[user_id])
                    .into_iter()
                    .filter_map(|it| {
                        let stream_ident = it.to_stream_ident();

                        match stream_ident {
                            Ok(id) => Some((id.dcid, id.stream_id)),
                            Err(e) => None,
                        }
                    })
                    .collect();

                let ids_ref: Vec<(&[u8], u64)> = ids.iter().map(|i| (&i.0[..], i.1)).collect();
                match session.send_one_or_more_message_on_stream(&ids_ref, |(scid, stream_id)| {
                    let message = StreamMessageCapsule::new(&message, scid, *stream_id);
                    let enc = bincode::encode_to_vec(&message, bincode::config::standard())
                        .map_err(|e| e.to_string());

                    match enc {
                        Ok(res) => Ok((message, res)),
                        Err(e) => Err(e),
                    }
                }) {
                    Ok(messages) => Ok(messages),
                    Err(e) => {
                        error!("Failed To send message to [{:?}] ", user_id);
                        Err(e)
                    }
                }
            })
            .map_err(|e| String::from("Failed to send message to subscriber"))
        }
        fn send_message_to_subscriber_on_path_with_filtered_scids<
            U: Decode<()> + Encode + Clone,
        >(
            &self,
            user_id: UserUuid,
            message: U,
            path: &str,
            scids: &[&[u8]],
        ) -> Result<Vec<StreamMessageCapsule<U>>, String> {
            self.get_stream_session(|session| {
                let Some(stream) = session.sessions.get(path) else {
                    return Err("Stream on path : No Session found ".to_string());
                };
                let ids: Vec<(Vec<u8>, u64)> = stream
                    .registered_sessions()
                    .get_connection_ids_on_user_ids(&[user_id])
                    .into_iter()
                    .filter_map(|it| {
                        let stream_ident = it.to_stream_ident();

                        match stream_ident {
                            Ok(id) => {
                                if scids.contains(&&id.dcid[..]) {
                                    None
                                } else {
                                    Some((id.dcid, id.stream_id))
                                }
                            }
                            Err(e) => None,
                        }
                    })
                    .collect();

                let ids_ref: Vec<(&[u8], u64)> = ids.iter().map(|i| (&i.0[..], i.1)).collect();
                match session.send_one_or_more_message_on_stream(&ids_ref, |(scid, stream_id)| {
                    let message = StreamMessageCapsule::new(&message, scid, *stream_id);
                    let enc = bincode::encode_to_vec(&message, bincode::config::standard())
                        .map_err(|e| e.to_string());

                    match enc {
                        Ok(res) => Ok((message, res)),
                        Err(e) => Err(e),
                    }
                }) {
                    Ok(messages) => Ok(messages),
                    Err(e) => {
                        error!("Failed To send message to [{:?}] ", user_id);
                        Err(e)
                    }
                }
            })
            .map_err(|e| String::from("Failed to send message to subscriber"))
        }
        fn clean_closed_connexions(&self, scid: &[u8]) {
            let guard = &mut *self.inner.lock().unwrap();

            stream_cleaning(&mut guard.sessions, scid);
        }
    }

    impl<T: UserSessions<Output = T>> StreamBridge<T> for StreamSessions<T> {
        fn user_session<U>(&self, cb: impl FnOnce(&StreamSessionsInner<T>) -> U) -> U {
            let guard = &*self.inner.lock().unwrap();

            cb(guard)
        }
    }
    pub struct StreamSessionsInner<T: UserSessions> {
        sessions: HashMap<StreamPath, Stream<T>>,
        chunking_station: Option<ChunkingStation>,
    }

    impl<T: UserSessions<Output = T>> StreamBridgeOps<T> for StreamSessionsInner<T> {
        fn get_connection_ids_on_stream_path_by_user_id(
            &self,
            stream_path: &str,
            peer_id: UserUuid,
        ) -> Result<crate::StreamIdent, ()> {
            if let Some(stream) = self.sessions.get(stream_path) {
                let mut stream_ident: Vec<StreamIdent> = stream
                    .registered_sessions()
                    .get_connection_ids_on_user_ids(&[peer_id])
                    .into_iter()
                    .filter_map(|it| {
                        if let Ok(s) = it.to_stream_ident() {
                            Some(s)
                        } else {
                            None
                        }
                    })
                    .collect();

                if !stream_ident.is_empty() && stream_ident[0].user_id == peer_id {
                    Ok(stream_ident.remove(0))
                } else {
                    Err(())
                }
            } else {
                Err(())
            }
        }
        fn is_connected(&self, stream_path: &str, peer_id: UserUuid) -> bool {
            if let Some(stream) = self.sessions.get(stream_path) {
                stream.registered_sessions().is_user_connected(peer_id)
            } else {
                false
            }
        }
        /// Success means every capsule was queued and the poll loop was notified.
        /// A send failure can occur after earlier targets were queued; it is not
        /// an atomic multicast and never proves delivery to a peer.
        fn send_one_or_more_message_on_stream<U: Decode<()> + Encode + Clone>(
            &self,
            connection_ids: &[(&[u8], u64)],
            data_cb: impl Fn(&(&[u8], u64)) -> Result<(StreamMessageCapsule<U>, Vec<u8>), String>,
        ) -> Result<Vec<StreamMessageCapsule<U>>, String> {
            if connection_ids.is_empty() {
                return Err("no stream targets".to_owned());
            }
            let station = self
                .chunking_station
                .as_ref()
                .ok_or("chunking station unavailable")?;
            // Resolve every queue before encoding or sending to avoid a partial
            // enqueue when a target is already absent.
            let senders = connection_ids
                .iter()
                .map(|ids| {
                    station
                        .get_body_sender(*ids)
                        .ok_or_else(|| "stream queue unavailable".to_owned())
                })
                .collect::<Result<Vec<_>, _>>()?;
            let prepared = connection_ids
                .iter()
                .map(|ids| {
                    let (capsule, data) = data_cb(ids)?;
                    let framed = data
                        .prepend_frame()
                        .map_err(|_| "failed to frame capsule".to_owned())?;
                    Ok((
                        capsule,
                        BodyRequest::new(ids.1, "", ids.0, 0, framed, false),
                    ))
                })
                .collect::<Result<Vec<_>, String>>()?;
            let mut capsules = Vec::with_capacity(prepared.len());
            for (sender, (capsule, body)) in senders.into_iter().zip(prepared) {
                if sender
                    .send(crate::request_response::QueuedRequest::StreamData(body))
                    .is_err()
                {
                    // Earlier targets may already contain their capsule.
                    let _ = station.wake_poll();
                    return Err(format!(
                        "stream queue disconnected after {} accepted targets",
                        capsules.len()
                    ));
                }
                capsules.push(capsule);
            }
            station
                .wake_poll()
                .map_err(|error| format!("failed to wake poll: {error}"))?;
            Ok(capsules)
        }
        fn send_one_message_on_stream<U: Decode<()> + Encode + Clone>(
            &self,
            conn_ids: (&[u8], u64),
            data_cb: impl Fn(&(&[u8], u64)) -> Result<(StreamMessageCapsule<U>, Vec<u8>), String>,
        ) -> Result<StreamMessageCapsule<U>, String> {
            self.send_one_or_more_message_on_stream(&[conn_ids], data_cb)?
                .pop()
                .ok_or_else(|| "no queued capsule".to_owned())
        }
    }

    impl<T: UserSessions> StreamSessionsInner<T> {
        fn new() -> Self {
            Self {
                sessions: HashMap::new(),
                chunking_station: None,
            }
        }
        fn stream_pathes_set(&self) -> HashSet<String> {
            self.sessions.keys().map(|it| it.to_owned()).collect()
        }
    }

    impl<T: UserSessions<Output = T>> StreamCreation<T> for StreamSessions<T> {
        fn create_stream(
            &self,
            path: &str,
            stream_type: super::stream_types::StreamType,
            stream_config: (),
            stream_builder_cb: impl FnOnce(&mut StreamBuilder<T>),
        ) {
            let mut stream_builder = StreamBuilder::new();

            stream_builder_cb(&mut stream_builder);
            stream_builder.set_path(path);
            stream_builder.set_stream_type(stream_type);

            let stream = stream_builder.build();

            if let Ok(stream) = stream {
                self.mut_access(|register| {
                    register.sessions.entry(path.to_owned()).or_insert(stream);
                });
            };
        }
    }

    #[cfg(test)]
    mod delivery_tests {
        use super::*;
        use crate::{request_response::QueuedRequest, test_support::TestSessions};
        use std::time::Duration;

        fn encoded(ids: &(&[u8], u64)) -> Result<(StreamMessageCapsule<u64>, Vec<u8>), String> {
            Ok((StreamMessageCapsule::new(&42, ids.0, ids.1), vec![1, 2, 3]))
        }
        fn station() -> (mio::Poll, ChunkingStation) {
            let poll = mio::Poll::new().unwrap();
            let waker = Arc::new(mio::Waker::new(poll.registry(), mio::Token(0)).unwrap());
            let station = ChunkingStation::new(waker, Arc::new(Mutex::new(Duration::ZERO)));
            (poll, station)
        }

        #[test]
        fn unavailable_targets_never_report_success_or_enqueue_a_partial_known_set() {
            let mut session = StreamSessionsInner::<TestSessions>::new();
            assert!(session
                .send_one_message_on_stream((&[1], 0), encoded)
                .is_err());
            let (_poll, station) = station();
            let queues = station.get_chunking_dispatch_channel();
            session.chunking_station = Some(station);
            assert!(session
                .send_one_message_on_stream((&[1], 0), encoded)
                .is_err());
            assert!(session
                .send_one_or_more_message_on_stream::<u64>(&[], encoded)
                .is_err());
            queues.insert_new_channel(0, &[1]);
            assert!(session
                .send_one_or_more_message_on_stream(&[(&[1], 0), (&[1], 4)], encoded)
                .is_err());
            assert_eq!(queues.in_queue(0, &[1]).unwrap(), 0);
            queues.insert_new_channel(4, &[1]);
            assert!(session
                .send_one_or_more_message_on_stream(&[(&[1], 0), (&[1], 4)], |ids| {
                    if ids.1 == 4 {
                        Err("encoding failure".into())
                    } else {
                        encoded(ids)
                    }
                })
                .is_err());
            assert_eq!(queues.in_queue(0, &[1]).unwrap(), 0);
        }

        #[test]
        fn successful_capsules_are_present_in_every_target_queue() {
            let (_poll, station) = station();
            let queues = station.get_chunking_dispatch_channel();
            for stream in [0, 4] {
                queues.insert_new_channel(stream, &[1]);
            }
            let mut session = StreamSessionsInner::<TestSessions>::new();
            session.chunking_station = Some(station);
            let capsules = session
                .send_one_or_more_message_on_stream(&[(&[1], 0), (&[1], 4)], encoded)
                .unwrap();
            assert_eq!(capsules.len(), 2);
            for stream in [0, 4] {
                match queues.try_pop(stream, &[1]).unwrap() {
                    QueuedRequest::StreamData(mut body) => {
                        assert_eq!(body.stream_id(), stream);
                        assert_eq!(body.scid(), &[1]);
                        assert!(body.take_data().ends_with(&[1, 2, 3]));
                    }
                    _ => panic!("expected stream payload"),
                }
                assert_eq!(queues.in_queue(stream, &[1]).unwrap(), 0);
            }
        }

        #[test]
        fn queue_disconnect_after_lookup_is_propagated() {
            let (_poll, station) = station();
            let queues = station.get_chunking_dispatch_channel();
            queues.insert_new_channel(0, &[1]);
            let mut session = StreamSessionsInner::<TestSessions>::new();
            session.chunking_station = Some(station);
            let result = session.send_one_message_on_stream((&[1], 0), |ids| {
                queues.remove_stream(ids.1, ids.0);
                encoded(ids)
            });
            assert!(result.is_err());
        }
    }
}
mod stream_sessions_traits {
    use std::any::Any;

    use bincode::{Decode, Encode};

    use crate::{
        prelude::StreamMessageCapsule, stream_sessions::UserUuid, FinishedEvent, StreamSessions,
        UserSessions,
    };

    use super::{
        stream_sessions::StreamSessionsInner,
        stream_types::{Stream, StreamBuilder, StreamIdent, StreamType},
    };

    pub trait StreamBridge<T: UserSessions<Output = T>> {
        fn user_session<U>(&self, cb: impl FnOnce(&StreamSessionsInner<T>) -> U) -> U;
    }
    pub trait StreamBridgeOps<T: UserSessions<Output = T>> {
        fn get_connection_ids_on_stream_path_by_user_id(
            &self,
            stream_path: &str,
            peer_id: UserUuid,
        ) -> Result<StreamIdent, ()>;
        fn send_one_or_more_message_on_stream<U: Decode<()> + Encode + Clone>(
            &self,
            connection_ids: &[(&[u8], u64)],
            data_cb: impl Fn(&(&[u8], u64)) -> Result<(StreamMessageCapsule<U>, Vec<u8>), String>,
        ) -> Result<Vec<StreamMessageCapsule<U>>, String>;

        fn send_one_message_on_stream<U: Decode<()> + Encode + Clone>(
            &self,
            connection_ids: (&[u8], u64),
            data_cb: impl Fn(&(&[u8], u64)) -> Result<(StreamMessageCapsule<U>, Vec<u8>), String>,
        ) -> Result<StreamMessageCapsule<U>, String>;
        fn is_connected(&self, stream_path: &str, peer_id: UserUuid) -> bool;
    }

    pub trait StreamCreation<T: UserSessions<Output = T>> {
        fn create_stream(
            &self,
            path: &str,
            stream_stype: StreamType,
            stream_config: (),
            stream_builder_cb: impl FnOnce(&mut StreamBuilder<T>),
        );
    }

    pub trait StreamHandle<T: UserSessions<Output = T>> {
        fn call(
            &self,
            event: FinishedEvent,
            user_session: &StreamSessions<T>,
            app_state: &dyn Any,
        ) -> Result<(), ()>;
    }
    impl<T, C> StreamHandle<T> for C
    where
        T: UserSessions<Output = T>,
        C: StreamHandleCallback<T>,
    {
        fn call(
            &self,
            event: FinishedEvent,
            user_session: &StreamSessions<T>,
            app_state: &dyn Any,
        ) -> Result<(), ()> {
            match app_state.downcast_ref::<C::State>() {
                Some(state) => self.callback(event, user_session, &state),
                None => Err(()),
            }
        }
    }

    pub trait StreamHandleCallback<T: UserSessions<Output = T>>: StreamHandle<T> {
        type State: Send + Sync + 'static + Any;
        fn callback(
            &self,
            event: FinishedEvent,
            user_session: &StreamSessions<T>,
            app_state: &Self::State,
        ) -> Result<(), ()>;
    }

    pub trait StreamManagement<T: UserSessions<Output = T>> {
        fn get_stream_from_path<R>(
            &self,
            path: &str,
            cb: impl FnOnce(&mut Stream<T>) -> R,
        ) -> Result<R, ()>;
        fn get_stream_session<V>(
            &self,
            cb: impl FnOnce(&StreamSessionsInner<T>) -> Result<V, String>,
        ) -> Result<V, String>;
        /// This handle method can notify an individual id
        fn send_message_to_subscriber_on_path<U: Decode<()> + Encode + Clone>(
            &self,
            user_id: UserUuid,
            message: U,
            path: &str,
        ) -> Result<Vec<StreamMessageCapsule<U>>, String>;
        fn send_message_to_subscriber_on_path_with_filtered_scids<U: Decode<()> + Encode + Clone>(
            &self,
            user_id: UserUuid,
            message: U,
            path: &str,
            scid: &[&[u8]],
        ) -> Result<Vec<StreamMessageCapsule<U>>, String>;
        fn retry_send_message_to_subscriber_on_path<U: Decode<()> + Encode + Clone>(
            &self,
            user_id: UserUuid,
            message: StreamMessageCapsule<U>,
            path: &str,
        ) -> Result<StreamMessageCapsule<U>, String>;
        fn retry_send_message_to_subscriber_on_path_with_filtered_scids<
            U: Decode<()> + Encode + Clone,
        >(
            &self,
            user_id: UserUuid,
            message: StreamMessageCapsule<U>,
            path: &str,
            scid: &[&[u8]],
        ) -> Result<StreamMessageCapsule<U>, String>;
        fn clean_closed_connexions(&self, scid: &[u8]);
    }

    pub trait ToStreamIdent {
        fn to_stream_ident(self) -> Result<StreamIdent, ()>;
    }

    mod to_stream_ident_foreign_implementations {
        use crate::stream_sessions::{stream_types::StreamIdent, UserUuid};

        use super::ToStreamIdent;

        impl ToStreamIdent for (UserUuid, Vec<u8>, u64) {
            fn to_stream_ident(
                self,
            ) -> Result<crate::stream_sessions::stream_types::StreamIdent, ()> {
                Ok(StreamIdent::new(self.0, self.1, self.2))
            }
        }
        impl ToStreamIdent for (UserUuid, &[u8], u64) {
            fn to_stream_ident(
                self,
            ) -> Result<crate::stream_sessions::stream_types::StreamIdent, ()> {
                Ok(StreamIdent::new(self.0, self.1.to_vec(), self.2))
            }
        }
    }
}

mod stream_types {
    use std::{collections::HashMap, sync::Arc};

    use bincode::{Decode, Encode};
    use serde::{Deserialize, Serialize};
    use uuid::Uuid;

    use crate::{handler_dispatcher, stream_sessions::UserUuid, MiddleWare};

    use super::{user_sessions_trait::UserSessions, StreamHandle};

    // an Id field to make possible request confirmation when client responds

    #[derive(Encode, Decode, Clone, Debug)]
    pub struct StreamMessageCapsule<T>
    where
        T: Encode + Decode<()> + Clone,
    {
        capsule_uuid: [u8; 16],
        scid: Vec<u8>,
        stream_id: u64,
        message: T,
    }

    impl<T> StreamMessageCapsule<T>
    where
        T: Encode + Decode<()> + Clone,
    {
        pub fn new(message: &T, scid: &[u8], stream_id: u64) -> Self {
            Self {
                capsule_uuid: Uuid::now_v7().into_bytes(),
                scid: scid.to_vec(),
                stream_id: stream_id,
                message: message.clone(),
            }
        }
        pub fn get_capsule_uuid(&self) -> Uuid {
            Uuid::from_bytes(self.capsule_uuid)
        }
        pub fn message(&self) -> &T {
            &self.message
        }
        pub fn get_conn_ids(&self) -> (&[u8], u64) {
            (&self.scid, self.stream_id)
        }
        pub fn get_scid(&self) -> Vec<u8> {
            self.scid.to_vec()
        }
        pub fn stream_id(&self) -> u64 {
            self.stream_id
        }
    }

    pub struct StreamIdent {
        pub user_id: UserUuid,
        pub dcid: Vec<u8>, //
        pub stream_id: u64,
    }
    impl StreamIdent {
        pub fn new(user_id: UserUuid, dcid: Vec<u8>, stream_id: u64) -> Self {
            Self {
                user_id,
                dcid,
                stream_id,
            }
        }
    }

    pub enum StreamType {
        Down,
        Up,
    }

    pub struct Stream<T: UserSessions> {
        stream_path: String,
        stream_handler: Arc<dyn StreamHandle<T> + Send + Sync + 'static>,
        middlewares: Vec<Arc<dyn MiddleWare + Send + Sync + 'static>>,
        stream_type: StreamType,
        registered_sessions: T,
    }

    pub struct StreamBuilder<T: UserSessions<Output = T>> {
        stream_path: Option<String>,
        stream_handler: Option<Arc<dyn StreamHandle<T> + Send + Sync + 'static>>,
        middlewares: Vec<Arc<dyn MiddleWare + Send + Sync + 'static>>,
        stream_type: Option<StreamType>,
    }
    impl<T: UserSessions<Output = T>> StreamBuilder<T> {
        pub fn new() -> Self {
            Self {
                stream_path: None,
                stream_handler: None,
                middlewares: vec![],
                stream_type: None,
            }
        }
        pub fn set_path(&mut self, path: &str) -> &mut Self {
            self.stream_path = Some(path.to_owned());
            self
        }
        pub fn middleware(
            &mut self,
            middleware: &Arc<dyn MiddleWare + Send + Sync + 'static>,
        ) -> &mut Self {
            self.middlewares.push(middleware.clone());
            self
        }
        fn add_global_middlewares(
            &mut self,
            entries: Vec<Arc<dyn MiddleWare + Send + Sync + 'static>>,
        ) -> &mut Self {
            let specific_mdw = std::mem::replace(&mut self.middlewares, entries);
            self.middlewares.extend(specific_mdw);
            self
        }
        pub fn stream_handler(
            &mut self,
            handler: &Arc<dyn StreamHandle<T> + Send + Sync + 'static>,
        ) -> &mut Self {
            self.stream_handler = Some(handler.clone());
            self
        }
        pub fn set_stream_type(&mut self, stream_type: StreamType) -> &mut Self {
            self.stream_type = Some(stream_type);
            self
        }
        pub fn build(mut self) -> Result<Stream<T>, ()> {
            if self.stream_path.as_ref().is_none()
                | self.stream_type.as_ref().is_none()
                | self.stream_handler.is_none()
            {
                warn!(
                    "returning error because [{:?}]",
                    (
                        self.stream_path.is_none(),
                        self.stream_type.is_none(),
                        self.stream_handler.is_none()
                    )
                );
                return Err(());
            }

            Ok(Stream {
                stream_path: self.stream_path.unwrap(),
                stream_handler: self.stream_handler.unwrap(),
                middlewares: std::mem::replace(&mut self.middlewares, vec![]),
                stream_type: self.stream_type.unwrap(),
                registered_sessions: T::new(),
            })
        }
    }
    impl<T: UserSessions<Output = T>> Stream<T> {
        pub fn to_middleware_coll(&self) -> Vec<Arc<dyn MiddleWare + Send + Sync + 'static>> {
            self.middlewares.clone()
        }
        pub fn stream_handler_callback(&self) -> &Arc<dyn StreamHandle<T> + Send + Sync + 'static> {
            &self.stream_handler
        }
        pub fn registered_sessions_mut(&mut self) -> &mut T {
            &mut self.registered_sessions
        }
        pub fn registered_sessions(&self) -> &T {
            &self.registered_sessions
        }
    }

    pub fn stream_cleaning<T: UserSessions>(map: &mut HashMap<String, Stream<T>>, scid: &[u8]) {
        for (path, stream) in map.iter_mut() {
            let user_id = stream
                .registered_sessions
                .remove_sessions_by_connection(scid);
        }
    }
}

mod user_sessions_trait {
    use std::fmt::Debug;

    use uuid::Uuid;

    use crate::stream_sessions::UserUuid;

    use super::ToStreamIdent;

    pub trait UserSessions: Send + Sync + 'static {
        type Output;
        fn new() -> Self::Output;
        fn user_sessions(&self) -> &Self::Output;
        fn get_connection_ids_on_user_ids(&self, keys: &[UserUuid]) -> Vec<impl ToStreamIdent>;
        fn get_all_connections(&self) -> Vec<impl ToStreamIdent + Debug>;
        fn register_sessions(&mut self, user_id: UserUuid, conn_ids: (Vec<u8>, u64));
        fn remove_sessions_by_connection(&mut self, conn_id: &[u8]) -> Option<UserUuid>;
        //        fn remove_sessions_by_user_id(&mut self, conn_id: User) -> Option<UserUuid>;
        fn is_user_connected(&self, user_id: UserUuid) -> bool;
    }
}

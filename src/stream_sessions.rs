pub use stream_sessions::StreamSessions;
pub use stream_sessions_traits::*;
pub use stream_types::{StreamBuilder, StreamType};
pub use user_sessions_trait::UserSessions;
mod stream_sessions {
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    use super::{
        stream_types::Stream, user_sessions_trait::UserSessions, StreamBuilder, StreamCreation,
    };

    type StreamPath = String;

    /// # Streams register
    ///
    /// StreamSessions registers sesssions routes opened via the router
    /// and keeps track of the users ids (quic dcid and stream id (u64))
    ///
    pub struct StreamSessions<S: Send + Sync + 'static, T: UserSessions> {
        inner: Arc<Mutex<StreamSessionsInner<S, T>>>,
    }

    impl<S: Send + Sync + 'static, T: UserSessions> StreamSessions<S, T> {
        pub fn new() -> Self {
            Self {
                inner: Arc::new(Mutex::new(StreamSessionsInner::new())),
            }
        }

        fn mut_access(&self, cb: impl FnOnce(&mut StreamSessionsInner<S, T>)) {
            let guard = &mut *self.inner.lock().unwrap();
            cb(guard);
        }
    }

    struct StreamSessionsInner<S: Send + Sync + 'static, T: UserSessions> {
        sessions: HashMap<StreamPath, Stream<S, T>>,
    }

    impl<S: Send + Sync + 'static, T: UserSessions> StreamSessionsInner<S, T> {
        fn new() -> Self {
            Self {
                sessions: HashMap::new(),
            }
        }
    }

    impl<S: Send + Sync + 'static, T: UserSessions<Output = T>> StreamCreation<S, T>
        for StreamSessions<S, T>
    {
        fn create_stream(
            &self,
            path: &str,
            stream_type: super::stream_types::StreamType,
            stream_config: (),
            stream_builder_cb: impl FnOnce(&mut StreamBuilder<S, T>),
        ) {
            let mut stream_builder = StreamBuilder::new();

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
}
mod stream_sessions_traits {
    use crate::{FinishedEvent, UserSessions};

    use super::stream_types::{StreamBuilder, StreamIdent, StreamType};

    pub trait StreamCreation<S: Send + Sync + 'static, T: UserSessions<Output = T>> {
        fn create_stream(
            &self,
            path: &str,
            stream_stype: StreamType,
            stream_config: (),
            stream_builder_cb: impl FnOnce(&mut StreamBuilder<S, T>),
        );
    }

    pub trait StreamHandle<S: Send + Sync + 'static, T: UserSessions<Output = T>> {
        fn call(&self, event: FinishedEvent, user_session: &T, app_state: &S) -> Result<(), ()>;
    }

    pub trait StreamManagement {
        fn add_user();
    }

    pub trait ToStreamIdent {
        fn to_stream_ident(self) -> Result<StreamIdent, ()>;
    }

    mod to_stream_ident_foreign_implementations {
        use crate::stream_sessions::stream_types::StreamIdent;

        use super::ToStreamIdent;

        impl ToStreamIdent for (Vec<u8>, u64) {
            fn to_stream_ident(
                self,
            ) -> Result<crate::stream_sessions::stream_types::StreamIdent, ()> {
                Ok(StreamIdent::new(self.0, self.1))
            }
        }
        impl ToStreamIdent for (&[u8], u64) {
            fn to_stream_ident(
                self,
            ) -> Result<crate::stream_sessions::stream_types::StreamIdent, ()> {
                Ok(StreamIdent::new(self.0.to_vec(), self.1))
            }
        }
    }
}

mod stream_types {
    use std::{collections::HashMap, sync::Arc};

    use crate::{handler_dispatcher, MiddleWare};

    use super::{user_sessions_trait::UserSessions, StreamHandle};

    pub struct StreamIdent {
        dcid: Vec<u8>, //
        stream_id: u64,
    }
    impl StreamIdent {
        pub fn new(dcid: Vec<u8>, stream_id: u64) -> Self {
            Self { dcid, stream_id }
        }
    }

    pub enum StreamType {
        Down,
        Up,
    }

    pub struct Stream<S: Send + Sync + 'static, T: UserSessions> {
        stream_path: String,
        stream_handler: Arc<dyn StreamHandle<S, T> + Send + Sync + 'static>,
        middlewares: Vec<Arc<dyn MiddleWare<S> + Send + Sync + 'static>>,
        stream_type: StreamType,
        registered_sessions: T,
    }

    pub struct StreamBuilder<S: Send + Sync + 'static, T: UserSessions<Output = T>> {
        stream_path: Option<String>,
        stream_handler: Option<Arc<dyn StreamHandle<S, T> + Send + Sync + 'static>>,
        middlewares: Vec<Arc<dyn MiddleWare<S> + Send + Sync + 'static>>,
        stream_type: Option<StreamType>,
    }
    impl<S: Send + Sync + 'static, T: UserSessions<Output = T>> StreamBuilder<S, T> {
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
            middleware: &Arc<dyn MiddleWare<S> + Send + Sync + 'static>,
        ) -> &mut Self {
            self.middlewares.push(middleware.clone());
            self
        }
        fn add_global_middlewares(
            &mut self,
            entries: Vec<Arc<dyn MiddleWare<S> + Send + Sync + 'static>>,
        ) -> &mut Self {
            let specific_mdw = std::mem::replace(&mut self.middlewares, entries);
            self.middlewares.extend(specific_mdw);
            self
        }
        pub fn stream_handler(
            &mut self,
            handler: &Arc<dyn StreamHandle<S, T> + Send + Sync + 'static>,
        ) -> &mut Self {
            self.stream_handler = Some(handler.clone());
            self
        }
        pub fn set_stream_type(&mut self, stream_type: StreamType) -> &mut Self {
            self.stream_type = Some(stream_type);
            self
        }
        pub fn build(mut self) -> Result<Stream<S, T>, ()> {
            if self.stream_path.as_ref().is_none()
                | self.stream_type.as_ref().is_none()
                | self.stream_handler.is_none()
            {
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
}

mod user_sessions_trait {
    use super::ToStreamIdent;

    pub trait UserSessions: Send + Sync + 'static {
        type Output;
        type Key;
        fn new() -> Self::Output;
        fn user_sessions(&self) -> &Self::Output;
        fn broadcast_to_streams(&self, keys: &[Self::Key]) -> Vec<impl ToStreamIdent>;
    }
}

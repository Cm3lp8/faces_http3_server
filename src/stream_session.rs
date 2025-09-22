pub use stream_sessions::StreamSessions;
mod stream_sessions {

    /// # Streams register
    ///
    /// StreamSession can be used as a stream register
    /// that keep track of differents stream_routes opened
    /// and collections of clients that are currently connceted
    pub struct StreamSessions;

    impl StreamSessions {
        pub fn new() -> Self {
            Self
        }
    }
}

mod stream_sessions_traits {}

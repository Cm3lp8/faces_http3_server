pub use conn_stats_data::ConnStats;

mod conn_stats_data {
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };

    #[derive(Debug)]
    pub struct ConnStats {
        inner: Arc<Mutex<ConnStatsInner>>,
    }

    #[derive(Debug)]
    pub struct ConnStatsInner {
        rtt: Duration,
        cwnd: usize,
    }

    impl ConnStats {
        pub fn new() -> Self {
            Self {
                inner: Arc::new(Mutex::new(ConnStatsInner {
                    rtt: Duration::from_millis(2),
                    cwnd: 0,
                })),
            }
        }
        pub fn set_conn_stats(&self, rtt: Duration, cwnd: usize) {
            let guard = &mut *self.inner.lock().unwrap();
            guard.cwnd = cwnd;
            guard.rtt = rtt;
        }
        pub fn stats(&self) -> (Duration, usize) {
            let guard = self.inner.lock().unwrap();

            (guard.rtt, guard.cwnd)
        }
    }

    impl Clone for ConnStats {
        fn clone(&self) -> Self {
            Self {
                inner: self.inner.clone(),
            }
        }
    }
}

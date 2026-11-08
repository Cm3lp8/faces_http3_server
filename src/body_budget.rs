//! Accounting for request bodies retained by the HTTP/3 receive and dispatch paths.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Copy)]
pub struct BodyLimits {
    pub default_body_bytes: usize,
    pub connection_bytes: usize,
    pub server_bytes: usize,
    pub connection_requests: usize,
    pub server_requests: usize,
    pub receive_timeout: Duration,
}

impl Default for BodyLimits {
    fn default() -> Self {
        Self {
            default_body_bytes: 1 << 20,
            connection_bytes: 8 << 20,
            server_bytes: 64 << 20,
            connection_requests: 32,
            server_requests: 512,
            receive_timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Default)]
struct ConnectionUsage {
    bytes: usize,
    streams: HashMap<u64, StreamUsage>,
}

struct StreamUsage {
    held: usize,
    deadline: Option<Instant>,
}

#[derive(Default)]
struct Usage {
    connections: HashMap<String, ConnectionUsage>,
    bytes: usize,
    requests: usize,
}

pub(crate) struct BodyBudgets {
    limits: BodyLimits,
    usage: Mutex<Usage>,
}

impl BodyBudgets {
    pub(crate) fn new(limits: BodyLimits) -> Self {
        Self {
            limits,
            usage: Mutex::new(Usage::default()),
        }
    }

    // A disk-backed route can have an arbitrarily long declared body. Its
    // prefix still obeys the memory limits while authorization is pending.
    pub(crate) fn admit(
        self: &Arc<Self>,
        connection: &str,
        stream: u64,
        disk_backed: bool,
        route_limit: Option<usize>,
        declared: Option<usize>,
        now: Instant,
    ) -> Result<Arc<BodyLease>, ()> {
        let memory_limit = route_limit.unwrap_or(self.limits.default_body_bytes);
        let body_limit = route_limit.or_else(|| (!disk_backed).then_some(memory_limit));
        if declared
            .zip(body_limit)
            .is_some_and(|(length, limit)| length > limit)
        {
            return Err(());
        }
        let mut usage = self.usage.lock().unwrap();
        if usage.requests >= self.limits.server_requests || self.limits.connection_requests == 0 {
            return Err(());
        }
        let conn = usage.connections.entry(connection.to_owned()).or_default();
        if conn.streams.len() >= self.limits.connection_requests
            || conn.streams.contains_key(&stream)
        {
            return Err(());
        }
        conn.streams.insert(
            stream,
            StreamUsage {
                held: 0,
                deadline: now.checked_add(self.limits.receive_timeout),
            },
        );
        usage.requests += 1;
        Ok(Arc::new(BodyLease {
            budgets: self.clone(),
            connection: connection.to_owned(),
            stream,
            memory_limit,
            body_limit,
        }))
    }

    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        let usage = self.usage.lock().unwrap();
        usage
            .connections
            .values()
            .flat_map(|conn| conn.streams.values())
            .filter_map(|s| s.deadline)
            .min()
    }

    pub(crate) fn expired(&self, now: Instant) -> Vec<(String, u64)> {
        let usage = self.usage.lock().unwrap();
        let mut expired = Vec::new();
        for (id, connection) in &usage.connections {
            for (&stream, body) in &connection.streams {
                if body.deadline.is_some_and(|deadline| now >= deadline) {
                    expired.push((id.clone(), stream));
                }
            }
        }
        expired
    }
}

pub(crate) struct BodyLease {
    budgets: Arc<BodyBudgets>,
    connection: String,
    stream: u64,
    memory_limit: usize,
    body_limit: Option<usize>,
}

impl BodyLease {
    fn check_length(&self, received: usize, packet: usize) -> Result<(), ()> {
        let total = received.checked_add(packet).ok_or(())?;
        if self.body_limit.is_some_and(|limit| total > limit) {
            return Err(());
        }
        Ok(())
    }

    // Charge before allocating. Vec capacity can exceed its current length.
    fn reserve(
        &self,
        minimum: usize,
        preferred: usize,
        memory_buffer: bool,
        now: Instant,
    ) -> Result<usize, ()> {
        let mut usage = self.budgets.usage.lock().unwrap();
        let server_room = self.budgets.limits.server_bytes.saturating_sub(usage.bytes);
        let conn = usage.connections.get_mut(&self.connection).ok_or(())?;
        let body = conn.streams.get_mut(&self.stream).ok_or(())?;
        if body.deadline.is_some_and(|deadline| now >= deadline) {
            return Err(());
        }
        let mut room = server_room.min(
            self.budgets
                .limits
                .connection_bytes
                .saturating_sub(conn.bytes),
        );
        if memory_buffer {
            room = room.min(self.memory_limit.saturating_sub(body.held));
        }
        if minimum > room {
            return Err(());
        }
        let grant = preferred.min(room);
        if memory_buffer {
            body.held += grant;
        } else {
            body.deadline = now.checked_add(self.budgets.limits.receive_timeout);
        }
        conn.bytes += grant;
        usage.bytes += grant;
        Ok(grant)
    }

    pub(crate) fn reserve_buffer(
        &self,
        data: &mut Vec<u8>,
        received: usize,
        length: usize,
        now: Instant,
    ) -> Result<(), ()> {
        self.check_length(received, length)?;
        let needed = data.len().checked_add(length).ok_or(())?;
        let capacity = data.capacity();
        let extra = needed.saturating_sub(capacity);
        let preferred = if extra == 0 { 0 } else { extra.max(capacity) };
        let grant = self.reserve(extra, preferred, true, now)?;
        if grant != 0 {
            data.try_reserve_exact(capacity + grant - data.len())
                .map_err(|_| ())?;
        }
        Ok(())
    }

    pub(crate) fn queue_file_packet(
        self: &Arc<Self>,
        received: usize,
        length: usize,
        now: Instant,
    ) -> Result<Arc<FilePacket>, ()> {
        self.check_length(received, length)?;
        self.reserve(length, length, false, now)?;
        Ok(Arc::new(FilePacket {
            lease: self.clone(),
            bytes: length,
        }))
    }

    pub(crate) fn receive_stream(
        &self,
        received: usize,
        length: usize,
        now: Instant,
    ) -> Result<(), ()> {
        self.check_length(received, length)?;
        self.reserve(0, 0, true, now)?;
        Ok(())
    }

    pub(crate) fn clear_buffered(&self, retired: bool) {
        let mut usage = self.budgets.usage.lock().unwrap();
        let conn = usage.connections.get_mut(&self.connection).unwrap();
        let body = conn.streams.get_mut(&self.stream).unwrap();
        let freed = std::mem::take(&mut body.held);
        if retired {
            body.deadline = None;
        }
        conn.bytes -= freed;
        usage.bytes -= freed;
    }

    pub(crate) fn finish_receiving(&self, now: Instant) -> bool {
        let mut usage = self.budgets.usage.lock().unwrap();
        let conn = usage.connections.get_mut(&self.connection).unwrap();
        let body = conn.streams.get_mut(&self.stream).unwrap();
        if body.deadline.is_some_and(|deadline| now >= deadline) {
            return false;
        }
        body.deadline = None;
        true
    }
}

// Clones of a queued write share its payload and its charge. The final
// consumer releases it even when the queue closes or a writer fails.
pub(crate) struct FilePacket {
    lease: Arc<BodyLease>,
    bytes: usize,
}
impl Drop for FilePacket {
    fn drop(&mut self) {
        let mut usage = self.lease.budgets.usage.lock().unwrap();
        let conn = usage.connections.get_mut(&self.lease.connection).unwrap();
        conn.bytes -= self.bytes;
        usage.bytes -= self.bytes;
    }
}
impl Drop for BodyLease {
    fn drop(&mut self) {
        let mut usage = self.budgets.usage.lock().unwrap();
        let conn = usage.connections.get_mut(&self.connection).unwrap();
        let body = conn.streams.remove(&self.stream).unwrap();
        conn.bytes -= body.held;
        if conn.streams.is_empty() {
            usage.connections.remove(&self.connection);
        }
        usage.bytes -= body.held;
        usage.requests -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> BodyLimits {
        BodyLimits {
            default_body_bytes: 4,
            connection_bytes: 6,
            server_bytes: 8,
            connection_requests: 2,
            server_requests: 3,
            receive_timeout: Duration::from_secs(1),
        }
    }

    fn append(lease: &BodyLease, body: &mut Vec<u8>, count: usize, now: Instant) -> Result<(), ()> {
        lease.reserve_buffer(body, body.len(), count, now)?;
        body.resize(body.len() + count, 42);
        Ok(())
    }
    #[test]
    fn enforces_actual_bytes_across_streams_and_connections_and_releases_on_drop() {
        let budgets = Arc::new(BodyBudgets::new(limits()));
        let now = Instant::now();
        assert!(budgets.admit("a", 0, false, None, Some(5), now).is_err());
        let a = budgets.admit("a", 0, false, None, Some(4), now).unwrap();
        let b = budgets.admit("a", 4, false, None, None, now).unwrap();
        assert!(budgets.admit("a", 8, false, None, None, now).is_err());
        let c = budgets.admit("b", 0, false, None, None, now).unwrap();
        assert!(budgets.admit("c", 0, false, None, None, now).is_err());
        let (mut a_buf, mut b_buf, mut c_buf) = (vec![], vec![], vec![]);
        append(&a, &mut a_buf, 2, now).unwrap();
        append(&a, &mut a_buf, 2, now).unwrap();
        assert!(append(&a, &mut a_buf, 1, now).is_err());
        append(&b, &mut b_buf, 2, now).unwrap();
        assert!(append(&b, &mut b_buf, 1, now).is_err());
        append(&c, &mut c_buf, 2, now).unwrap();
        assert!(append(&c, &mut c_buf, 1, now).is_err());
        assert!(a.finish_receiving(now)); // FIN does not uncharge a queued body.
        assert!(append(&c, &mut c_buf, 1, now).is_err());
        drop(a);
        append(&b, &mut b_buf, 2, now).unwrap();
        append(&c, &mut c_buf, 2, now).unwrap();
        drop((b, c));
        let usage = budgets.usage.lock().unwrap();
        assert_eq!((usage.bytes, usage.requests), (0, 0));
        assert!(usage.connections.is_empty());
    }

    #[test]
    fn deadline_is_absolute_and_file_prefix_is_bounded_until_spooled() {
        let budgets = Arc::new(BodyBudgets::new(limits()));
        let now = Instant::now();
        let file = budgets.admit("a", 0, true, None, Some(500), now).unwrap();
        let mut prefix = vec![];
        append(&file, &mut prefix, 4, now).unwrap();
        assert!(append(&file, &mut prefix, 1, now).is_err());
        drop(prefix);
        file.clear_buffered(false);
        let queued = file
            .queue_file_packet(4, 6, now + Duration::from_millis(900))
            .unwrap();
        assert!(file
            .queue_file_packet(10, 1, now + Duration::from_millis(901))
            .is_err());
        let memory = budgets.admit("a", 4, false, Some(2), None, now).unwrap();
        assert!(append(&memory, &mut vec![], 1, now).is_err());
        drop(queued);
        append(&memory, &mut vec![], 1, now + Duration::from_millis(900)).unwrap();
        assert_eq!(
            budgets.expired(now + Duration::from_secs(1)),
            vec![("a".into(), 4)]
        );
        assert!(!memory.finish_receiving(now + Duration::from_secs(1)));
        assert!(file
            .queue_file_packet(10, 1, now + Duration::from_secs(2))
            .is_err());
        drop((memory, file));
        assert!(budgets.next_deadline().is_none());
    }
}

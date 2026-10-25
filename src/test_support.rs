use crate::UserSessions;
use uuid::Uuid;

pub struct TestSessions;
impl UserSessions for TestSessions {
    type Output = Self;
    fn new() -> Self {
        Self
    }
    fn user_sessions(&self) -> &Self {
        self
    }
    fn is_user_connected(&self, _: Uuid) -> bool {
        false
    }
    fn get_connection_ids_on_user_ids(&self, _: &[Uuid]) -> Vec<impl crate::ToStreamIdent> {
        Vec::<(Uuid, Vec<u8>, u64)>::new()
    }
    fn get_all_connections(&self) -> Vec<impl crate::ToStreamIdent + std::fmt::Debug> {
        Vec::<(Uuid, Vec<u8>, u64)>::new()
    }
    fn register_sessions(&mut self, _: Uuid, _: (Vec<u8>, u64)) {}
    fn remove_sessions_by_connection(&mut self, _: &[u8]) -> Option<Uuid> {
        None
    }
}

#[macro_use]
extern crate log;
pub use server_config::{RequestForm, RequestManager, RequestType, ServerConfig};
mod server_config;
mod server_init;
#[cfg(test)]
mod tests {
    use super::*;

    /*
    #[test]
    fn it_works() {
        let result = add(2, 2);
        assert_eq!(result, 4);
    }
    */
}

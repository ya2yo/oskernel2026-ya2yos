macro_rules! env_or_default {
    ($key:literal, $default:literal) => {
        match option_env!($key) {
            Some(val) => val,
            None => $default,
        }
    };
}

pub const IP: &str = env_or_default!("AX_IP", "10.0.2.15");
pub const GATEWAY: &str = env_or_default!("AX_GW", "10.0.2.2");
pub const IP_PREFIX: u8 = 24;

pub const STANDARD_MTU: usize = 1500;

// A local TCP segment is emitted as IPv4 fragments and reassembled before it
// reaches a loopback peer. This keeps the Router and physical NIC at 1500 MTU
// while ensuring a local HTTP request is delivered as one TCP segment.
pub const LOOPBACK_TCP_MSS: usize = 4096;

pub const TCP_RX_BUF_LEN: usize = 64 * 1024;
pub const TCP_TX_BUF_LEN: usize = 64 * 1024;
pub const UDP_RX_BUF_LEN: usize = 64 * 1024;
pub const UDP_TX_BUF_LEN: usize = 64 * 1024;
pub const LISTEN_QUEUE_SIZE: usize = 512;

pub const SOCKET_BUFFER_SIZE: usize = 64;
pub const ETHERNET_MAX_PENDING_PACKETS: usize = 32;

use user_lib::{
    bind, close, get_time, println, recv_from, send_to, sleep, socket, SockAddrIn, AF_INET,
    SOCK_DGRAM, SOCK_NONBLOCK,
};

const LOOPBACK: [u8; 4] = [127, 0, 0, 1];
const SLIRP_DNS: [u8; 4] = [10, 0, 2, 3];
const SLIRP_GATEWAY: [u8; 4] = [10, 0, 2, 2];
const DNS_PORT: u16 = 53;
const EAGAIN: isize = -11;
const EINTR: isize = -4;
const ETIMEDOUT: isize = -110;
const DNS_TIMEOUT_MS: usize = 3_000;
// The kernel queue has 128 entries; crossing that boundary exposes leaked descriptors.
const STRESS_ROUNDS: usize = 160;

#[derive(Clone, Copy)]
struct Failure {
    stage: &'static str,
    code: isize,
}

type TestResult = Result<(), Failure>;

impl Failure {
    const fn new(stage: &'static str, code: isize) -> Self {
        Self { stage, code }
    }
}

struct SocketFd(usize);

impl SocketFd {
    fn udp_nonblocking() -> Result<Self, Failure> {
        let fd = socket(AF_INET, SOCK_DGRAM | SOCK_NONBLOCK, 0);
        if fd < 0 {
            Err(Failure::new("socket", fd))
        } else {
            Ok(Self(fd as usize))
        }
    }

    const fn raw(&self) -> usize {
        self.0
    }
}

impl Drop for SocketFd {
    fn drop(&mut self) {
        let _ = close(self.0);
    }
}

fn send_datagram(fd: &SocketFd, data: &[u8], destination: &SockAddrIn) -> TestResult {
    let sent = send_to(fd.raw(), data, 0, destination);
    if sent == data.len() as isize {
        Ok(())
    } else {
        Err(Failure::new("sendto", sent))
    }
}

fn recv_until<'a>(
    fd: &SocketFd,
    buf: &'a mut [u8],
    timeout_ms: usize,
) -> Result<(&'a [u8], SockAddrIn), Failure> {
    let started = get_time();
    loop {
        let mut source = SockAddrIn::new([0, 0, 0, 0], 0);
        let received = recv_from(fd.raw(), buf, 0, &mut source);
        if received >= 0 {
            return Ok((&buf[..received as usize], source));
        }
        if received != EAGAIN && received != EINTR {
            return Err(Failure::new("recvfrom", received));
        }
        if get_time().saturating_sub(started) >= timeout_ms {
            return Err(Failure::new("receive timeout", ETIMEDOUT));
        }
        sleep(2);
    }
}

fn udp_loopback_baseline() -> TestResult {
    const PAYLOAD: &[u8] = b"Ya2yOS UDP loopback baseline";
    let receiver = SocketFd::udp_nonblocking()?;
    let sender = SocketFd::udp_nonblocking()?;
    let destination = SockAddrIn::new(LOOPBACK, 41_000);

    let ret = bind(receiver.raw(), &destination);
    if ret != 0 {
        return Err(Failure::new("bind loopback", ret));
    }
    send_datagram(&sender, PAYLOAD, &destination)?;

    let mut response = [0u8; 128];
    let (payload, source) = recv_until(&receiver, &mut response, 1_000)?;
    if payload != PAYLOAD {
        return Err(Failure::new("loopback payload mismatch", -1_001));
    }
    if source.ip() != LOOPBACK || source.port() == 0 {
        return Err(Failure::new("loopback source mismatch", -1_002));
    }
    Ok(())
}

fn build_dns_query(id: u16, query: &mut [u8; 32]) -> usize {
    query.fill(0);
    query[0..2].copy_from_slice(&id.to_be_bytes());
    query[2] = 0x01; // recursion desired
    query[5] = 0x01; // one question
    query[12] = 9;
    query[13..22].copy_from_slice(b"localhost");
    query[22] = 0;
    query[23..25].copy_from_slice(&1u16.to_be_bytes()); // A
    query[25..27].copy_from_slice(&1u16.to_be_bytes()); // IN
    27
}

fn recv_dns_response(fd: &SocketFd, expected_id: u16, timeout_ms: usize) -> TestResult {
    let started = get_time();
    let mut response = [0u8; 512];

    loop {
        let elapsed = get_time().saturating_sub(started);
        if elapsed >= timeout_ms {
            return Err(Failure::new("DNS receive timeout", ETIMEDOUT));
        }
        let (packet, source) = recv_until(fd, &mut response, timeout_ms - elapsed)?;
        if packet.len() < 12 {
            return Err(Failure::new("short DNS response", packet.len() as isize));
        }
        if u16::from_be_bytes([packet[0], packet[1]]) != expected_id {
            continue;
        }
        if packet[2] & 0x80 == 0 {
            return Err(Failure::new("DNS QR bit missing", -1_003));
        }
        if source.ip() != SLIRP_DNS || source.port() != DNS_PORT {
            return Err(Failure::new("DNS source mismatch", -1_004));
        }
        return Ok(());
    }
}

fn dns_round_trip_once() -> TestResult {
    let socket = SocketFd::udp_nonblocking()?;
    let dns = SockAddrIn::new(SLIRP_DNS, DNS_PORT);
    let mut query = [0u8; 32];
    let id = 0x5a01;
    let query_len = build_dns_query(id, &mut query);

    send_datagram(&socket, &query[..query_len], &dns)?;
    recv_dns_response(&socket, id, DNS_TIMEOUT_MS)
}

fn dns_descriptor_recycling_stress() -> TestResult {
    let socket = SocketFd::udp_nonblocking()?;
    let dns = SockAddrIn::new(SLIRP_DNS, DNS_PORT);
    let mut query = [0u8; 32];

    for round in 0..STRESS_ROUNDS {
        let id = 0xa000u16.wrapping_add(round as u16);
        let query_len = build_dns_query(id, &mut query);
        send_datagram(&socket, &query[..query_len], &dns)?;
        recv_dns_response(&socket, id, DNS_TIMEOUT_MS)?;
        if (round + 1) % 32 == 0 {
            println!(
                "TINFO: completed {} / {} DNS round trips",
                round + 1,
                STRESS_ROUNDS
            );
        }
    }
    Ok(())
}

fn nonblocking_timeout_path() -> TestResult {
    const PAYLOAD: &[u8] = b"Ya2yOS timeout probe";
    let socket = SocketFd::udp_nonblocking()?;
    let closed_service = SockAddrIn::new(SLIRP_GATEWAY, 9);
    send_datagram(&socket, PAYLOAD, &closed_service)?;

    let mut response = [0u8; 64];
    match recv_until(&socket, &mut response, 250) {
        Err(Failure {
            code: ETIMEDOUT, ..
        }) => Ok(()),
        Err(error) => Err(error),
        Ok(_) => Err(Failure::new("unexpected timeout-probe response", -1_005)),
    }
}

fn run_case(name: &str, test: fn() -> TestResult, passed: &mut usize, failed: &mut usize) {
    match test() {
        Ok(()) => {
            *passed += 1;
            println!("TPASS: {}", name);
        }
        Err(error) => {
            *failed += 1;
            println!(
                "TFAIL: {}: stage=\"{}\", code={}",
                name, error.stage, error.code
            );
        }
    }
}
#[allow(unused)]
pub fn run_all() -> i32 {
    let mut passed = 0;
    let mut failed = 0;

    println!("#### NETDEV TEST START ####");
    println!("TINFO: loopback is a protocol-stack baseline and does not exercise eth0");
    run_case(
        "udp_loopback_baseline",
        udp_loopback_baseline,
        &mut passed,
        &mut failed,
    );

    #[cfg(target_arch = "loongarch64")]
    println!("TINFO: remaining cases route through eth0 using VirtIO PCI");
    #[cfg(not(target_arch = "loongarch64"))]
    println!("TINFO: remaining cases route through eth0 using the VirtIO transport");
    run_case(
        "dns_round_trip_once",
        dns_round_trip_once,
        &mut passed,
        &mut failed,
    );
    run_case(
        "dns_descriptor_recycling_stress",
        dns_descriptor_recycling_stress,
        &mut passed,
        &mut failed,
    );
    run_case(
        "nonblocking_timeout_path",
        nonblocking_timeout_path,
        &mut passed,
        &mut failed,
    );

    println!("Summary: netdev passed {} failed {}", passed, failed);
    println!("#### NETDEV TEST END ####");
    if failed == 0 {
        0
    } else {
        1
    }
}

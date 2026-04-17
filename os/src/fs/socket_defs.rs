use num_enum::FromPrimitive;

#[derive(Debug, PartialEq, FromPrimitive)]
#[repr(u32)]
pub enum SocketDomain {
    Unix = 1,      //      Local communication
    Inet = 2,      //      IPv4 Internet protocols
    Ax25 = 3,      //      Amateur radio AX.25 protocol
    Ipx = 4,       //       IPX - Novell protocols
    Appletalk = 5, // AppleTalk
    X25 = 9,       //       ITU-T X.25 / ISO/IEC 8208 protocol
    Inet6 = 10,    //    IPv6 Internet protocols
    Decnet = 12,   //    DECet protocol sockets
    Key = 15,      //     Key management protocol, originally developed for usage with IPsec
    Netlink = 16,  //  Kernel user interface device
    Packet = 17,   //   Low-level packet interface
    Rds = 21,      //   Reliable Datagram Sockets (RDS) protocol
    Pppox = 24,    //  Generic PPP transport layer, for setting up L2 tunnels (L2TP and PPPoE)
    Llc = 26,      //     Logical link control (IEEE 802.2 LLC) protocol
    // AF_IB,        //      InfiniBand native addressing
    // AF_MPLS,      //    Multiprotocol Label Switching
    Can = 29,       //     Controller Area Network automotive bus protocol
    Tipc = 30,      //   TIPC, "cluster domain sockets" protocol
    Bluetooth = 31, //Bluetooth low-level socket protocol
    Alg = 38,       //    Interface to kernel crypto API
    Vsock = 40, //    VSOCK (originally "VMWare VSockets")   protocol for hypervisor-guestcommunication
    // AF_KCM,   //    KCM (kernel connection multiplexer) interface
    // AF_XDP,   //      XDP (express data path) interface
    #[num_enum(default)]
    Default = 0, // 一个虚假的domain号
}

pub struct SockAddrUnix {
    family: u16,
    path: [u8; 108],
}

#[derive(Clone, Copy)]
#[repr(C)]
pub struct SockAddrInet {
    pub family: u16,
    pub port: u16,
    pub addr: u32,
}

use user_lib::{exit, fork, msgctl, msgget, msgrcv, msgsnd, println, sleep, waitpid};

const IPC_PRIVATE: i32 = 0;
const IPC_CREAT: i32 = 0o1000;
const IPC_NOWAIT: i32 = 0o4000;
const IPC_RMID: i32 = 0;
const IPC_SET: i32 = 1;
const IPC_STAT: i32 = 2;
const MSG_COPY: i32 = 0o40000;
const MSG_NOERROR: i32 = 0o10000;

const ENOMSG: isize = -42;
const EIDRM: isize = -43;
const TEXT_SIZE: usize = 8;

#[repr(C)]
struct MsgBuf {
    mtype: isize,
    text: [u8; 8],
}

impl MsgBuf {
    const fn new(mtype: isize, text: [u8; 8]) -> Self {
        Self { mtype, text }
    }
}

fn expect(condition: bool, message: &str) -> bool {
    if !condition {
        println!("msg regression failed: {}", message);
    }
    condition
}

fn send(msqid: i32, message: &MsgBuf) -> bool {
    msgsnd(
        msqid,
        message as *const MsgBuf as *const u8,
        message.text.len(),
        0,
    ) == 0
}

fn recv(msqid: i32, message: &mut MsgBuf, msgsz: usize, msgtyp: isize, flags: i32) -> isize {
    msgrcv(
        msqid,
        message as *mut MsgBuf as *mut u8,
        msgsz,
        msgtyp,
        flags,
    )
}

pub fn run() -> bool {
    let msqid = msgget(IPC_PRIVATE, IPC_CREAT | 0o600);
    if !expect(msqid > 0, "msgget") {
        return false;
    }
    let msqid = msqid as i32;

    let type_two = MsgBuf::new(2, *b"type-two");
    let type_one = MsgBuf::new(1, *b"type-one");
    if !expect(send(msqid, &type_two) && send(msqid, &type_one), "msgsnd") {
        let _ = msgctl(msqid, IPC_RMID, core::ptr::null_mut());
        return false;
    }

    let mut received = MsgBuf::new(0, [0; 8]);
    let selected = recv(msqid, &mut received, TEXT_SIZE, -2, 0);
    if !expect(
        selected == 8 && received.mtype == 1 && received.text == *b"type-one",
        "negative mtype selection",
    ) {
        let _ = msgctl(msqid, IPC_RMID, core::ptr::null_mut());
        return false;
    }

    received = MsgBuf::new(0, [0; 8]);
    let copied = recv(msqid, &mut received, TEXT_SIZE, 0, IPC_NOWAIT | MSG_COPY);
    if !expect(
        copied == 8 && received.mtype == 2 && received.text == *b"type-two",
        "MSG_COPY",
    ) {
        let _ = msgctl(msqid, IPC_RMID, core::ptr::null_mut());
        return false;
    }

    received = MsgBuf::new(0, [0; 8]);
    if !expect(
        recv(msqid, &mut received, TEXT_SIZE, 2, 0) == 8 && received.text == *b"type-two",
        "MSG_COPY preserves queue",
    ) {
        let _ = msgctl(msqid, IPC_RMID, core::ptr::null_mut());
        return false;
    }

    let long_message = MsgBuf::new(3, *b"truncate");
    if !expect(send(msqid, &long_message), "send truncation message") {
        let _ = msgctl(msqid, IPC_RMID, core::ptr::null_mut());
        return false;
    }
    received = MsgBuf::new(0, [0; 8]);
    if !expect(
        recv(msqid, &mut received, 3, 3, MSG_NOERROR) == 3
            && received.mtype == 3
            && received.text[..3] == *b"tru",
        "MSG_NOERROR truncation",
    ) {
        let _ = msgctl(msqid, IPC_RMID, core::ptr::null_mut());
        return false;
    }

    let stat_message = MsgBuf::new(4, *b"stat-msg");
    if !expect(send(msqid, &stat_message), "send stat message") {
        let _ = msgctl(msqid, IPC_RMID, core::ptr::null_mut());
        return false;
    }
    let mut stat = [0usize; 15];
    if !expect(
        msgctl(msqid, IPC_STAT, stat.as_mut_ptr() as *mut u8) == 0
            && stat[9] == stat_message.text.len()
            && stat[10] == 1,
        "IPC_STAT queue accounting",
    ) {
        let _ = msgctl(msqid, IPC_RMID, core::ptr::null_mut());
        return false;
    }
    stat[11] = 1024;
    if !expect(
        msgctl(msqid, IPC_SET, stat.as_mut_ptr() as *mut u8) == 0,
        "IPC_SET msg_qbytes",
    ) {
        let _ = msgctl(msqid, IPC_RMID, core::ptr::null_mut());
        return false;
    }
    received = MsgBuf::new(0, [0; 8]);
    let _ = recv(msqid, &mut received, TEXT_SIZE, 0, 0);
    if !expect(
        recv(msqid, &mut received, TEXT_SIZE, 0, IPC_NOWAIT) == ENOMSG,
        "IPC_NOWAIT ENOMSG",
    ) {
        let _ = msgctl(msqid, IPC_RMID, core::ptr::null_mut());
        return false;
    }

    let sender_queue = msgget(IPC_PRIVATE, IPC_CREAT | 0o600) as i32;
    let mut sender_stat = [0usize; 15];
    if !expect(
        msgctl(sender_queue, IPC_STAT, sender_stat.as_mut_ptr() as *mut u8) == 0,
        "IPC_STAT sender queue",
    ) {
        let _ = msgctl(msqid, IPC_RMID, core::ptr::null_mut());
        let _ = msgctl(sender_queue, IPC_RMID, core::ptr::null_mut());
        return false;
    }
    sender_stat[11] = TEXT_SIZE;
    if !expect(
        msgctl(sender_queue, IPC_SET, sender_stat.as_mut_ptr() as *mut u8) == 0,
        "IPC_SET sender queue",
    ) {
        let _ = msgctl(msqid, IPC_RMID, core::ptr::null_mut());
        let _ = msgctl(sender_queue, IPC_RMID, core::ptr::null_mut());
        return false;
    }
    let send_one = MsgBuf::new(1, *b"send-one");
    let send_two = MsgBuf::new(1, *b"send-two");
    if !expect(send(sender_queue, &send_one), "fill sender queue") {
        let _ = msgctl(msqid, IPC_RMID, core::ptr::null_mut());
        let _ = msgctl(sender_queue, IPC_RMID, core::ptr::null_mut());
        return false;
    }
    let sender = fork();
    if sender == 0 {
        exit(if send(sender_queue, &send_two) { 0 } else { 1 });
    }
    if sender <= 0 {
        let _ = msgctl(msqid, IPC_RMID, core::ptr::null_mut());
        let _ = msgctl(sender_queue, IPC_RMID, core::ptr::null_mut());
        return false;
    }
    sleep(20);
    received = MsgBuf::new(0, [0; 8]);
    let first_received = recv(sender_queue, &mut received, TEXT_SIZE, 0, 0) == TEXT_SIZE as isize
        && received.text == *b"send-one";
    let mut sender_status = 0;
    let sender_reaped =
        waitpid(sender as usize, &mut sender_status) == sender && sender_status == 0;
    received = MsgBuf::new(0, [0; 8]);
    let second_received = recv(sender_queue, &mut received, TEXT_SIZE, 0, 0) == TEXT_SIZE as isize
        && received.text == *b"send-two";
    if !expect(
        first_received && sender_reaped && second_received,
        "full queue wakes blocked msgsnd",
    ) {
        let _ = msgctl(msqid, IPC_RMID, core::ptr::null_mut());
        let _ = msgctl(sender_queue, IPC_RMID, core::ptr::null_mut());
        return false;
    }
    let _ = msgctl(sender_queue, IPC_RMID, core::ptr::null_mut());

    let blocking_queue = msgget(IPC_PRIVATE, IPC_CREAT | 0o600) as i32;
    let child = fork();
    if child == 0 {
        let mut blocked = MsgBuf::new(0, [0; 8]);
        let result = recv(blocking_queue, &mut blocked, TEXT_SIZE, 1, 0);
        exit(if result == EIDRM { 0 } else { 1 });
    }
    if child <= 0 {
        let _ = msgctl(msqid, IPC_RMID, core::ptr::null_mut());
        let _ = msgctl(blocking_queue, IPC_RMID, core::ptr::null_mut());
        return false;
    }
    sleep(20);
    let removed = msgctl(blocking_queue, IPC_RMID, core::ptr::null_mut()) == 0;
    let mut status = 0;
    let reaped = waitpid(child as usize, &mut status) == child && status == 0;
    let cleaned = msgctl(msqid, IPC_RMID, core::ptr::null_mut()) == 0;
    expect(removed && reaped && cleaned, "blocked msgrcv returns EIDRM")
}

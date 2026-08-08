use user_lib::{close, openat, println, read, OpenFlags};

const AT_FDCWD: isize = -100;

fn parse_centiseconds(bytes: &[u8]) -> Option<usize> {
    let mut value = 0usize;
    let mut fraction_digits = 0usize;
    let mut seen_digit = false;
    for byte in bytes {
        match byte {
            b'0'..=b'9' if fraction_digits == 0 => {
                seen_digit = true;
                value = value.checked_mul(10)?.checked_add((byte - b'0') as usize)?;
            }
            b'.' if fraction_digits == 0 && seen_digit => fraction_digits = 1,
            b'0'..=b'9' if fraction_digits == 1 => {
                fraction_digits = 2;
                value = value.checked_mul(10)?.checked_add((byte - b'0') as usize)?;
            }
            b'0'..=b'9' if fraction_digits == 2 => {
                value = value.checked_mul(10)?.checked_add((byte - b'0') as usize)?;
                fraction_digits = 3;
            }
            _ => break,
        }
    }
    if seen_digit && fraction_digits >= 2 {
        Some(value)
    } else {
        None
    }
}

fn read_uptime(fd: usize, buffer: &mut [u8]) -> Option<usize> {
    let mut used = 0;
    while used < buffer.len() {
        let ret = read(fd, &mut buffer[used..used + 1], 1);
        if ret <= 0 {
            break;
        }
        used += ret as usize;
    }
    Some(used)
}

pub fn run() -> bool {
    let fd = openat(AT_FDCWD, "/proc/uptime\0", OpenFlags::O_RDONLY, 0);
    if fd < 0 {
        println!("uptime regression: FAIL (open {})", fd);
        return false;
    }
    let mut first = [0u8; 64];
    let first_len = read_uptime(fd as usize, &mut first).unwrap_or(0);
    let eof = read(fd as usize, &mut first, 1);
    let _ = close(fd as usize);
    let Some(space) = first[..first_len].iter().position(|byte| *byte == b' ') else {
        println!("uptime regression: FAIL (format)");
        return false;
    };
    let first_value = parse_centiseconds(&first[..space]);
    let fd = openat(AT_FDCWD, "/proc/uptime\0", OpenFlags::O_RDONLY, 0);
    if fd < 0 {
        println!("uptime regression: FAIL (reopen {})", fd);
        return false;
    }
    let mut second = [0u8; 64];
    let second_len = read_uptime(fd as usize, &mut second).unwrap_or(0);
    let _ = close(fd as usize);
    let second_value = second[..second_len]
        .iter()
        .position(|byte| *byte == b' ')
        .and_then(|space| parse_centiseconds(&second[..space]));
    let monotonic = match first_value.zip(second_value) {
        Some((a, b)) => b >= a,
        None => false,
    };
    let ok = eof == 0
        && first_len >= 7
        && second_len >= 7
        && first[..first_len].last() == Some(&b'\n')
        && second[..second_len].last() == Some(&b'\n')
        && monotonic;
    println!("uptime regression: {}", if ok { "PASS" } else { "FAIL" });
    ok
}

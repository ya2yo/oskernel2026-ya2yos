use user_lib::{println, rseq, sleep, RseqAbi, RSEQ_CPU_ID_UNINITIALIZED, RSEQ_FLAG_UNREGISTER};

const RSEQ_LEN: u32 = 32;
// The kernel stores the caller-provided signature for matching unregistration
// and for an in-critical-section abort. This probe uses an outside-CS
// descriptor, so the same nonzero value is suitable for both architectures.
const RSEQ_SIG: u32 = 0xf140_1073;

const EINVAL: isize = -22;
const EFAULT: isize = -14;
const EBUSY: isize = -16;
const EPERM: isize = -1;

#[repr(C, align(32))]
#[derive(Clone, Copy)]
struct RseqCs {
    version: u32,
    flags: u32,
    start_ip: u64,
    post_commit_offset: u64,
    abort_ip: u64,
}

static mut RSEQ_AREA: RseqAbi = RseqAbi::new();
static RSEQ_CS_OUTSIDE: RseqCs = RseqCs {
    version: 0,
    flags: 0,
    start_ip: 0,
    post_commit_offset: 0,
    abort_ip: 0,
};

fn read_area() -> RseqAbi {
    unsafe { core::ptr::read(core::ptr::addr_of!(RSEQ_AREA)) }
}

fn write_area(area: RseqAbi) {
    unsafe { core::ptr::write(core::ptr::addr_of_mut!(RSEQ_AREA), area) }
}

pub fn run() -> bool {
    let result = (|| -> Result<(), &'static str> {
        if core::mem::size_of::<RseqAbi>() != RSEQ_LEN as usize {
            return Err("ABI size");
        }

        let area = core::ptr::addr_of_mut!(RSEQ_AREA);
        if area as usize % RSEQ_LEN as usize != 0 {
            return Err("ABI alignment");
        }

        if rseq(area, RSEQ_LEN, u32::MAX, RSEQ_SIG) != EINVAL {
            return Err("invalid flags errno");
        }
        let unaligned = unsafe { (area as *mut u8).add(1) as *mut RseqAbi };
        if rseq(unaligned, RSEQ_LEN, 0, RSEQ_SIG) != EINVAL {
            return Err("unaligned ABI errno");
        }
        if rseq(area, RSEQ_LEN - 1, 0, RSEQ_SIG) != EINVAL {
            return Err("short ABI errno");
        }
        if rseq(core::ptr::null_mut(), RSEQ_LEN, 0, RSEQ_SIG) != EFAULT {
            return Err("bad address errno");
        }

        if rseq(area, RSEQ_LEN, 0, RSEQ_SIG) != 0 {
            return Err("register");
        }
        let registered = read_area();
        if registered.rseq_cs != 0
            || registered.cpu_id != registered.cpu_id_start
            || registered.cpu_id == RSEQ_CPU_ID_UNINITIALIZED
        {
            return Err("registration fields");
        }
        if rseq(area, RSEQ_LEN, 0, RSEQ_SIG) != EBUSY {
            return Err("duplicate register errno");
        }

        // A syscall which does not schedule is not an rseq event.  Its return
        // must leave an active descriptor untouched.
        let mut area_value = read_area();
        area_value.rseq_cs = core::ptr::addr_of!(RSEQ_CS_OUTSIDE) as u64;
        write_area(area_value);
        if rseq(area, RSEQ_LEN, 0, RSEQ_SIG) != EBUSY {
            return Err("cleanup trigger errno");
        }
        if read_area().rseq_cs == 0 {
            return Err("unexpected syscall cleanup");
        }

        // Blocking causes a real context switch; the resumed task must then
        // consume the pending event and clear the descriptor.
        sleep(1);
        if read_area().rseq_cs != 0 {
            return Err("scheduled return cleanup");
        }

        if rseq(
            area,
            RSEQ_LEN,
            RSEQ_FLAG_UNREGISTER,
            RSEQ_SIG.wrapping_add(1),
        ) != EPERM
        {
            return Err("wrong signature errno");
        }
        if rseq(area, RSEQ_LEN, RSEQ_FLAG_UNREGISTER, RSEQ_SIG) != 0 {
            return Err("unregister");
        }
        let unregistered = read_area();
        if unregistered.cpu_id_start != RSEQ_CPU_ID_UNINITIALIZED
            || unregistered.cpu_id != RSEQ_CPU_ID_UNINITIALIZED
        {
            return Err("unregister fields");
        }
        if rseq(area, RSEQ_LEN, RSEQ_FLAG_UNREGISTER, RSEQ_SIG) != EINVAL {
            return Err("duplicate unregister errno");
        }

        Ok(())
    })();

    match result {
        Ok(()) => {
            println!("rseq regression: PASS");
            true
        }
        Err(step) => {
            println!("rseq regression: FAIL ({})", step);
            false
        }
    }
}

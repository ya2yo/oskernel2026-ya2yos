//! Interrupt request (IRQ) handling.
//! TODO: 目前还未实现 irq 功能
use core::sync::atomic::{AtomicUsize, Ordering};

mod handler_table;
use handler_table::*;

static IRQ_HOOK: AtomicUsize = AtomicUsize::new(0);
pub fn register_irq_hook(hook: fn(usize)) -> bool {
    IRQ_HOOK
        .compare_exchange(
            0,
            hook as *const () as usize,
            Ordering::SeqCst,
            Ordering::SeqCst,
        )
        .is_ok()
}

pub use handler_table::HandlerTable;
/// The type if an IRQ handler.
pub type IrqHandler = handler_table::Handler;

/// Target specification for inter-processor interrupts (IPIs).
pub enum IpiTarget {
    /// Send to the current CPU.
    Current {
        /// The CPU ID of the current CPU.
        cpu_id: usize,
    },
    /// Send to a specific CPU.
    Other {
        /// The CPU ID of the target CPU.
        cpu_id: usize,
    },
    /// Send to all other CPUs.
    AllExceptCurrent {
        /// The CPU ID of the current CPU.
        cpu_id: usize,
        /// The total number of CPUs.
        cpu_num: usize,
    },
}

const MAX_IRQ_COUNT: usize = 256;

pub static IRQ_HANDLERS: HandlerTable<MAX_IRQ_COUNT> = HandlerTable::new();

/// IRQ management interface.
/// Enables or disables the given IRQ.
pub fn set_enable(_irq: usize, _enabled: bool) {
    #[cfg(feature = "irq")]
    if irq < MAX_IRQ_COUNT {
        unimplemented!("Call your hardware driver to enable/disable IRQ");
    }
}

/// Registers an IRQ handler for the given IRQ.
///
/// It also enables the IRQ if the registration succeeds. It returns `false`
/// if the registration failed.
pub fn register(irq: usize, handler: IrqHandler) -> bool {
    if IRQ_HANDLERS.register_handler(irq, handler) {
        set_enable(irq, true);
        true
    } else {
        false
    }
}

/// Unregisters the IRQ handler for the given IRQ.
///
/// It also disables the IRQ if the unregistration succeeds. It returns the
/// existing handler if it is registered, `None` otherwise.
pub fn unregister(irq: usize) -> Option<IrqHandler> {
    let handler = IRQ_HANDLERS.unregister_handler(irq);
    if handler.is_some() {
        set_enable(irq, false);
    }
    handler
}

/// Handles the IRQ.
///
/// It is called by the common interrupt handler. It should look up in the
/// IRQ handler table and calls the corresponding handler. If necessary, it
/// also acknowledges the interrupt controller after handling.
///
/// Returns the "real" IRQ number. On some platforms, this may differ from
/// the input `irq` number, for example on AArch64 the input `irq` is
/// ignored and the real IRQ number is obtained from the GIC. Returns
/// `None` if the IRQ is spurious.
pub fn handle(irq: usize) -> Option<usize> {
    let real_irq = irq;
    let hook = IRQ_HOOK.load(Ordering::Relaxed);
    if hook != 0 {
        let hook_fn: fn(usize) = unsafe { core::mem::transmute(hook) };
        hook_fn(real_irq);
    }
    if !IRQ_HANDLERS.handle(real_irq) {
        println!("Unhandled IRQ: {}", real_irq);
    }
    unimplemented!("通知硬件中断处理完成");
    Some(real_irq)
}

/// Sends an inter-processor interrupt (IPI) to the specified target CPU or all CPUs.
pub fn send_ipi(_irq_num: usize, _target: IpiTarget) {
    unimplemented!("call hardware function")
}

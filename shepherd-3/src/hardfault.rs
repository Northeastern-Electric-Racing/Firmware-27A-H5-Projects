//! Cortex-M hardfault handling.

use defmt::{info, warn};

/// magic number constant to mark valid crash data. the hex value is meant to spell "balls"
/// this is set in CrashInfo by the fault handler, and then when the next boot reads the memory, it uses this const to verify that there is real crash data there instead of random junk
const CRASH_MAGIC: u32 = 0xBA115;

/// Stores CPU state and fault data as of the last HardFault.
#[repr(C)]
#[derive(Clone, Copy, defmt::Format)]
struct CrashInfo {
    magic: u32,
    /// Program Counter.
    pc: u32,
    /// Link Register.
    lr: u32,
    /// Configurable Fault Status Register.
    cfsr: u32,
    /// HardFault Status.
    hfsr: u32,
    /// MemManage Fault Address (valid only when CFSR marks it).
    mmfar: u32,
    /// BusFault Address.
    bfar: u32,
    /// Consecutive HardFaults (used to catch boot loops).
    count: u32,
}

/// SAFETY: `.uninit` is a NOLOAD section, and `CrashInfo` data is static (so putting it there is fine)
#[unsafe(link_section = ".uninit.CRASH")]
static mut CRASH: core::mem::MaybeUninit<CrashInfo> = core::mem::MaybeUninit::uninit();

/// Reports why we last reset, logs any crash record left in RAM.
/// this is meant to be called at the top of main() after embassy_stm32::init
pub fn report_last_reset() {
    use embassy_stm32::pac;

    let rsr = pac::RCC.rsr().read();
    info!("Reset cause: pin={} bor={} sw={} iwdg={} wwdg={} lowpower={}", rsr.pinrstf(), rsr.borrstf(), rsr.sftrstf(), rsr.iwdgrstf(), rsr.wwdgrstf(), rsr.lpwrrstf());
    pac::RCC.rsr().modify(|w| w.set_rmvf(true));

    // SAFETY: `CRASH` is correctly sized by the linker. validity of data is checked via CRASH_MAGIC
    let crash = unsafe { (&raw const CRASH).read_volatile().assume_init() };
    if crash.magic == CRASH_MAGIC {
        warn!("Recovered crash record from previous boot: {}", crash);
        // SAFETY: no reference to `CRASH` exists, and no race conditions can happen here because STM32H563 is single-core and this write can't be pre-empted by another task.
        // i think a race condition could technically happen if an ISR were to try acessing this data while it is mid-write but there's no reason this should ever be accessed in an ISR
        unsafe { (&raw mut CRASH).write_volatile(core::mem::MaybeUninit::zeroed()) };
    }
}

#[cortex_m_rt::exception]
unsafe fn HardFault(frame: &cortex_m_rt::ExceptionFrame) -> ! {
    use cortex_m::peripheral::{DCB, SCB};

    // SAFETY: the SCB block is at a fixed address, is always mapped, and doesn't need initialization. every field is a VolatileCell. this is how you're supposed to access the block
    let scb = unsafe { &*SCB::PTR };

    // SAFETY: `CRASH` is correctly sized by the linker. validity of data is checked via `CRASH_MAGIC`
    let previous = unsafe { (&raw const CRASH).read_volatile().assume_init() };
    let count = if previous.magic == CRASH_MAGIC { previous.count + 1 } else { 1 };

    let info = CrashInfo {
        magic: CRASH_MAGIC,
        pc: frame.pc(),
        lr: frame.lr(),
        cfsr: scb.cfsr.read(),
        hfsr: scb.hfsr.read(),
        mmfar: scb.mmfar.read(),
        bfar: scb.bfar.read(),
        count,
    };
    // SAFETY: no reference to `CRASH` exists and HardFault can't be preempted by anything
    unsafe { (&raw mut CRASH).write_volatile(core::mem::MaybeUninit::new(info)) };

    defmt::error!("HardFault: {}", info);

    if DCB::is_debugger_attached() {
        cortex_m::asm::bkpt();
    }

    SCB::sys_reset()
}

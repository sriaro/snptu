
use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

const MI_OPTION_PURGE_DELAY: libmimalloc_sys::mi_option_t = 15;

pub fn init() {
    unsafe {
        libmimalloc_sys::mi_option_set(MI_OPTION_PURGE_DELAY, 0);
    }
}

pub fn release() {
    crate::text::release_fallbacks();
    unsafe {
        libmimalloc_sys::mi_collect(true);
    }
}

pub fn trim() {
    release();
    use windows::Win32::System::{ProcessStatus::EmptyWorkingSet, Threading::GetCurrentProcess};
    unsafe {
        let _ = EmptyWorkingSet(GetCurrentProcess());
    }
}

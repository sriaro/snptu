
use std::any::Any;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use windows::Win32::System::Threading::THREAD_PRIORITY;

const MIN_PARALLEL_BYTES: usize = 192 * 1024;

pub fn workers() -> usize {
    thread::available_parallelism().map_or(1, |n| n.get().saturating_sub(1).max(1)).min(16)
}

pub fn lower_priority() {
    set_priority(windows::Win32::System::Threading::THREAD_PRIORITY_BELOW_NORMAL);
}

fn callers_priority() -> THREAD_PRIORITY {
    use windows::Win32::System::Threading::{GetCurrentThread, GetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL};
    match unsafe { GetThreadPriority(GetCurrentThread()) } {
        i32::MAX => THREAD_PRIORITY_BELOW_NORMAL,
        p => THREAD_PRIORITY(p),
    }
}

fn set_priority(priority: THREAD_PRIORITY) {
    use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority};
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), priority);
    }
}

type Spawn<'a> = dyn Fn(Box<dyn FnOnce() + Send>) + Sync + 'a;

struct Shared {
    state: Mutex<State>,
    through: Condvar,
    work: *const (),
    spawn: *const (),
    priority: THREAD_PRIORITY,
}

#[derive(Default)]
struct State {
    to_start: usize,
    at_work: usize,
    closed: bool,
    panic: Option<Box<dyn Any + Send>>,
}

unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

fn share(helpers: usize, work: &(dyn Fn() + Sync), spawn: &Spawn<'_>) {
    let shared = Arc::new(Shared {
        state: Mutex::new(State { to_start: helpers, ..State::default() }),
        through: Condvar::new(),
        work: &work as *const &(dyn Fn() + Sync) as *const (),
        spawn: &spawn as *const &Spawn<'_> as *const (),
        priority: callers_priority(),
    });
    struct Close<'a>(&'a Shared);
    impl Drop for Close<'_> {
        fn drop(&mut self) {
            let mut s = lock(&self.0.state);
            s.closed = true;
            while s.at_work > 0 {
                s = self.0.through.wait(s).unwrap_or_else(PoisonError::into_inner);
            }
        }
    }
    let close = Close(&shared);
    start_two(&shared, spawn);
    work();
    drop(close);
    let panic = lock(&shared.state).panic.take();
    if let Some(panic) = panic {
        std::panic::resume_unwind(panic);
    }
}

fn start_two(shared: &Arc<Shared>, spawn: &Spawn<'_>) {
    let n = {
        let mut s = lock(&shared.state);
        let n = if s.closed { 0 } else { s.to_start.min(2) };
        s.to_start -= n;
        n
    };
    for _ in 0..n {
        let shared = Arc::clone(shared);
        spawn(Box::new(move || helper(&shared)));
    }
}

fn helper(shared: &Arc<Shared>) {
    set_priority(shared.priority);
    {
        let mut s = lock(&shared.state);
        if s.closed {
            return;
        }
        s.at_work += 1;
    }
    let (work, spawn) = unsafe { (*(shared.work as *const &(dyn Fn() + Sync)), *(shared.spawn as *const &Spawn<'_>)) };
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        start_two(shared, spawn);
        work();
    }))
    .err();
    let mut s = lock(&shared.state);
    s.at_work -= 1;
    s.panic = s.panic.take().or(panic);
    if s.closed && s.at_work == 0 {
        shared.through.notify_one();
    }
}

fn spawn(helper: Box<dyn FnOnce() + Send>) {
    let _ = thread::Builder::new().spawn(helper);
}

pub fn rows<T: Send>(data: &mut [T], row_len: usize, f: impl Fn(usize, &mut [T]) + Sync) {
    if row_len == 0 || data.is_empty() {
        return;
    }
    let n_rows = data.len() / row_len;
    let t = workers().min(n_rows);
    if t <= 1 || std::mem::size_of_val(data) < MIN_PARALLEL_BYTES {
        for (y, row) in data.chunks_exact_mut(row_len).enumerate() {
            f(y, row);
        }
        return;
    }
    let per = n_rows.div_ceil(t * 4).max(1);
    let queue = Mutex::new(data.chunks_mut(per * row_len).enumerate());
    let work = || {
        while let Some((block, chunk)) = queue.lock().ok().and_then(|mut q| q.next()) {
            for (i, row) in chunk.chunks_exact_mut(row_len).enumerate() {
                f(block * per + i, row);
            }
        }
    };
    share(t - 1, &work, &spawn);
}

pub fn ranges<R: Send>(n: usize, min_per_thread: usize, f: impl Fn(std::ops::Range<usize>) -> R + Sync) -> Vec<R> {
    let t = workers().min(n.div_ceil(min_per_thread.max(1))).max(1);
    if t == 1 {
        return vec![f(0..n)];
    }
    let parts = (t * 4).min(n);
    let per = n.div_ceil(parts);
    let next = AtomicUsize::new(0);
    let done = Mutex::new(Vec::with_capacity(parts));
    let work = || {
        let mut mine = Vec::new();
        loop {
            let i = next.fetch_add(1, Ordering::Relaxed);
            let (a, b) = (i * per, ((i + 1) * per).min(n));
            if a >= n {
                break;
            }
            mine.push((i, f(a..b)));
        }
        done.lock().unwrap_or_else(PoisonError::into_inner).append(&mut mine);
    };
    share(t - 1, &work, &spawn);
    let mut done = done.into_inner().unwrap_or_else(PoisonError::into_inner);
    done.sort_by_key(|(i, _)| *i);
    done.into_iter().map(|(_, r)| r).collect()
}

pub fn blur_rgba(data: &mut [u8], w: usize, h: usize, radius: usize) {
    if w == 0 || h == 0 || radius == 0 || data.len() != w * h * 4 {
        return;
    }
    let mut tmp = vec![0u8; data.len()];
    for _ in 0..3 {
        box_h(data, &mut tmp, w, h, radius);
        box_h(&tmp, data, h, w, radius);
    }
}

fn box_h(src: &[u8], dst: &mut [u8], w: usize, h: usize, r: usize) {
    let blurred: Vec<Vec<u8>> = ranges(h, 16, |range| {
        let mut out = Vec::with_capacity(range.len() * w * 4);
        let mut row_buf = vec![0u8; w * 4];
        for y in range {
            blur_row(&src[y * w * 4..(y + 1) * w * 4], &mut row_buf, r);
            out.extend_from_slice(&row_buf);
        }
        out
    });
    let mut y = 0;
    for block in &blurred {
        for row in block.chunks_exact(w * 4) {
            for x in 0..w {
                let d = (x * h + y) * 4;
                dst[d..d + 4].copy_from_slice(&row[x * 4..x * 4 + 4]);
            }
            y += 1;
        }
    }
}

fn blur_row(src: &[u8], dst: &mut [u8], r: usize) {
    let w = src.len() / 4;
    let win = (2 * r + 1) as u32;
    let px = |i: isize| -> [u32; 4] {
        let i = i.clamp(0, w as isize - 1) as usize * 4;
        [src[i] as u32, src[i + 1] as u32, src[i + 2] as u32, src[i + 3] as u32]
    };
    let mut sum = [0u32; 4];
    for i in -(r as isize)..=(r as isize) {
        let p = px(i);
        for k in 0..4 {
            sum[k] += p[k];
        }
    }
    for x in 0..w {
        for k in 0..4 {
            dst[x * 4 + k] = ((sum[k] + win / 2) / win) as u8;
        }
        let (add, sub) = (px(x as isize + r as isize + 1), px(x as isize - r as isize));
        for k in 0..4 {
            sum[k] = sum[k] + add[k] - sub[k];
        }
    }
}

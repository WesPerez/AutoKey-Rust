use crate::config::{Config, KeyConfig, KEY_SLOT_COUNT, MAX_DELAY_MS};
use crate::input::InputTarget;
use crate::{humanizer, input, window, AppCommand};
use anyhow::{Context, Result};
use parking_lot::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, TryRecvError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

// ── High-precision timer using QueryPerformanceCounter ──────────────
// Uses QPC for sub-millisecond timing accuracy instead of relying on
// the default ~15.6ms Windows timer resolution. Combined with
// timeBeginPeriod(1) for better sleep precision and a hybrid
// sleep+busy-wait strategy for the final ~0.5ms.

use once_cell::sync::Lazy;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

struct HiResTimer {
    freq: i64,
}

static HI_RES_TIMER: Lazy<HiResTimer> = Lazy::new(|| {
    let freq = unsafe {
        let mut f = 1i64;
        let _ = QueryPerformanceFrequency(&mut f);
        f.max(1)
    };
    HiResTimer { freq }
});

static TIMER_RESOLUTION_SET: Lazy<bool> = Lazy::new(|| {
    // Set Windows timer resolution to 1ms for better sleep precision
    // This affects the entire process but is necessary for accurate timing
    unsafe { timeBeginPeriod(1) == 0 }
});

// Link to winmm for timeBeginPeriod/timeEndPeriod
#[link(name = "winmm")]
extern "system" {
    fn timeBeginPeriod(uPeriod: u32) -> u32;
    fn timeEndPeriod(uPeriod: u32) -> u32;
}

impl HiResTimer {
    fn now_ns(&self) -> u64 {
        unsafe {
            let mut ticks = 0i64;
            let _ = QueryPerformanceCounter(&mut ticks);
            (ticks as u128 * 1_000_000_000 / self.freq as u128) as u64
        }
    }
}

const RECOVERY_DELAY: Duration = Duration::from_millis(100);

pub struct AutomationEngine {
    worker: Option<JoinHandle<()>>,
}

impl AutomationEngine {
    pub fn spawn(
        commands: Receiver<AppCommand>,
        config: Arc<RwLock<Config>>,
        is_running: Arc<AtomicBool>,
        key_running: Arc<RwLock<Vec<bool>>>,
        bound_window: Arc<RwLock<Option<isize>>>,
        status: Arc<RwLock<String>>,
    ) -> Result<Self> {
        let panic_running = is_running.clone();
        let panic_key_running = key_running.clone();
        let panic_status = status.clone();
        let worker = thread::Builder::new()
            .name(crate::stealth::random_thread_name())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_engine(
                        commands,
                        config,
                        is_running,
                        key_running,
                        bound_window,
                        status,
                    );
                }));
                if result.is_err() {
                    panic_running.store(false, Ordering::Release);
                    panic_key_running.write().fill(false);
                    *panic_status.write() = "调度线程异常退出".to_owned();
                }
            })
            .context("无法启动调度线程")?;

        Ok(Self {
            worker: Some(worker),
        })
    }

    pub fn join(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for AutomationEngine {
    fn drop(&mut self) {
        self.join();
        if once_cell::sync::Lazy::get(&TIMER_RESOLUTION_SET)
            .copied()
            .unwrap_or(false)
        {
            // Restore timer resolution on exit.
            unsafe {
                let _ = timeEndPeriod(1);
            }
        }
    }
}

#[derive(Debug, Clone)]
struct RunKey {
    index: usize,
    config: KeyConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Control {
    Continue,
    Stop,
    Exit,
}

fn run_engine(
    commands: Receiver<AppCommand>,
    config: Arc<RwLock<Config>>,
    is_running: Arc<AtomicBool>,
    key_running: Arc<RwLock<Vec<bool>>>,
    bound_window: Arc<RwLock<Option<isize>>>,
    status: Arc<RwLock<String>>,
) {
    // Ensure high-resolution timer is initialized
    let _ = *TIMER_RESOLUTION_SET;

    while let Ok(command) = commands.recv() {
        let should_start = match command {
            AppCommand::Start => true,
            AppCommand::ToggleRunning => !is_running.load(Ordering::Acquire),
            AppCommand::Exit => return,
            AppCommand::Stop => false,
        };
        if !should_start {
            set_stopped(&is_running, &key_running, &status, "已停止");
            continue;
        }

        let snapshot = config.read().clone();
        if let Some(error) = snapshot.validation_error() {
            set_stopped(&is_running, &key_running, &status, &error);
            continue;
        }
        if snapshot.max_loops == 0 {
            set_stopped(
                &is_running,
                &key_running,
                &status,
                "循环次数为 0，没有执行任务",
            );
            continue;
        }

        let keys: Vec<RunKey> = snapshot
            .keys
            .iter()
            .cloned()
            .enumerate()
            .filter(|(_, key)| key.enabled && key.vk_code > 0)
            .map(|(index, config)| RunKey { index, config })
            .collect();

        humanizer::set_timing_variation_level(snapshot.timing_variation_level);
        humanizer::reset();
        is_running.store(true, Ordering::Release);
        *status.write() = format!("运行中，共 {} 个按键", keys.len());

        let control = if snapshot.independent_loop {
            run_independent(
                &commands,
                &snapshot,
                keys,
                is_running.clone(),
                key_running.clone(),
                bound_window.clone(),
                status.clone(),
            )
        } else {
            run_sequential(
                &commands,
                &snapshot,
                &keys,
                &is_running,
                &key_running,
                &bound_window,
                &status,
            )
        };

        key_running.write().fill(false);
        is_running.store(false, Ordering::Release);
        match control {
            Control::Continue => *status.write() = "已完成".to_owned(),
            Control::Stop => {
                if status.read().starts_with("运行中") {
                    *status.write() = "已停止".to_owned();
                }
            }
            Control::Exit => return,
        }
    }
}

fn run_independent(
    commands: &Receiver<AppCommand>,
    config: &Config,
    keys: Vec<RunKey>,
    is_running: Arc<AtomicBool>,
    key_running: Arc<RwLock<Vec<bool>>>,
    bound_window: Arc<RwLock<Option<isize>>>,
    status: Arc<RwLock<String>>,
) -> Control {
    let stop_workers = Arc::new(AtomicBool::new(false));
    let (done_tx, done_rx) = channel();
    let mut workers = Vec::with_capacity(keys.len());

    for key in keys {
        set_key_running(&key_running, key.index, true);
        let worker_config = config.clone();
        let worker_running = is_running.clone();
        let worker_stop = stop_workers.clone();
        let worker_key_running = key_running.clone();
        let worker_bound_window = bound_window.clone();
        let worker_status = status.clone();
        let worker_done = done_tx.clone();
        let key_index = key.index;

        match thread::Builder::new()
            .name(crate::stealth::random_thread_name())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_independent_key(
                        &worker_config,
                        &key,
                        &worker_running,
                        &worker_stop,
                        &worker_bound_window,
                        &worker_status,
                    )
                }));
                let control = result.unwrap_or_else(|_| {
                    worker_running.store(false, Ordering::Release);
                    worker_stop.store(true, Ordering::Release);
                    *worker_status.write() = "key worker thread panicked".to_owned();
                    Control::Stop
                });
                set_key_running(&worker_key_running, key_index, false);
                let _ = worker_done.send(control);
            }) {
            Ok(worker) => workers.push(worker),
            Err(error) => {
                stop_independent_workers(&stop_workers, &is_running, workers);
                *status.write() = format!("failed to start key worker thread: {error}");
                return Control::Stop;
            }
        }
    }
    drop(done_tx);

    let mut remaining = workers.len();
    while remaining > 0 {
        while let Ok(control) = done_rx.try_recv() {
            remaining -= 1;
            if control != Control::Continue {
                stop_independent_workers(&stop_workers, &is_running, workers);
                return control;
            }
        }

        match commands.recv_timeout(Duration::from_millis(20)) {
            Ok(AppCommand::Start) | Err(RecvTimeoutError::Timeout) => {}
            Ok(AppCommand::Exit) | Err(RecvTimeoutError::Disconnected) => {
                stop_independent_workers(&stop_workers, &is_running, workers);
                return Control::Exit;
            }
            Ok(AppCommand::Stop) | Ok(AppCommand::ToggleRunning) => {
                stop_independent_workers(&stop_workers, &is_running, workers);
                return Control::Stop;
            }
        }
    }

    for worker in workers {
        let _ = worker.join();
    }
    Control::Continue
}

fn run_independent_key(
    config: &Config,
    key: &RunKey,
    is_running: &AtomicBool,
    stop: &AtomicBool,
    bound_window: &RwLock<Option<isize>>,
    status: &RwLock<String>,
) -> Control {
    let mut completed = 0u32;
    let mut next_due = Instant::now();

    loop {
        match wait_until_running(next_due, is_running, stop) {
            Control::Continue => {}
            control => return control,
        }

        match perform_press_running(key, is_running, stop, bound_window) {
            Ok(Control::Continue) => {
                completed = completed.saturating_add(1);
                if reached_limit(config.max_loops, completed) {
                    return Control::Continue;
                }
                next_due = next_scheduled_due(next_due, calculate_delay(config, key));
            }
            Ok(control) => return control,
            Err(error) => {
                handle_send_error(key, error, bound_window, status);
                if config.max_loops >= 0 {
                    return Control::Continue;
                }
                match wait_running_interruptible(RECOVERY_DELAY, is_running, stop) {
                    Control::Continue => next_due = Instant::now(),
                    control => return control,
                }
            }
        }
    }
}

fn stop_independent_workers(
    stop_workers: &AtomicBool,
    is_running: &AtomicBool,
    workers: Vec<JoinHandle<()>>,
) {
    stop_workers.store(true, Ordering::Release);
    is_running.store(false, Ordering::Release);
    for worker in workers {
        let _ = worker.join();
    }
}

fn next_scheduled_due(previous_due: Instant, delay: Duration) -> Instant {
    let now = Instant::now();
    previous_due
        .checked_add(delay)
        .filter(|candidate| *candidate > now)
        .unwrap_or(now)
}

fn wait_until_interruptible(
    deadline: Instant,
    commands: &Receiver<AppCommand>,
    is_running: &AtomicBool,
    status: &RwLock<String>,
) -> Control {
    let now = Instant::now();
    if deadline <= now {
        return drain_control_commands(commands, is_running, status);
    }
    wait_interruptible(deadline.duration_since(now), commands, is_running, status)
}

fn drain_control_commands(
    commands: &Receiver<AppCommand>,
    is_running: &AtomicBool,
    status: &RwLock<String>,
) -> Control {
    loop {
        match commands.try_recv() {
            Ok(AppCommand::Start) => {}
            Ok(AppCommand::Exit) => return Control::Exit,
            Ok(AppCommand::Stop) | Ok(AppCommand::ToggleRunning) => {
                is_running.store(false, Ordering::Release);
                *status.write() = "已停止".to_owned();
                return Control::Stop;
            }
            Err(TryRecvError::Empty) => return Control::Continue,
            Err(TryRecvError::Disconnected) => return Control::Exit,
        }
    }
}
/// Interruptible wait with a short high-precision finish.
fn wait_interruptible(
    duration: Duration,
    commands: &Receiver<AppCommand>,
    is_running: &AtomicBool,
    status: &RwLock<String>,
) -> Control {
    if duration.is_zero() {
        return drain_control_commands(commands, is_running, status);
    }

    let deadline_ns = HI_RES_TIMER
        .now_ns()
        .saturating_add(duration.as_nanos() as u64);
    const SPIN_THRESHOLD_NS: u64 = 200_000;
    const MAX_SLEEP_SLICE: Duration = Duration::from_millis(20);

    loop {
        match drain_control_commands(commands, is_running, status) {
            Control::Continue => {}
            control => return control,
        }

        let now = HI_RES_TIMER.now_ns();
        if now >= deadline_ns {
            return Control::Continue;
        }

        let remaining_ns = deadline_ns - now;
        if remaining_ns <= SPIN_THRESHOLD_NS {
            break;
        }

        let sleep_ns = (remaining_ns - SPIN_THRESHOLD_NS).min(MAX_SLEEP_SLICE.as_nanos() as u64);
        match commands.recv_timeout(Duration::from_nanos(sleep_ns)) {
            Ok(AppCommand::Start) | Err(RecvTimeoutError::Timeout) => {}
            Ok(AppCommand::Exit) | Err(RecvTimeoutError::Disconnected) => return Control::Exit,
            Ok(AppCommand::Stop) | Ok(AppCommand::ToggleRunning) => {
                is_running.store(false, Ordering::Release);
                *status.write() = "已停止".to_owned();
                return Control::Stop;
            }
        }
    }

    loop {
        match drain_control_commands(commands, is_running, status) {
            Control::Continue => {}
            control => return control,
        }
        if HI_RES_TIMER.now_ns() >= deadline_ns {
            return Control::Continue;
        }
        std::hint::spin_loop();
    }
}

fn wait_until_running(deadline: Instant, is_running: &AtomicBool, stop: &AtomicBool) -> Control {
    let now = Instant::now();
    if deadline <= now {
        return running_control(is_running, stop);
    }
    wait_running_interruptible(deadline.duration_since(now), is_running, stop)
}

fn wait_running_interruptible(
    duration: Duration,
    is_running: &AtomicBool,
    stop: &AtomicBool,
) -> Control {
    if duration.is_zero() {
        return running_control(is_running, stop);
    }

    let deadline_ns = HI_RES_TIMER
        .now_ns()
        .saturating_add(duration.as_nanos() as u64);
    const SPIN_THRESHOLD_NS: u64 = 200_000;
    const MAX_SLEEP_SLICE: Duration = Duration::from_millis(20);

    loop {
        match running_control(is_running, stop) {
            Control::Continue => {}
            control => return control,
        }

        let now = HI_RES_TIMER.now_ns();
        if now >= deadline_ns {
            return Control::Continue;
        }

        let remaining_ns = deadline_ns - now;
        if remaining_ns <= SPIN_THRESHOLD_NS {
            break;
        }

        let sleep_ns = (remaining_ns - SPIN_THRESHOLD_NS).min(MAX_SLEEP_SLICE.as_nanos() as u64);
        thread::sleep(Duration::from_nanos(sleep_ns));
    }

    loop {
        match running_control(is_running, stop) {
            Control::Continue => {}
            control => return control,
        }
        if HI_RES_TIMER.now_ns() >= deadline_ns {
            return Control::Continue;
        }
        std::hint::spin_loop();
    }
}

fn running_control(is_running: &AtomicBool, stop: &AtomicBool) -> Control {
    if stop.load(Ordering::Acquire) || !is_running.load(Ordering::Acquire) {
        Control::Stop
    } else {
        Control::Continue
    }
}

fn run_sequential(
    commands: &Receiver<AppCommand>,
    config: &Config,
    keys: &[RunKey],
    is_running: &AtomicBool,
    key_running: &RwLock<Vec<bool>>,
    bound_window: &RwLock<Option<isize>>,
    status: &RwLock<String>,
) -> Control {
    let mut completed_cycles = 0u32;
    loop {
        for key in keys {
            set_key_running(key_running, key.index, true);
            let started_at = Instant::now();
            match perform_press(commands, key, is_running, bound_window, status) {
                Ok(control) => {
                    if control != Control::Continue {
                        set_key_running(key_running, key.index, false);
                        return control;
                    }
                }
                Err(error) => {
                    handle_send_error(key, error, bound_window, status);
                    set_key_running(key_running, key.index, false);
                    return Control::Stop;
                }
            }

            let next_due = next_scheduled_due(started_at, calculate_delay(config, key));
            let control = wait_until_interruptible(next_due, commands, is_running, status);
            set_key_running(key_running, key.index, false);
            if control != Control::Continue {
                return control;
            }
        }

        completed_cycles = completed_cycles.saturating_add(1);
        if reached_limit(config.max_loops, completed_cycles) {
            return Control::Continue;
        }
    }
}

fn perform_press(
    commands: &Receiver<AppCommand>,
    key: &RunKey,
    is_running: &AtomicBool,
    bound_window: &RwLock<Option<isize>>,
    status: &RwLock<String>,
) -> Result<Control> {
    let target = match *bound_window.read() {
        Some(hwnd) => InputTarget::Window(hwnd),
        None => InputTarget::Foreground,
    };
    input::key_down(target, key.config.vk_code)?;
    let control = wait_interruptible(
        Duration::from_millis(humanizer::next_press_duration() as u64),
        commands,
        is_running,
        status,
    );
    let key_up_result = input::key_up(target, key.config.vk_code);
    key_up_result?;
    Ok(control)
}

fn perform_press_running(
    key: &RunKey,
    is_running: &AtomicBool,
    stop: &AtomicBool,
    bound_window: &RwLock<Option<isize>>,
) -> Result<Control> {
    let target = match *bound_window.read() {
        Some(hwnd) => InputTarget::Window(hwnd),
        None => InputTarget::Foreground,
    };
    input::key_down(target, key.config.vk_code)?;
    let control = wait_running_interruptible(
        Duration::from_millis(humanizer::next_press_duration() as u64),
        is_running,
        stop,
    );
    let key_up_result = input::key_up(target, key.config.vk_code);
    key_up_result?;
    Ok(control)
}

fn calculate_delay(config: &Config, key: &RunKey) -> Duration {
    let combined_range = key
        .config
        .random_range
        .saturating_add(config.global_random_delay)
        .min(MAX_DELAY_MS);

    Duration::from_millis(
        humanizer::next_delay(key.config.base_delay, combined_range, key.index) as u64,
    )
}

fn reached_limit(max_loops: i32, completed: u32) -> bool {
    max_loops >= 0 && completed >= max_loops as u32
}

fn handle_send_error(
    key: &RunKey,
    error: anyhow::Error,
    bound_window: &RwLock<Option<isize>>,
    status: &RwLock<String>,
) {
    if let Some(hwnd) = *bound_window.read() {
        if !window::is_window_valid(hwnd) {
            *bound_window.write() = None;
        }
    }
    *status.write() = format!("按键 [{}] 发送失败: {error}", key.config.key_name);
}

fn set_key_running(key_running: &RwLock<Vec<bool>>, index: usize, running: bool) {
    let mut states = key_running.write();
    if states.len() != KEY_SLOT_COUNT {
        states.resize(KEY_SLOT_COUNT, false);
    }
    if let Some(state) = states.get_mut(index) {
        *state = running;
    }
}

fn set_stopped(
    is_running: &AtomicBool,
    key_running: &RwLock<Vec<bool>>,
    status: &RwLock<String>,
    message: &str,
) {
    is_running.store(false, Ordering::Release);
    key_running.write().fill(false);
    *status.write() = message.to_owned();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loop_limit_is_inclusive() {
        assert!(!reached_limit(3, 2));
        assert!(reached_limit(3, 3));
        assert!(!reached_limit(-1, u32::MAX));
    }

    #[test]
    fn combines_per_key_and_global_random_ranges() {
        let _guard = humanizer::TEST_LOCK.lock();
        humanizer::reset();
        humanizer::set_timing_variation_level(1);
        let config = Config {
            global_random_delay: 200,
            ..Config::default()
        };
        let key = RunKey {
            index: 0,
            config: KeyConfig {
                base_delay: 1000,
                random_range: 100,
                ..KeyConfig::default()
            },
        };
        for _ in 0..100 {
            let delay = calculate_delay(&config, &key).as_millis();
            assert!((700..=1300).contains(&delay));
        }
    }

    #[test]
    fn hi_res_timer_is_monotonic() {
        let t1 = HI_RES_TIMER.now_ns();
        let t2 = HI_RES_TIMER.now_ns();
        assert!(t2 >= t1, "QPC timer should be monotonic");
    }
}

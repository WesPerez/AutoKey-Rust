use crate::config::{Config, KeyConfig, KEY_SLOT_COUNT, MAX_DELAY_MS};
use crate::input::InputTarget;
use crate::{humanizer, input, window, AppCommand};
use anyhow::{Context, Result};
use parking_lot::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

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
            .name(crate::obfuscate::random_thread_name())
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
        // Restore timer resolution on exit
        unsafe {
            let _ = timeEndPeriod(1);
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
                &is_running,
                &key_running,
                &bound_window,
                &status,
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
    is_running: &AtomicBool,
    key_running: &RwLock<Vec<bool>>,
    bound_window: &RwLock<Option<isize>>,
    status: &RwLock<String>,
) -> Control {
    let stop = Arc::new(AtomicBool::new(false));
    let worker_count = keys.len();
    let (done_tx, done_rx) = std::sync::mpsc::channel();

    thread::scope(|scope| {
        for key in keys {
            let stop = stop.clone();
            let done_tx = done_tx.clone();
            set_key_running(key_running, key.index, true);
            scope.spawn(move || {
                run_independent_key(config, &key, &stop, bound_window, status);
                set_key_running(key_running, key.index, false);
                let _ = done_tx.send(());
            });
        }
        drop(done_tx);

        let mut remaining = worker_count;
        let mut control = Control::Continue;
        while remaining > 0 {
            loop {
                match done_rx.try_recv() {
                    Ok(()) => remaining = remaining.saturating_sub(1),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        remaining = 0;
                        break;
                    }
                }
            }
            if remaining == 0 {
                break;
            }

            match commands.recv_timeout(Duration::from_millis(20)) {
                Ok(AppCommand::Start) => {}
                Ok(AppCommand::Stop) | Ok(AppCommand::ToggleRunning) => {
                    stop.store(true, Ordering::Release);
                    is_running.store(false, Ordering::Release);
                    *status.write() = "已停止".to_owned();
                    control = Control::Stop;
                    break;
                }
                Ok(AppCommand::Exit) | Err(RecvTimeoutError::Disconnected) => {
                    stop.store(true, Ordering::Release);
                    control = Control::Exit;
                    break;
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
        stop.store(true, Ordering::Release);
        control
    })
}

fn run_independent_key(
    config: &Config,
    key: &RunKey,
    stop: &AtomicBool,
    bound_window: &RwLock<Option<isize>>,
    status: &RwLock<String>,
) {
    let mut completed = 0u32;
    while !stop.load(Ordering::Acquire) {
        match perform_press_until_stopped(key, stop, bound_window) {
            Ok(true) => break,
            Ok(false) => {}
            Err(error) => {
                handle_send_error(key, error, bound_window, status);
                if config.max_loops >= 0 || wait_for_stop(RECOVERY_DELAY, stop) {
                    break;
                }
                continue;
            }
        }

        if wait_for_stop(calculate_delay(config, key), stop) {
            break;
        }
        completed = completed.saturating_add(1);
        if reached_limit(config.max_loops, completed) {
            break;
        }
    }
}

fn perform_press_until_stopped(
    key: &RunKey,
    stop: &AtomicBool,
    bound_window: &RwLock<Option<isize>>,
) -> Result<bool> {
    let pre_press = Duration::from_millis(humanizer::next_pre_press_delay(key.index) as u64);
    if wait_for_stop(pre_press, stop) {
        return Ok(true);
    }

    let target = match *bound_window.read() {
        Some(hwnd) => InputTarget::Window(hwnd),
        None => InputTarget::Foreground,
    };
    input::key_down(target, key.config.vk_code)?;
    let stopped = wait_for_stop(
        Duration::from_millis(humanizer::next_press_duration() as u64),
        stop,
    );
    input::key_up(target, key.config.vk_code)?;
    Ok(stopped)
}

/// High-precision wait with hybrid sleep + busy-wait strategy.
/// Uses thread::sleep for the bulk of the duration, then busy-waits
/// using QueryPerformanceCounter for the final ~0.5ms to achieve
/// sub-millisecond accuracy.
fn wait_for_stop(duration: Duration, stop: &AtomicBool) -> bool {
    if duration.is_zero() {
        return stop.load(Ordering::Acquire);
    }

    let deadline_ns = HI_RES_TIMER
        .now_ns()
        .saturating_add(duration.as_nanos() as u64);

    // Sleep for most of the duration, leaving 0.5ms for busy-wait
    let sleep_duration = duration.saturating_sub(Duration::from_micros(500));
    if !sleep_duration.is_zero() {
        thread::sleep(sleep_duration);
    }

    // Busy-wait for the remaining time using QPC
    loop {
        if stop.load(Ordering::Acquire) {
            return true;
        }
        if HI_RES_TIMER.now_ns() >= deadline_ns {
            return false;
        }
        std::hint::spin_loop();
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

            let control =
                wait_interruptible(calculate_delay(config, key), commands, is_running, status);
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
    let pre_press = Duration::from_millis(humanizer::next_pre_press_delay(key.index) as u64);
    let control = wait_interruptible(pre_press, commands, is_running, status);
    if control != Control::Continue {
        return Ok(control);
    }

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

fn wait_interruptible(
    duration: Duration,
    commands: &Receiver<AppCommand>,
    is_running: &AtomicBool,
    status: &RwLock<String>,
) -> Control {
    if duration.is_zero() {
        return Control::Continue;
    }

    match commands.recv_timeout(duration) {
        Err(RecvTimeoutError::Timeout) => Control::Continue,
        Err(RecvTimeoutError::Disconnected) => Control::Exit,
        Ok(AppCommand::Exit) => Control::Exit,
        Ok(AppCommand::Stop) | Ok(AppCommand::ToggleRunning) => {
            is_running.store(false, Ordering::Release);
            *status.write() = "已停止".to_owned();
            Control::Stop
        }
        Ok(AppCommand::Start) => Control::Continue,
    }
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

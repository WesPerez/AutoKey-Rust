use crate::config;
use std::fs;
use std::io::Write;

pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| log_error("panic", info)));
}

pub fn log_startup(started_by_autostart: bool) {
    let executable = std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|error| format!("<unknown: {error}>"));
    let arguments = std::env::args_os()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    log_event(
        "startup",
        &format!(
            "version={} commit={} autostart={} exe={} args={}",
            env!("CARGO_PKG_VERSION"),
            env!("AUTOKEY_BUILD_GIT_HASH"),
            started_by_autostart,
            executable,
            arguments
        ),
    );
}

pub fn log_event(source: &str, message: &str) {
    write_log("app.log", source, message);
}

pub fn log_error(source: &str, error: &dyn std::fmt::Display) {
    write_log("error.log", source, &error.to_string());
}

fn write_log(file_name: &str, source: &str, message: &str) {
    let result = (|| -> std::io::Result<()> {
        let directory = config::app_directory().join(&crate::obfstr!("logs"));
        fs::create_dir_all(&directory)?;
        let path = directory.join(file_name);
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        let line = format!(
            "[{:?}] pid={} {source}: {message}\n",
            std::time::SystemTime::now(),
            std::process::id()
        );
        file.write_all(line.as_bytes())
    })();
    let _ = result;
}

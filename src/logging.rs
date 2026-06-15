use crate::config;
use std::fs;
use std::io::Write;

pub fn log_error(source: &str, error: &dyn std::fmt::Display) {
    let result = (|| -> std::io::Result<()> {
        let directory = config::app_directory().join(&crate::obfstr!("logs"));
        fs::create_dir_all(&directory)?;
        let path = directory.join(&crate::obfstr!("error.log"));
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        writeln!(
            file,
            "[{:?}] {source}: {error}",
            std::time::SystemTime::now()
        )
    })();
    let _ = result;
}

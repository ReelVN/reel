//! Format runtime diagnostics before crossing the platform stderr boundary.
use std::{cell::RefCell, fmt::Write as _, io::Write as _};

pub(super) fn log(arguments: std::fmt::Arguments<'_>) {
    let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::SessionDiagnostics);
    thread_local! {
        static LINE: RefCell<String> = RefCell::new(String::with_capacity(2048));
    }
    LINE.with_borrow_mut(|line| {
        line.clear();
        line.write_fmt(arguments)
            .expect("format runtime diagnostic");
        line.push('\n');
        let _ = std::io::stderr().lock().write_all(line.as_bytes());
    });
}

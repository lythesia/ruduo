use async_logging::AsyncLogger;
use once_cell::sync::OnceCell;

static LOGGER: OnceCell<AsyncLogger> = OnceCell::new();

pub fn logger() -> &'static AsyncLogger {
    LOGGER.get().expect("LOGGER not initialized")
}

unsafe fn bad_cast<T>(r: &T) -> &mut T {
    let const_ptr = r as *const T;
    let mut_ptr = const_ptr as *mut T;
    &mut *mut_ptr
}

pub fn set_logger(logger: AsyncLogger) {
    LOGGER.set(logger).map_err(|_| eprintln!("failed to set LOGGER")).unwrap();
}

pub fn start() {
    if let Some(logger) = LOGGER.get() {
        unsafe {
            bad_cast(logger).start();
        }
    }
}

pub fn stop() {
    if let Some(logger) = LOGGER.get() {
        unsafe {
            bad_cast(logger).stop();
        }
    }
}

mod macros {
    #[macro_export]
    macro_rules! error {
        ($fmt:expr, $($msg:expr),* $(,)?) => { ::async_logging::error![$crate::logger::logger(), $fmt, $($msg),*] };
        ($fmt:expr $(,)?) => { ::async_logging::error![$crate::logger::logger(), $fmt] };
    }

    #[macro_export]
    macro_rules! warn {
        ($fmt:expr, $($msg:expr),* $(,)?) => { ::async_logging::warn![$crate::logger::logger(), $fmt, $($msg),*] };
        ($fmt:expr $(,)?) => { ::async_logging::warn![$crate::logger::logger(), $fmt] };
    }

    #[macro_export]
    macro_rules! info {
        ($fmt:expr, $($msg:expr),* $(,)?) => { ::async_logging::info![$crate::logger::logger(), $fmt, $($msg),*] };
        ($fmt:expr $(,)?) => { ::async_logging::info![$crate::logger::logger(), $fmt] };
    }

    #[macro_export]
    macro_rules! debug {
        ($fmt:expr, $($msg:expr),* $(,)?) => { ::async_logging::debug![$crate::logger::logger(), $fmt, $($msg),*] };
        ($fmt:expr $(,)?) => { ::async_logging::debug![$crate::logger::logger(), $fmt] };
    }

    #[macro_export]
    macro_rules! trace {
        ($fmt:expr, $($msg:expr),* $(,)?) => { ::async_logging::trace![$crate::logger::logger(), $fmt, $($msg),*] };
        ($fmt:expr $(,)?) => { ::async_logging::trace![$crate::logger::logger(), $fmt] };
    }
}

#[cfg(test)]
mod tests {
    use super::{set_logger, start, stop, AsyncLogger};
    use crate::{error, trace};

    #[test]
    fn test_wrap_logger() {
        let async_logger = AsyncLogger::new(
            "/tmp/ruduo".into(),
            4096,
            2,
        ).unwrap();
        set_logger(async_logger);
        start();

        error!("err1");
        let s = "n/a";
        error!("err2: {}", s);
        trace!("Ahh");

        stop();
    }
}
//! Writing to standard error and standard output.
//!
//! Every message the command line writes goes through [`stderr`] or
//! [`stdout`] (by way of the `errln!` and `outln!` macros), never
//! `eprintln!` or `println!`, which panic when the stream is closed. A
//! reader that stops early, such as `folio check 2>&1 | head`, closes the
//! pipe: the write then fails with `BrokenPipe`, and the command goes on
//! quietly, writing nothing more to that stream, and exits with the status
//! of what it did (SPEC §12.1), not 101, which means a crash.

use std::fmt;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether standard error failed a write, after which nothing more is
/// written to it.
static STDERR_CLOSED: AtomicBool = AtomicBool::new(false);
/// Whether standard output failed a write.
static STDOUT_CLOSED: AtomicBool = AtomicBool::new(false);

/// Writes to standard error, unless a write to it has failed.
pub(crate) fn stderr(args: fmt::Arguments<'_>) {
    write(&mut io::stderr().lock(), &STDERR_CLOSED, args);
}

/// Writes to standard output, unless a write to it has failed.
pub(crate) fn stdout(args: fmt::Arguments<'_>) {
    write(&mut io::stdout().lock(), &STDOUT_CLOSED, args);
}

/// Writes `args` to `stream` and flushes it, unless `closed` says the
/// reader went away. A write that fails with `BrokenPipe` sets `closed`, and
/// nothing more is written to the stream. Any other failure loses only this
/// message: a stream that cannot take a message cannot take a report of the
/// failure either, and a later one may go through.
fn write(stream: &mut dyn Write, closed: &AtomicBool, args: fmt::Arguments<'_>) {
    if closed.load(Ordering::Relaxed) {
        return;
    }
    let result = stream.write_fmt(args).and_then(|()| stream.flush());
    if result.is_err_and(|e| e.kind() == io::ErrorKind::BrokenPipe) {
        closed.store(true, Ordering::Relaxed);
    }
}

/// Writes a line to standard error, as `eprintln!` does, through
/// [`stderr`].
macro_rules! errln {
    ($($arg:tt)*) => {
        $crate::cli::out::stderr(format_args!("{}\n", format_args!($($arg)*)))
    };
}

/// Writes a line to standard output, as `println!` does, through
/// [`stdout`].
macro_rules! outln {
    ($($arg:tt)*) => {
        $crate::cli::out::stdout(format_args!("{}\n", format_args!($($arg)*)))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stream whose writes fail with `kind`.
    struct Failing(io::ErrorKind);

    impl Write for Failing {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(self.0.into())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_closed_stream_is_left_alone() {
        let closed = AtomicBool::new(false);
        write(
            &mut Failing(io::ErrorKind::WouldBlock),
            &closed,
            format_args!("lost"),
        );
        assert!(!closed.load(Ordering::Relaxed));
        write(
            &mut Failing(io::ErrorKind::BrokenPipe),
            &closed,
            format_args!("lost"),
        );
        assert!(closed.load(Ordering::Relaxed));

        let mut written = Vec::new();
        write(&mut written, &closed, format_args!("not written"));
        assert!(written.is_empty());

        let open = AtomicBool::new(false);
        write(&mut written, &open, format_args!("{} {}", "written", 1));
        assert_eq!(written, b"written 1");
        assert!(!open.load(Ordering::Relaxed));
    }
}

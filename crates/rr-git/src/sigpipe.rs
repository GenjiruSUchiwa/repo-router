//! Temporarily ignore SIGPIPE while a Git clean filter is running.
//!
//! `rr` restores a terminating SIGPIPE disposition so a consumer that stops
//! reading kills the process quietly. That disposition is process-wide: it
//! also applies to the pipe `gix-filter` writes a blob into. A driver that
//! closes stdin early — `head -c 8` is the usual example — is survivable
//! there (`gix-filter` treats the resulting `EPIPE` as success when the
//! driver itself exited 0). Our disposition turned that write into a kill,
//! so the run died with 141 and nothing to report.
//!
//! The ignore is refcounted because convert-to-Git runs on the rayon pool:
//! the first caller installs `SIG_IGN`, the last restores whatever was
//! there. Nested and overlapping calls therefore cannot restore a
//! terminating disposition under a sibling that is still writing.

#![allow(unsafe_code)]

/// Holds the process-wide ignore for the duration of one convert-to-Git call.
pub(crate) struct Ignore {
    _private: (),
}

/// Ignores SIGPIPE until the returned guard is dropped.
pub(crate) fn ignore() -> Ignore {
    #[cfg(unix)]
    unix::acquire();
    Ignore { _private: () }
}

impl Drop for Ignore {
    fn drop(&mut self) {
        #[cfg(unix)]
        unix::release();
    }
}

#[cfg(unix)]
mod unix {
    use std::sync::Mutex;

    struct State {
        depth: usize,
        previous: libc::sighandler_t,
    }

    static STATE: Mutex<State> = Mutex::new(State {
        depth: 0,
        previous: 0,
    });

    pub(super) fn acquire() {
        let Ok(mut state) = STATE.lock() else {
            return;
        };
        if state.depth == 0 {

            state.previous = unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };
        }
        state.depth += 1;
    }

    pub(super) fn release() {
        let Ok(mut state) = STATE.lock() else {
            return;
        };
        state.depth = state.depth.saturating_sub(1);
        if state.depth == 0 {

            unsafe {
                libc::signal(libc::SIGPIPE, state.previous);
            }
        }
    }
}

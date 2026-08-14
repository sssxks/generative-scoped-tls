//! An intentionally unsound caller of `ScopedKey::set`.
//!
//! This is an expected-failure Miri fixture, not a program to run natively.

use generative_scoped_tls::{scoped, scoped_thread_local};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll, Waker};

struct Context {
    answer: u32,
}

scoped_thread_local!(static CX: Context);

struct YieldOnce(bool);

impl Future for YieldOnce {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        if self.0 {
            Poll::Ready(())
        } else {
            self.0 = true;
            Poll::Pending
        }
    }
}

async fn read_after_suspending() -> u32 {
    scoped!(let cx = CX);
    YieldOnce(false).await;
    cx.answer
}

fn poll<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
    let mut task_context = TaskContext::from_waker(Waker::noop());
    future.poll(&mut task_context)
}

fn main() {
    let mut future = Box::pin(read_after_suspending());

    let value = Box::new(Context { answer: 42 });
    let poll_until_suspended = || {
        assert!(poll(future.as_mut()).is_pending());
    };

    // CONTRACT VIOLATION: the suspended future retains a reference obtained
    // from CX after this invocation of `set` returns.
    unsafe { CX.set(&value, poll_until_suspended) };

    drop(value);

    // `YieldOnce` now completes, so the future dereferences its dangling `cx`.
    // Miri must reject this access as use-after-free.
    let _ = poll(future.as_mut());
}

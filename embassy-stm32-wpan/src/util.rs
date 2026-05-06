//! Crate utils

use core::future::poll_fn;
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::Poll;

use embassy_sync::waitqueue::AtomicWaker;

#[allow(unused)]
pub struct Flag {
    state: AtomicBool,
    waker: AtomicWaker,
}

#[allow(unused)]
impl Flag {
    pub const fn new(state: bool) -> Self {
        Self {
            state: AtomicBool::new(state),
            waker: AtomicWaker::new(),
        }
    }

    pub fn set_high(&self) {
        if !self.state.swap(true, Ordering::AcqRel) {
            self.waker.wake();
        }
    }

    pub fn set_low(&self) {
        if self.state.swap(false, Ordering::AcqRel) {
            self.waker.wake();
        }
    }

    pub async fn wait_for_high(&self) {
        poll_fn(|cx| {
            self.waker.register(cx.waker());

            if !self.state.load(Ordering::Acquire) {
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })
        .await;
    }

    pub async fn wait_for_low(&self) {
        poll_fn(|cx| {
            self.waker.register(cx.waker());

            if self.state.load(Ordering::Acquire) {
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })
        .await;
    }
}

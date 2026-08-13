//! The one clock primitive the UI uses, on the browser's event loop.

use std::time::Duration;

pub async fn sleep(d: Duration) {
    gloo_timers::future::TimeoutFuture::new(d.as_millis().min(u32::MAX as u128) as u32).await;
}

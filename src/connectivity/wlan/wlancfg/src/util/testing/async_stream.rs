// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#![cfg(test)]

use fuchsia_async::{self as fasync, TimeoutExt};

use futures::future::Either;
use futures::prelude::*;
use futures::stream::StreamFuture;
use futures::task::Poll;
use std::pin::pin;

/// Run a background task while waiting for a future that should occur.
/// This is useful for running a task which you expect to produce side effects that
/// mean the task is operating correctly. i.e. reacting to a peer action by producing a
/// response on a client's hanging get.
/// `background_fut` is expected not to finish. If it finishes, this function will panic.
/// Cribbed from bluetooth at src/connectivity/bluetooth/lib/async-helpers/src/test/lib.rs
#[track_caller]
pub fn run_while<BackgroundFut, ResultFut, Out>(
    exec: &mut fasync::TestExecutor,
    background_fut: &mut BackgroundFut,
    result_fut: ResultFut,
) -> Out
where
    BackgroundFut: Future + Unpin,
    ResultFut: Future<Output = Out>,
{
    let result_fut = pin!(result_fut);

    // Set an arbitrary timeout to catch the case where `result_fut` never provides a result.
    // Even a few milliseconds should be sufficient on all but the slowest hardware.
    const RESULT_TIMEOUT: zx::MonotonicDuration = zx::MonotonicDuration::from_seconds(5);
    let result_fut_with_timeout = pin!(result_fut.on_timeout(RESULT_TIMEOUT, || {
        panic!("Future failed to produce a result within {} seconds", RESULT_TIMEOUT.into_seconds())
    }));

    // Advance both futures, with the expectation that only `result_fut` will finish.
    let mut select_fut = futures::future::select(background_fut, result_fut_with_timeout);
    match exec.run_singlethreaded(pin!(select_fut)) {
        Either::Left(_) => panic!("Background future finished"),
        Either::Right((result, _background_fut)) => result,
    }
}

#[track_caller]
pub fn poll_sme_req(
    exec: &mut fasync::TestExecutor,
    next_sme_req: &mut StreamFuture<fidl_fuchsia_wlan_sme::ClientSmeRequestStream>,
) -> Poll<fidl_fuchsia_wlan_sme::ClientSmeRequest> {
    exec.run_until_stalled(next_sme_req).map(|(req, stream)| {
        *next_sme_req = stream.into_future();
        req.expect("did not expect the SME request stream to end")
            .expect("error polling SME request stream")
    })
}

#[track_caller]
pub fn poll_ap_sme_req(
    exec: &mut fasync::TestExecutor,
    next_sme_req: &mut StreamFuture<fidl_fuchsia_wlan_sme::ApSmeRequestStream>,
) -> Poll<fidl_fuchsia_wlan_sme::ApSmeRequest> {
    exec.run_until_stalled(next_sme_req).map(|(req, stream)| {
        *next_sme_req = stream.into_future();
        req.expect("did not expect the SME request stream to end")
            .expect("error polling SME request stream")
    })
}

mod tests {
    use super::*;
    use fuchsia_async as fasync;

    #[fuchsia::test]
    fn test_run_while() {
        let mut exec = fasync::TestExecutor::new();
        let neverending_background_fut: future::Pending<bool> = future::pending();
        let mut neverending_background_fut = pin!(neverending_background_fut);

        // You can directly pass in the future
        let result_fut = future::ready(1);
        let result = run_while(&mut exec, &mut neverending_background_fut, result_fut);
        assert_eq!(result, 1);

        // You can pass in a reference to the future
        let result_fut = future::ready(1);
        let result = run_while(&mut exec, &mut neverending_background_fut, result_fut);
        assert_eq!(result, 1);
    }
}

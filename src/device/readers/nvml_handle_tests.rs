// Copyright 2025 Lablup Inc. and Jeongkyu Shin
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! State-machine tests for the shared NVML slot. They drive [`NvmlSlot`]
//! with a fake handle, a counting init function, and explicit instants, so
//! no GPU is needed.

use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Stand-in for `Nvml`: an id to tell handles apart and a liveness flag the
/// test flips to simulate a driver reload.
struct FakeHandle {
    id: usize,
    live: AtomicBool,
}

/// An init function whose result the test controls, counting its calls.
struct FakeInit {
    calls: AtomicUsize,
    succeed: AtomicBool,
}

impl FakeInit {
    fn new(succeed: bool) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            succeed: AtomicBool::new(succeed),
        }
    }

    fn init(&self) -> Result<FakeHandle, String> {
        let id = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if self.succeed.load(Ordering::SeqCst) {
            Ok(FakeHandle {
                id,
                live: AtomicBool::new(true),
            })
        } else {
            Err("Driver Not Loaded".to_string())
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn set_succeed(&self, succeed: bool) {
        self.succeed.store(succeed, Ordering::SeqCst);
    }
}

fn get(
    slot: &NvmlSlot<FakeHandle>,
    fake: &FakeInit,
    now: Instant,
) -> Result<Arc<FakeHandle>, SharedNvmlError> {
    slot.get(|| fake.init(), |h| h.live.load(Ordering::SeqCst), now)
}

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

#[test]
fn failed_init_runs_once_per_backoff_window() {
    let slot = NvmlSlot::new();
    let fake = FakeInit::new(false);
    let t0 = Instant::now();

    for i in 0..1_000u64 {
        // Spread the calls across the first window, as a poll loop would.
        let now = t0 + Duration::from_millis(i * 29);
        let err = get(&slot, &fake, now).err().expect("init must fail");
        assert_eq!(err.to_string(), "Driver Not Loaded");
    }
    assert_eq!(fake.calls(), 1, "init ran again inside the first window");

    // The window ends 30 s after the failure: one more attempt, then quiet.
    for _ in 0..1_000 {
        assert!(get(&slot, &fake, t0 + INITIAL_BACKOFF).is_err());
    }
    assert_eq!(fake.calls(), 2);
}

#[test]
fn backoff_doubles_and_caps() {
    let slot = NvmlSlot::new();
    let fake = FakeInit::new(false);
    let mut now = Instant::now();
    let mut windows = Vec::new();

    assert!(get(&slot, &fake, now).is_err());
    for _ in 0..7 {
        // Find the window length by probing one second before it ends and
        // at its end.
        let calls = fake.calls();
        let mut wait = secs(1);
        loop {
            let _ = get(&slot, &fake, now + wait);
            if fake.calls() > calls {
                break;
            }
            wait += secs(1);
        }
        windows.push(wait.as_secs());
        now += wait;
    }
    assert_eq!(windows, vec![30, 60, 120, 240, 300, 300, 300]);
    assert_eq!(MAX_BACKOFF, secs(300));
}

#[test]
fn recovers_after_backoff_and_then_stops_initialising() {
    let slot = NvmlSlot::new();
    let fake = FakeInit::new(false);
    let t0 = Instant::now();

    assert!(get(&slot, &fake, t0).is_err());
    fake.set_succeed(true);
    // Still inside the window: NVML is not tried even though it would work.
    assert!(get(&slot, &fake, t0 + secs(29)).is_err());
    assert_eq!(fake.calls(), 1);

    let handle = get(&slot, &fake, t0 + secs(30)).expect("init succeeds");
    assert_eq!(handle.id, 2);
    for i in 0..1_000 {
        let again = get(&slot, &fake, t0 + secs(31 + i)).expect("cached");
        assert!(Arc::ptr_eq(&handle, &again));
    }
    assert_eq!(fake.calls(), 2, "a working handle was re-initialised");
}

#[test]
fn success_resets_the_backoff() {
    let slot = NvmlSlot::new();
    let fake = FakeInit::new(false);
    let t0 = Instant::now();

    // Fail twice so the window grows to 60 s.
    assert!(get(&slot, &fake, t0).is_err());
    assert!(get(&slot, &fake, t0 + secs(30)).is_err());
    fake.set_succeed(true);
    let handle = get(&slot, &fake, t0 + secs(90)).expect("recovers");

    // The driver goes away: the stale handle is dropped, re-init fails, and
    // the next window starts again at 30 s rather than 120 s.
    fake.set_succeed(false);
    handle.live.store(false, Ordering::SeqCst);
    drop(handle);
    let t1 = t0 + secs(100);
    assert!(get(&slot, &fake, t1).is_err());
    let calls = fake.calls();
    assert!(get(&slot, &fake, t1 + secs(29)).is_err());
    assert_eq!(fake.calls(), calls);
    assert!(get(&slot, &fake, t1 + secs(30)).is_err());
    assert_eq!(fake.calls(), calls + 1);
}

#[test]
fn stale_handle_is_replaced_once() {
    let slot = NvmlSlot::new();
    let fake = FakeInit::new(true);
    let now = Instant::now();

    let first = get(&slot, &fake, now).expect("init succeeds");
    assert_eq!(fake.calls(), 1);
    first.live.store(false, Ordering::SeqCst);

    let second = get(&slot, &fake, now).expect("re-init succeeds");
    assert_eq!(second.id, 2);
    assert_eq!(fake.calls(), 2);
    // The caller that still holds the old handle keeps it.
    assert_eq!(first.id, 1);

    for _ in 0..100 {
        let again = get(&slot, &fake, now).expect("cached");
        assert!(Arc::ptr_eq(&second, &again));
    }
    assert_eq!(fake.calls(), 2);
}

#[test]
fn stale_handle_with_failing_reinit_enters_backoff() {
    let slot = NvmlSlot::new();
    let fake = FakeInit::new(true);
    let t0 = Instant::now();

    let handle = get(&slot, &fake, t0).expect("init succeeds");
    handle.live.store(false, Ordering::SeqCst);
    fake.set_succeed(false);

    let err = get(&slot, &fake, t0).err().expect("re-init fails");
    assert_eq!(err.to_string(), "Driver Not Loaded");
    assert_eq!(fake.calls(), 2);
    for _ in 0..100 {
        assert!(get(&slot, &fake, t0 + secs(10)).is_err());
    }
    assert_eq!(fake.calls(), 2);
}

#[test]
fn invalidate_only_clears_the_handle_it_was_given() {
    let slot = NvmlSlot::new();
    let fake = FakeInit::new(true);
    let now = Instant::now();

    let old = get(&slot, &fake, now).expect("init succeeds");
    assert!(slot.invalidate(&old));
    let new = get(&slot, &fake, now).expect("re-init succeeds");
    assert!(!Arc::ptr_eq(&old, &new));

    // A second caller that saw the same stale handle must not throw away
    // the replacement another caller already installed.
    assert!(!slot.invalidate(&old));
    let current = get(&slot, &fake, now).expect("cached");
    assert!(Arc::ptr_eq(&new, &current));
    assert_eq!(fake.calls(), 2);
}

#[test]
fn concurrent_callers_share_one_failed_attempt() {
    let slot = NvmlSlot::new();
    let fake = FakeInit::new(false);
    let now = Instant::now();

    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                for _ in 0..200 {
                    assert!(get(&slot, &fake, now).is_err());
                }
            });
        }
    });
    assert_eq!(fake.calls(), 1);
}

#[test]
fn dead_fresh_handle_is_a_failed_init_with_backoff() {
    let slot = NvmlSlot::new();
    let fake = FakeInit::new(true);
    let t0 = Instant::now();
    let never_live = |_: &FakeHandle| false;

    let err = slot
        .get(|| fake.init(), never_live, t0)
        .err()
        .expect("dead handle must be rejected");
    assert!(err.to_string().contains("device_count() failed"));
    assert!(fake.calls() <= 2);
    let calls = fake.calls();

    for i in 0..1_000u64 {
        let now = t0 + Duration::from_millis(i * 29);
        assert!(slot.get(|| fake.init(), never_live, now).is_err());
    }
    assert_eq!(fake.calls(), calls, "init ran again inside the back-off");
    assert!(!slot.has_handle());
}

#[test]
fn dead_fresh_handle_recovers_once_live_after_the_window() {
    let slot = NvmlSlot::new();
    let fake = FakeInit::new(true);
    let t0 = Instant::now();
    let live = AtomicBool::new(false);
    let is_live = |_: &FakeHandle| live.load(Ordering::SeqCst);

    assert!(slot.get(|| fake.init(), is_live, t0).is_err());
    let calls = fake.calls();
    live.store(true, Ordering::SeqCst);
    // Still inside the window: no init even though the handle would be live.
    assert!(slot.get(|| fake.init(), is_live, t0 + secs(29)).is_err());
    assert_eq!(fake.calls(), calls);

    let handle = slot
        .get(|| fake.init(), is_live, t0 + secs(30))
        .expect("recovers after the window");
    assert_eq!(fake.calls(), calls + 1);
    let again = slot
        .get(|| fake.init(), is_live, t0 + secs(31))
        .expect("cached");
    assert!(Arc::ptr_eq(&handle, &again));
    assert_eq!(fake.calls(), calls + 1);
}

#[test]
fn library_pin_opens_once_and_is_never_reopened() {
    let pin = LibraryPin::<()>::new();
    let opens = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                for _ in 0..100 {
                    assert!(pin.ensure(|| {
                        opens.fetch_add(1, Ordering::SeqCst);
                        Some(())
                    }));
                }
            });
        }
    });
    assert_eq!(opens.load(Ordering::SeqCst), 1);
}

#[test]
fn library_pin_retries_after_failure_then_holds() {
    let pin = LibraryPin::<()>::new();
    let attempts = AtomicUsize::new(0);
    for _ in 0..5 {
        assert!(!pin.ensure(|| {
            attempts.fetch_add(1, Ordering::SeqCst);
            None
        }));
    }
    assert_eq!(attempts.load(Ordering::SeqCst), 5);

    // The library becomes loadable: pinned once, never retried after.
    for _ in 0..100 {
        assert!(pin.ensure(|| {
            attempts.fetch_add(1, Ordering::SeqCst);
            Some(())
        }));
    }
    assert_eq!(attempts.load(Ordering::SeqCst), 6);
}

#[test]
fn real_pin_stops_reopening_once_pinned() {
    // Runs on any host: with a driver the first call pins and later calls
    // do not reopen; without one every call is a fresh attempt.
    let before = pin_open_attempts();
    let mut pinned = false;
    for _ in 0..100 {
        pinned = pin_library();
    }
    let made = pin_open_attempts() - before;
    if pinned {
        assert!(made <= 1);
    } else {
        assert_eq!(made, 100);
    }
}

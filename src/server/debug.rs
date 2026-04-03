use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::AppCtx;

pub(super) fn start_rf_debug_listener(ctx: &AppCtx, was_listening: bool) -> Arc<str> {
    if !was_listening {
        ctx.debug
            .rf_debug_listener_count
            .fetch_add(1, Ordering::SeqCst);
        ctx.debug.rf_debug_enabled.store(true, Ordering::SeqCst);
        super::runtime::ensure_rf_debug_worker(ctx);
    }

    ctx.rf_debug_state_json(true)
}

pub(super) fn stop_rf_debug_listener(ctx: &AppCtx, was_listening: bool) -> Arc<str> {
    release_rf_debug_listener(ctx, was_listening);
    ctx.rf_debug_state_json(false)
}

pub(super) fn clear_rf_debug_events(ctx: &AppCtx, listening: bool) -> Arc<str> {
    ctx.clear_rf_debug_events(listening)
}

pub(super) fn release_rf_debug_listener(ctx: &AppCtx, was_listening: bool) {
    if !was_listening {
        return;
    }

    let previous = ctx
        .debug
        .rf_debug_listener_count
        .fetch_sub(1, Ordering::SeqCst);
    if previous <= 1 {
        ctx.debug.rf_debug_enabled.store(false, Ordering::SeqCst);
    }
}

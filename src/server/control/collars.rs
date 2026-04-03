use crate::protocol::Collar;
use crate::server::{AppCtx, ControlResult};

pub(super) fn add(ctx: &AppCtx, name: String, collar_id: u16, channel: u8) -> ControlResult {
    ctx.add_collar(Collar {
        name,
        collar_id,
        channel,
    })
}

pub(super) fn update(
    ctx: &AppCtx,
    original_name: String,
    name: String,
    collar_id: u16,
    channel: u8,
) -> ControlResult {
    ctx.update_collar(
        original_name,
        Collar {
            name,
            collar_id,
            channel,
        },
    )
}

pub(super) fn delete(ctx: &AppCtx, name: String) -> ControlResult {
    ctx.delete_collar(name)
}

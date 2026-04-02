pub const MAX_INTENSITY: u8 = 99;
pub const MAX_CHANNEL: u8 = 2;

pub fn encode_rf_frame(collar_id: u16, channel: u8, mode: u8, intensity: u8) -> [u8; 5] {
    assert!(
        intensity <= MAX_INTENSITY,
        "intensity {intensity} exceeds MAX_INTENSITY {MAX_INTENSITY}"
    );
    let b0 = (collar_id >> 8) as u8;
    let b1 = (collar_id & 0xFF) as u8;
    let b2 = (channel << 4) | (mode & 0x0F);
    let b3 = intensity;
    let b4 = b0.wrapping_add(b1).wrapping_add(b2).wrapping_add(b3);
    [b0, b1, b2, b3, b4]
}

pub fn decode_rf_frame(raw: &[u8; 5]) -> (u16, u8, u8, u8, bool) {
    let collar_id = u16::from(raw[0]) << 8 | u16::from(raw[1]);
    let channel = raw[2] >> 4;
    let mode_raw = raw[2] & 0x0F;
    let intensity = raw[3];
    let checksum_ok = raw[4]
        == raw[0]
            .wrapping_add(raw[1])
            .wrapping_add(raw[2])
            .wrapping_add(raw[3]);
    (collar_id, channel, mode_raw, intensity, checksum_ok)
}

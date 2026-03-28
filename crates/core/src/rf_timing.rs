/// Type-1 collar protocol timing constants.
///
/// The transmitter sends a 5-byte payload plus two trailing zero bits,
/// repeated three times back-to-back.
pub const REPEAT_COUNT: usize = 3;
pub const FRAME_BYTES: usize = 5;
pub const FRAME_BITS: usize = FRAME_BYTES * 8 + 2;
pub const PREAMBLE_HIGH_US: u32 = 1400;
pub const PREAMBLE_LOW_US: u32 = 750;
pub const BIT_ONE_HIGH_US: u32 = 750;
pub const BIT_ONE_LOW_US: u32 = 250;
pub const BIT_ZERO_HIGH_US: u32 = 250;
pub const BIT_ZERO_LOW_US: u32 = 750;
pub const BIT_TOTAL_US: u64 = (BIT_ONE_HIGH_US + BIT_ONE_LOW_US) as u64;
pub const RF_REPEAT_DURATION_US: u64 =
    PREAMBLE_HIGH_US as u64 + PREAMBLE_LOW_US as u64 + FRAME_BITS as u64 * BIT_TOTAL_US;
pub const RF_COMMAND_TRANSMIT_DURATION_US: u64 = REPEAT_COUNT as u64 * RF_REPEAT_DURATION_US;
pub const RF_COMMAND_REPEAT_SPAN_US: u64 = (REPEAT_COUNT as u64 - 1) * RF_REPEAT_DURATION_US;
pub const RETRANSMIT_INTERVAL_US: u64 = 200_000;
pub const RF_COMMAND_COVERAGE_US: u64 = RF_COMMAND_REPEAT_SPAN_US + RETRANSMIT_INTERVAL_US;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_timings_match() {
        assert_eq!(BIT_ONE_HIGH_US as u64 + BIT_ONE_LOW_US as u64, BIT_TOTAL_US);
        assert_eq!(
            BIT_ZERO_HIGH_US as u64 + BIT_ZERO_LOW_US as u64,
            BIT_TOTAL_US
        );
    }

    #[test]
    fn derived_command_timing_matches_waveform() {
        assert_eq!(RF_REPEAT_DURATION_US, 44_150);
        assert_eq!(RF_COMMAND_REPEAT_SPAN_US, 88_300);
        assert_eq!(RF_COMMAND_TRANSMIT_DURATION_US, 132_450);
        assert_eq!(RF_COMMAND_COVERAGE_US, 288_300);
    }
}

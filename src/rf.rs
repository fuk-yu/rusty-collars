use anyhow::Result;
use esp_idf_svc::hal::delay::Ets;
use esp_idf_svc::hal::gpio::{Input, InputPin, Output, OutputPin, PinDriver, Pull};
use log::debug;
use rusty_collars_core::rf_timing::{
    BIT_ONE_HIGH_US, BIT_ONE_LOW_US, BIT_ZERO_HIGH_US, BIT_ZERO_LOW_US, FRAME_BITS, FRAME_BYTES,
    PREAMBLE_HIGH_US, PREAMBLE_LOW_US, REPEAT_COUNT,
};

use crate::protocol::{self, CommandMode, RfDebugFrame};

/// Type 1 collar protocol encoder using direct GPIO + busy-wait delay.
///
/// Protocol: 433 MHz OOK, 5-byte payload sent MSB-first, repeated 3 times.
/// Preamble: HIGH 1400us, LOW 750us
/// Bit 1:    HIGH 750us,  LOW 250us
/// Bit 0:    HIGH 250us,  LOW 750us
/// Payload:  [id_hi:8][id_lo:8][channel:4|mode:4][intensity:8][checksum:8]
/// Tail:     2x bit-0
const PREAMBLE_HIGH_TOLERANCE_US: u32 = 350;
const PREAMBLE_LOW_TOLERANCE_US: u32 = 180;
const BIT_TOLERANCE_US: u32 = 180;
const FRAME_GAP_TIMEOUT_US: u64 = 5000;
const DUPLICATE_FRAME_WINDOW_US: u64 = 100000;
const IDLE_POLL_SLEEP_US: u64 = 200;

pub struct RfTransmitter {
    pin: PinDriver<'static, Output>,
}

impl RfTransmitter {
    pub fn new(pin: impl OutputPin + 'static) -> Result<Self> {
        let pin = PinDriver::output(pin)?;
        Ok(Self { pin })
    }

    pub fn send_command(
        &mut self,
        collar_id: u16,
        channel: u8,
        mode: u8,
        intensity: u8,
    ) -> Result<()> {
        debug!(
            "TX: id=0x{:04X} ch={} mode={} intensity={}",
            collar_id, channel, mode, intensity
        );

        let payload = protocol::encode_rf_frame(collar_id, channel, mode, intensity);

        // Raise to max priority during bitbanging to prevent WiFi/system
        // task preemption causing timing jitter in the RF signal.
        let task = unsafe { esp_idf_svc::sys::xTaskGetCurrentTaskHandle() };
        let original_priority = unsafe { esp_idf_svc::sys::uxTaskPriorityGet(task) };
        unsafe {
            esp_idf_svc::sys::vTaskPrioritySet(
                task,
                esp_idf_svc::sys::configMAX_PRIORITIES as u32 - 1,
            );
        }

        let result = self.transmit_frame(&payload);

        unsafe { esp_idf_svc::sys::vTaskPrioritySet(task, original_priority) };

        result
    }

    fn transmit_frame(&mut self, payload: &[u8; 5]) -> Result<()> {
        for _ in 0..REPEAT_COUNT {
            // Preamble
            self.pin.set_high()?;
            Ets::delay_us(PREAMBLE_HIGH_US);
            self.pin.set_low()?;
            Ets::delay_us(PREAMBLE_LOW_US);

            // Data bits (MSB first)
            for &byte in payload {
                for bit_pos in (0..8).rev() {
                    if (byte >> bit_pos) & 1 == 1 {
                        self.pin.set_high()?;
                        Ets::delay_us(BIT_ONE_HIGH_US);
                        self.pin.set_low()?;
                        Ets::delay_us(BIT_ONE_LOW_US);
                    } else {
                        self.pin.set_high()?;
                        Ets::delay_us(BIT_ZERO_HIGH_US);
                        self.pin.set_low()?;
                        Ets::delay_us(BIT_ZERO_LOW_US);
                    }
                }
            }

            // Two trailing zero bits
            for _ in 0..2 {
                self.pin.set_high()?;
                Ets::delay_us(BIT_ZERO_HIGH_US);
                self.pin.set_low()?;
                Ets::delay_us(BIT_ZERO_LOW_US);
            }
        }

        Ok(())
    }
}

#[derive(Clone, Copy)]
enum ReceivePhase {
    SeekingPreambleHigh,
    SeekingPreambleLow,
    ReadingHigh,
    ReadingLow(u32),
}

pub struct RfReceiver {
    pin: PinDriver<'static, Input>,
    phase: ReceivePhase,
    bits: [u8; FRAME_BITS],
    bit_len: usize,
    last_frame: Option<([u8; FRAME_BYTES], u64)>,
}

impl RfReceiver {
    pub fn new(pin: impl InputPin + 'static) -> Result<Self> {
        let pin = PinDriver::input(pin, Pull::Floating)?;
        Ok(Self {
            pin,
            phase: ReceivePhase::SeekingPreambleHigh,
            bits: [0; FRAME_BITS],
            bit_len: 0,
            last_frame: None,
        })
    }

    pub fn listen_until_disabled(
        &mut self,
        enabled: &std::sync::atomic::AtomicBool,
    ) -> Result<Option<RfDebugFrame>> {
        let mut last_level = self.pin.is_high();
        let mut last_edge_us = now_micros();

        while enabled.load(std::sync::atomic::Ordering::SeqCst) {
            let level = self.pin.is_high();
            let now_us = now_micros();
            if level != last_level {
                let duration_us = now_us.saturating_sub(last_edge_us).min(u32::MAX as u64) as u32;
                if let Some(frame) = self.process_pulse(last_level, duration_us, now_us) {
                    return Ok(Some(frame));
                }
                last_level = level;
                last_edge_us = now_us;
                continue;
            }

            if self.is_receiving() && now_us.saturating_sub(last_edge_us) > FRAME_GAP_TIMEOUT_US {
                self.reset();
                last_edge_us = now_us;
            }

            if !self.is_receiving() && !level {
                std::thread::sleep(std::time::Duration::from_micros(IDLE_POLL_SLEEP_US));
            } else {
                std::hint::spin_loop();
            }
        }

        self.reset();
        Ok(None)
    }

    fn process_pulse(
        &mut self,
        level: bool,
        duration_us: u32,
        pulse_end_us: u64,
    ) -> Option<RfDebugFrame> {
        match self.phase {
            ReceivePhase::SeekingPreambleHigh => {
                if level && approx_eq(duration_us, PREAMBLE_HIGH_US, PREAMBLE_HIGH_TOLERANCE_US) {
                    self.phase = ReceivePhase::SeekingPreambleLow;
                }
            }
            ReceivePhase::SeekingPreambleLow => {
                if !level && approx_eq(duration_us, PREAMBLE_LOW_US, PREAMBLE_LOW_TOLERANCE_US) {
                    self.phase = ReceivePhase::ReadingHigh;
                    self.bit_len = 0;
                } else {
                    self.resync_from_pulse(level, duration_us);
                }
            }
            ReceivePhase::ReadingHigh => {
                if level
                    && (approx_eq(duration_us, BIT_ZERO_HIGH_US, BIT_TOLERANCE_US)
                        || approx_eq(duration_us, BIT_ONE_HIGH_US, BIT_TOLERANCE_US))
                {
                    self.phase = ReceivePhase::ReadingLow(duration_us);
                } else {
                    self.resync_from_pulse(level, duration_us);
                }
            }
            ReceivePhase::ReadingLow(high_us) => {
                if !level {
                    if let Some(bit) = decode_bit(high_us, duration_us) {
                        self.push_bit(bit);
                        if self.bit_len == FRAME_BITS {
                            let frame = self.finish_frame(pulse_end_us);
                            self.reset();
                            return frame;
                        }
                        self.phase = ReceivePhase::ReadingHigh;
                    } else {
                        self.resync_from_pulse(level, duration_us);
                    }
                } else {
                    self.resync_from_pulse(level, duration_us);
                }
            }
        }

        None
    }

    fn finish_frame(&mut self, pulse_end_us: u64) -> Option<RfDebugFrame> {
        if self.bits[FRAME_BITS - 2] != 0 || self.bits[FRAME_BITS - 1] != 0 {
            return None;
        }

        let mut raw = [0u8; FRAME_BYTES];
        for (index, bit) in self.bits[..FRAME_BYTES * 8].iter().enumerate() {
            raw[index / 8] = (raw[index / 8] << 1) | *bit;
        }

        if let Some((last_raw, last_at_us)) = self.last_frame {
            if last_raw == raw
                && pulse_end_us.saturating_sub(last_at_us) <= DUPLICATE_FRAME_WINDOW_US
            {
                return None;
            }
        }
        self.last_frame = Some((raw, pulse_end_us));

        let (collar_id, channel, mode_raw, intensity, checksum_ok) =
            protocol::decode_rf_frame(&raw);

        Some(RfDebugFrame {
            received_at_ms: pulse_end_us / 1000,
            raw_hex: format!(
                "{:02X}{:02X}{:02X}{:02X}{:02X}",
                raw[0], raw[1], raw[2], raw[3], raw[4]
            ),
            collar_id,
            channel,
            mode_raw,
            mode: CommandMode::from_rf_byte(mode_raw),
            intensity,
            checksum_ok,
        })
    }

    fn push_bit(&mut self, bit: u8) {
        self.bits[self.bit_len] = bit;
        self.bit_len += 1;
    }

    fn reset(&mut self) {
        self.phase = ReceivePhase::SeekingPreambleHigh;
        self.bit_len = 0;
    }

    fn resync_from_pulse(&mut self, level: bool, duration_us: u32) {
        self.reset();
        if level && approx_eq(duration_us, PREAMBLE_HIGH_US, PREAMBLE_HIGH_TOLERANCE_US) {
            self.phase = ReceivePhase::SeekingPreambleLow;
        }
    }

    fn is_receiving(&self) -> bool {
        !matches!(self.phase, ReceivePhase::SeekingPreambleHigh)
    }
}

fn decode_bit(high_us: u32, low_us: u32) -> Option<u8> {
    if approx_eq(high_us, BIT_ONE_HIGH_US, BIT_TOLERANCE_US)
        && approx_eq(low_us, BIT_ONE_LOW_US, BIT_TOLERANCE_US)
    {
        return Some(1);
    }
    if approx_eq(high_us, BIT_ZERO_HIGH_US, BIT_TOLERANCE_US)
        && approx_eq(low_us, BIT_ZERO_LOW_US, BIT_TOLERANCE_US)
    {
        return Some(0);
    }
    None
}

fn approx_eq(actual: u32, expected: u32, tolerance: u32) -> bool {
    actual.abs_diff(expected) <= tolerance
}

fn now_micros() -> u64 {
    unsafe { esp_idf_svc::sys::esp_timer_get_time() as u64 }
}

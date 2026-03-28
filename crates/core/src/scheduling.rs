/// Retransmit interval for sustained collar activation.
/// Each RF transmission takes ~130ms (3x43ms). The collar likely stops
/// shortly after the last signal, so we retransmit frequently to maintain
/// the effect and tolerate missed frames from interference.
const RETRANSMIT_INTERVAL_MS: u64 = 200;

/// A single scheduled RF transmission event.
#[derive(Debug, Clone)]
pub struct PresetEvent {
    pub time_ms: u64,
    pub collar_id: u16,
    pub channel: u8,
    pub mode_byte: u8,
    pub intensity: u8,
}

/// Schedule RF retransmission events for a single preset step.
///
/// Transmits at start, then retransmits every RETRANSMIT_INTERVAL_MS to sustain
/// the effect. The last retransmit is scheduled so that transmission completes
/// at or before step end (no spillover into the next step).
pub fn schedule_step_events(
    events: &mut Vec<PresetEvent>,
    start_ms: u64,
    end_ms: u64,
    collar_id: u16,
    channel: u8,
    mode_byte: u8,
    intensity: u8,
) {
    if start_ms >= end_ms {
        return;
    }

    let mut t = start_ms;
    while t < end_ms {
        events.push(PresetEvent {
            time_ms: t,
            collar_id,
            channel,
            mode_byte,
            intensity,
        });
        t += RETRANSMIT_INTERVAL_MS;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn times(events: &[PresetEvent]) -> Vec<u64> {
        events.iter().map(|e| e.time_ms).collect()
    }

    #[test]
    fn very_short_step_single_event() {
        let mut events = Vec::new();
        schedule_step_events(&mut events, 0, 100, 0x1234, 0, 2, 50);
        assert_eq!(times(&events), vec![0]);
    }

    #[test]
    fn step_200ms_single_event() {
        let mut events = Vec::new();
        schedule_step_events(&mut events, 0, 200, 0x1234, 0, 2, 50);
        assert_eq!(times(&events), vec![0]);
    }

    #[test]
    fn step_201ms_two_events() {
        let mut events = Vec::new();
        schedule_step_events(&mut events, 0, 201, 0x1234, 0, 2, 50);
        assert_eq!(times(&events), vec![0, 200]);
    }

    #[test]
    fn one_second_step() {
        let mut events = Vec::new();
        schedule_step_events(&mut events, 0, 1000, 0x1234, 0, 2, 50);
        // 0, 200, 400, 600, 800
        assert_eq!(times(&events), vec![0, 200, 400, 600, 800]);
    }

    #[test]
    fn two_second_step() {
        let mut events = Vec::new();
        schedule_step_events(&mut events, 0, 2000, 0x1234, 0, 2, 50);
        let t = times(&events);
        assert_eq!(t.len(), 10); // 2000/200 = 10 events
        assert_eq!(t[0], 0);
        assert_eq!(t[9], 1800);
    }

    #[test]
    fn step_with_offset_start() {
        let mut events = Vec::new();
        schedule_step_events(&mut events, 500, 1500, 0x1234, 0, 1, 30);
        assert_eq!(times(&events), vec![500, 700, 900, 1100, 1300]);
    }

    #[test]
    fn inverted_range_produces_nothing() {
        let mut events = Vec::new();
        schedule_step_events(&mut events, 1000, 500, 0x1234, 0, 2, 50);
        assert!(events.is_empty());
    }

    #[test]
    fn zero_duration_produces_nothing() {
        let mut events = Vec::new();
        schedule_step_events(&mut events, 1000, 1000, 0x1234, 0, 2, 50);
        assert!(events.is_empty());
    }

    #[test]
    fn event_fields_correct() {
        let mut events = Vec::new();
        schedule_step_events(&mut events, 100, 250, 0xABCD, 2, 3, 77);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].time_ms, 100);
        assert_eq!(events[0].collar_id, 0xABCD);
        assert_eq!(events[0].channel, 2);
        assert_eq!(events[0].mode_byte, 3);
        assert_eq!(events[0].intensity, 77);
    }

    #[test]
    fn no_event_past_end() {
        let mut events = Vec::new();
        schedule_step_events(&mut events, 0, 400, 0x1234, 0, 2, 50);
        // 0, 200 — next would be 400 which is == end, so excluded
        assert_eq!(times(&events), vec![0, 200]);
    }
}

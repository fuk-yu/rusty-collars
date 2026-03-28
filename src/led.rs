use anyhow::Result;
use esp_idf_svc::hal::gpio::{Output, OutputPin, PinDriver};

pub struct Led {
    pin: PinDriver<'static, Output>,
}

impl Led {
    pub fn new(pin: impl OutputPin + 'static) -> Result<Self> {
        let mut pin = PinDriver::output(pin)?;
        pin.set_low()?;
        Ok(Self { pin })
    }

    pub fn set(&mut self, on: bool) {
        let result = if on {
            self.pin.set_high()
        } else {
            self.pin.set_low()
        };
        if let Err(e) = result {
            log::warn!("LED GPIO error: {e}");
        }
    }
}

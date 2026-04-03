use thiserror::Error;

#[derive(Debug, Error)]
pub enum ControlError {
    #[error("Unknown collar: {0}")]
    UnknownCollar(String),
    #[error("Unknown preset: {0}")]
    UnknownPreset(String),
    #[error("Collar '{0}' already exists")]
    DuplicateCollar(String),
    #[error("Preset '{0}' already exists")]
    DuplicatePreset(String),
    #[error("Cannot delete '{0}': presets reference it")]
    CollarReferencedByPreset(String),
    #[error("Transmissions locked after STOP")]
    TransmissionLockout,
    #[error("Intensity {intensity} exceeds max {max}")]
    InvalidIntensity { intensity: u8, max: u8 },
    #[error("Action duration must be greater than zero")]
    ActionDurationZero,
    #[error("Held actions require an owning connection")]
    HeldActionRequiresOwner,
    #[error("{operation} is not available over remote control")]
    LocalUiOnly { operation: &'static str },
    #[error("NTP server cannot be empty when time sync is enabled")]
    EmptyNtpServer,
    #[error("{0}")]
    Validation(String),
    #[error("{0}")]
    RemoteControlUrl(String),
    #[error("{operation} failed: {source}")]
    Persistence {
        operation: &'static str,
        #[source]
        source: anyhow::Error,
    },
    #[error(transparent)]
    Serialization(#[from] serde_json::Error),
}

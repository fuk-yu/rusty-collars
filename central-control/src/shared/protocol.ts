// Board protocol types — matches the firmware WebSocket protocol

export type CommandMode = "shock" | "vibrate" | "beep";
export type PresetStepMode = CommandMode | "pause";
export type Distribution = "uniform" | "gaussian";

export interface Collar {
  name: string;
  collar_id: number;
  channel: number;
}

export interface PresetStep {
  mode: PresetStepMode;
  intensity: number;
  intensity_max?: number;
  duration_ms: number;
  duration_max_ms?: number;
  intensity_distribution?: Distribution;
  duration_distribution?: Distribution;
}

export interface PresetTrack {
  collar_name: string;
  steps: PresetStep[];
}

export interface Preset {
  name: string;
  tracks: PresetTrack[];
}

export interface StateMessage {
  type: "state";
  device_id: string;
  app_version: string;
  server_uptime_s: number;
  collars: Collar[];
  presets: Preset[];
  preset_running: string | null;
  rf_lockout_remaining_ms: number;
}

export interface PongMessage {
  type: "pong";
  nonce: number;
  server_uptime_s?: number;
  free_heap_bytes?: number;
  connected_clients?: number;
  client_ips?: string[];
}

export interface PingMessage {
  type: "ping";
  nonce: number;
}

export interface ErrorMessage {
  type: "error";
  message: string;
}

export interface RemoteControlStatusMessage {
  type: "remote_control_status";
  status: {
    enabled: boolean;
    connected: boolean;
    url: string;
    validate_cert: boolean;
    rtt_ms: number | null;
    status_text: string;
  };
}

export interface PresetPreviewMessage {
  type: "preset_preview";
  nonce: number;
  preview: PresetPreview | null;
  error: string | null;
}

export interface PresetPreview {
  total_duration_us: number;
  events: PresetPreviewEvent[];
}

export interface PresetPreviewEvent {
  requested_time_us: number;
  actual_time_us: number;
  track_index: number;
  step_index: number;
  transmit_duration_us: number;
  collar_name: string;
  collar_id: number;
  channel: number;
  mode: CommandMode;
  mode_byte: number;
  intensity: number;
  raw_hex: string;
}

export type EventSource = "local_ui" | "remote_control" | "system";

export interface EventLogAction {
  sequence: number;
  monotonic_ms: number;
  unix_ms: number | null;
  source: EventSource;
  event: "action";
  collar_name: string;
  mode: CommandMode;
  intensity: number | null;
  duration_ms: number;
}

export interface EventLogPresetRun {
  sequence: number;
  monotonic_ms: number;
  unix_ms: number | null;
  source: EventSource;
  event: "preset_run";
  preset_name: string;
  resolved_preset?: Preset;
}

export type EventLogEntry = EventLogAction | EventLogPresetRun;

export interface EventLogStateMessage {
  type: "event_log_state";
  enabled: boolean;
  events: EventLogEntry[];
}

export interface EventLogEventMessage {
  type: "event_log_event";
  event: EventLogEntry;
}

export type BoardIncomingMessage =
  | StateMessage
  | EventLogStateMessage
  | EventLogEventMessage
  | RemoteControlStatusMessage
  | PresetPreviewMessage
  | PongMessage
  | PingMessage
  | ErrorMessage;

// Commands sent TO the board
export interface RunActionCommand {
  type: "run_action";
  collar_name: string;
  mode: CommandMode;
  intensity: number;
  intensity_max?: number;
  duration_ms: number;
  duration_max_ms?: number;
  intensity_distribution?: Distribution;
  duration_distribution?: Distribution;
}

export interface StartActionCommand {
  type: "start_action";
  collar_name: string;
  mode: CommandMode;
  intensity: number;
  intensity_max?: number;
  intensity_distribution?: Distribution;
}

export interface StopActionCommand {
  type: "stop_action";
  collar_name: string;
  mode: CommandMode;
}

export interface StopAllCommand {
  type: "stop_all";
}

export interface RunPresetCommand {
  type: "run_preset";
  name: string;
}

export interface StopPresetCommand {
  type: "stop_preset";
}

export interface SavePresetCommand {
  type: "save_preset";
  original_name: string | null;
  preset: Preset;
}

export interface DeletePresetCommand {
  type: "delete_preset";
  name: string;
}

export interface ReorderPresetsCommand {
  type: "reorder_presets";
  names: string[];
}

export interface PreviewPresetCommand {
  type: "preview_preset";
  nonce: number;
  preset: Preset;
}

export type BoardCommand =
  | RunActionCommand
  | StartActionCommand
  | StopActionCommand
  | StopAllCommand
  | RunPresetCommand
  | StopPresetCommand
  | SavePresetCommand
  | DeletePresetCommand
  | ReorderPresetsCommand
  | PreviewPresetCommand
  | PingMessage
  | PongMessage;

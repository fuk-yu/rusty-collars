export type CommandMode = "shock" | "vibrate" | "beep";
export type EventSource = "local_ui" | "remote_control" | "system";

export interface Collar {
  name: string;
  collar_id: number;
  channel: number;
}

export interface PresetStep {
  mode: "shock" | "vibrate" | "beep" | "pause";
  intensity: number;
  duration_ms: number;
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
}

export interface EventLogNtpSync {
  sequence: number;
  monotonic_ms: number;
  unix_ms: number | null;
  source: EventSource;
  event: "ntp_sync";
  server: string;
}

export interface EventLogRemoteConnection {
  sequence: number;
  monotonic_ms: number;
  unix_ms: number | null;
  source: EventSource;
  event: "remote_control_connection";
  connected: boolean;
  url: string;
  reason: string | null;
}

export type EventLogEntry =
  | EventLogAction
  | EventLogPresetRun
  | EventLogNtpSync
  | EventLogRemoteConnection;

export interface EventLogStateMessage {
  type: "event_log_state";
  enabled: boolean;
  events: EventLogEntry[];
}

export interface EventLogEventMessage {
  type: "event_log_event";
  event: EventLogEntry;
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

export type BoardIncomingMessage =
  | StateMessage
  | EventLogStateMessage
  | EventLogEventMessage
  | RemoteControlStatusMessage
  | PresetPreviewMessage
  | PongMessage
  | PingMessage
  | ErrorMessage;

export interface AddCollarCommand {
  type: "add_collar";
  name: string;
  collar_id: number;
  channel: number;
}

export interface UpdateCollarCommand {
  type: "update_collar";
  original_name: string;
  name: string;
  collar_id: number;
  channel: number;
}

export interface DeleteCollarCommand {
  type: "delete_collar";
  name: string;
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

export interface RunPresetCommand {
  type: "run_preset";
  name: string;
}

export interface StopPresetCommand {
  type: "stop_preset";
}

export interface StopAllCommand {
  type: "stop_all";
}

export interface RunActionCommand {
  type: "run_action";
  collar_name: string;
  mode: CommandMode;
  intensity: number;
  duration_ms: number;
}

export interface StartActionCommand {
  type: "start_action";
  collar_name: string;
  mode: CommandMode;
  intensity: number;
}

export interface StopActionCommand {
  type: "stop_action";
  collar_name: string;
  mode: CommandMode;
}

export interface PreviewPresetCommand {
  type: "preview_preset";
  nonce: number;
  preset: Preset;
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

export type BoardCommand =
  | AddCollarCommand
  | UpdateCollarCommand
  | DeleteCollarCommand
  | SavePresetCommand
  | DeletePresetCommand
  | ReorderPresetsCommand
  | RunPresetCommand
  | StopPresetCommand
  | StopAllCommand
  | RunActionCommand
  | StartActionCommand
  | StopActionCommand
  | PreviewPresetCommand
  | PingMessage
  | PongMessage;

export interface BoardSnapshot {
  id: string;
  peer: string;
  path: string;
  connected_at_ms: number;
  last_seen_at_ms: number;
  rtt_ms: number | null;
  last_error: string | null;
  remote_control_status: RemoteControlStatusMessage["status"] | null;
  state: StateMessage | null;
  event_log_enabled: boolean;
  event_log_events: EventLogEntry[];
}

export interface SnapshotMessage {
  type: "snapshot";
  server_started_at_ms: number;
  boards: BoardSnapshot[];
}

export interface UiErrorMessage {
  type: "ui_error";
  message: string;
}

export interface UiInfoMessage {
  type: "ui_info";
  message: string;
}

export interface UiBoardCommand {
  type: "board_command";
  board_id: string;
  command: BoardCommand;
}

export interface UiPingBoard {
  type: "ping_board";
  board_id: string;
}

export type UiIncomingMessage = UiBoardCommand | UiPingBoard;
export type UiOutgoingMessage = SnapshotMessage | UiErrorMessage | UiInfoMessage | PresetPreviewMessage;

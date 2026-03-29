// Thin wrapper around the shared preset editor for central-control.
import {
  openEditor,
  closeEditor,
  handlePreviewResult,
  type EditorCollar,
  type EditorPreset,
  type EditorCollarPermission,
  type PresetEditorConfig,
} from "../../../../ui-shared/preset-editor.js";
import type { Collar, Preset } from "../../shared/protocol.js";
import type { DevicePermission } from "../../shared/types.js";
import * as ws from "../ws.js";

export { handlePreviewResult, closeEditor };

type SaveCallback = (preset: Preset) => Promise<void>;

export function openPresetEditor(
  preset: Preset | null,
  originalName: string | null,
  collars: Collar[],
  onSave: SaveCallback,
  permissions?: DevicePermission,
  deviceUuid?: string,
): void {
  const cfg: PresetEditorConfig = {
    collars: collars as EditorCollar[],
    ...(permissions ? { permissions: permissions.collars as EditorCollarPermission[] } : {}),
    onSave: async (_origName, edited) => {
      await onSave(edited as Preset);
    },
    ...(deviceUuid
      ? {
          onPreview: (nonce: number, previewPreset: EditorPreset) => {
            ws.sendDeviceCommand(deviceUuid, {
              type: "preview_preset",
              nonce,
              preset: previewPreset as Preset,
            });
          },
        }
      : {}),
  };
  openEditor(cfg, preset as EditorPreset | null, originalName);
}

import type { Preset, CommandMode } from "../shared/protocol.js";
import type { CollarPermission, DevicePermission, ModeLimit, BeepLimit } from "../shared/types.js";

export interface CommandCheck {
  collarName: string;
  mode: CommandMode;
  intensity: number;
  durationMs: number;
}

export function checkCommandPermission(perm: DevicePermission, check: CommandCheck): string | null {
  const collarPerm = perm.collars.find((c) => c.collarName === check.collarName);
  if (!collarPerm) {
    return `No access to collar "${check.collarName}"`;
  }

  return checkModeLimits(collarPerm, check.mode, check.intensity, check.durationMs);
}

function checkModeLimits(
  collarPerm: CollarPermission,
  mode: CommandMode,
  intensity: number,
  durationMs: number,
): string | null {
  switch (mode) {
    case "shock": {
      if (!collarPerm.shock) return "Shock not permitted on this collar";
      return checkIntensityDuration(collarPerm.shock, intensity, durationMs, "shock");
    }
    case "vibrate": {
      if (!collarPerm.vibrate) return "Vibrate not permitted on this collar";
      return checkIntensityDuration(collarPerm.vibrate, intensity, durationMs, "vibrate");
    }
    case "beep": {
      if (!collarPerm.beep) return "Beep not permitted on this collar";
      if (durationMs > collarPerm.beep.maxDurationMs) {
        return `Beep duration ${durationMs}ms exceeds max ${collarPerm.beep.maxDurationMs}ms`;
      }
      return null;
    }
  }
}

function checkIntensityDuration(
  limit: ModeLimit,
  intensity: number,
  durationMs: number,
  modeName: string,
): string | null {
  if (intensity > limit.maxIntensity) {
    return `${modeName} intensity ${intensity} exceeds max ${limit.maxIntensity}`;
  }
  if (durationMs > limit.maxDurationMs) {
    return `${modeName} duration ${durationMs}ms exceeds max ${limit.maxDurationMs}ms`;
  }
  return null;
}

export function checkPresetPermission(perm: DevicePermission, preset: Preset): string | null {
  for (const track of preset.tracks) {
    const collarPerm = perm.collars.find((c) => c.collarName === track.collar_name);
    if (!collarPerm) {
      return `No access to collar "${track.collar_name}"`;
    }

    for (const step of track.steps) {
      if (step.mode === "pause") continue;

      const error = checkModeLimits(collarPerm, step.mode, step.intensity, step.duration_ms);
      if (error) {
        return `Track "${track.collar_name}": ${error}`;
      }
    }
  }
  return null;
}

export function isPresetWithinLimits(perm: DevicePermission, preset: Preset): boolean {
  return checkPresetPermission(perm, preset) === null;
}

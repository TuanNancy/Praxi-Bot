/** Rules of the "Save as skill" dialog. The daemon enforces the same: this only gates the button. */

export interface SaveGate {
  /** The text in the editor changed since the last scan. */
  dirty: boolean;
  /** `low`, `medium` or `high` from the last scan. */
  risk: string;
  /** The run had read untrusted or private content. */
  tainted: boolean;
  /** The owner ticked the box that they read the findings. */
  acknowledged: boolean;
  busy: boolean;
}

/** A skill above low risk, or one from a run that read outside content, needs a tick. */
export function needsAcknowledgement(risk: string, tainted: boolean): boolean {
  return tainted || risk === "medium" || risk === "high";
}

/** Saving is possible only for text that was scanned, and after the tick when one is needed. */
export function canSave(g: SaveGate): boolean {
  if (g.busy || g.dirty) return false;
  return !needsAcknowledgement(g.risk, g.tainted) || g.acknowledged;
}

/** Plain label of a tool effect; unknown values read as "unknown". */
export function effectKey(effect: string): string {
  return effect === "read" || effect === "write" || effect === "destructive"
    ? `skillFromChat.effect.${effect}`
    : "skillFromChat.effect.unknown";
}

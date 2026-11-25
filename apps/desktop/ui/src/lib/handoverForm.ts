/**
 * Pure rules of the hand-over card (`HandoverCard`): a bot asks the owner to solve a CAPTCHA or to
 * type a one-time (2FA) code. Kept apart from React so they can be tested. The code is read from
 * the input once, posted to the daemon, and never stored, logged or put in state.
 */

export type HandoverKind = "captcha" | "code";

/** Letters and digits of a typed or pasted code: spaces and dashes (`123 456`, `123-456`) go. */
export function cleanCode(raw: string): string {
  return raw.replace(/[\s-]+/g, "");
}

/** The daemon accepts 4 to 12 letters or digits. */
export function isCodeValid(raw: string): boolean {
  return /^[A-Za-z0-9]{4,12}$/.test(cleanCode(raw));
}

/** The site the browser reported (`https://bank.example:443`) as the owner reads it: `bank.example`. */
export function siteLabel(origin: string): string {
  try {
    return new URL(origin).hostname;
  } catch {
    return origin;
  }
}

/** Error codes after which the card cannot be answered any more. */
export function isHandoverGone(code: string): boolean {
  return code === "handover_gone";
}

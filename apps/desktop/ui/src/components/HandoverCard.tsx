import { useEffect, useId, useRef, useState, type FormEvent, type KeyboardEvent } from "react";
import { Hand, ShieldQuestion } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { ApiError } from "../api/client";
import type { HandoverItem } from "../state/timeline";
import { shieldFromCapture } from "../lib/captureShield";
import { escapeInvisible } from "../lib/approvalPlain";
import { humanLine } from "../lib/humanError";
import { cleanCode, isCodeValid, isHandoverGone, siteLabel } from "../lib/handoverForm";
import { NoClientError, formatCountdown, secondsLeft } from "../lib/secretRequestForm";
import formStyles from "./Form.module.css";
import styles from "./HandoverCard.module.css";

export interface HandoverCardProps {
  item: HandoverItem;
  /** Which bot is asking (the bot that owns the run). */
  botName: string;
  /** Posts the cleaned code to the daemon, which types it into the page. Never stored here. */
  onSubmitCode: (code: string) => Promise<void>;
  /** The owner says the CAPTCHA is solved; the daemon checks that the page moved on. */
  onDone: () => Promise<void>;
  onCancel: () => Promise<void>;
  /** Opens the live view of the bot's browser, where the owner takes control. */
  onTakeControl: () => void;
}

/**
 * The owner's card for a bot's `browser_handover`: a CAPTCHA to solve in the live view, or a
 * one-time (2FA) code to type. Everything on it is the app's own text; the only things the bot
 * supplies are the kind and the site the BROWSER reported. A typed code lives only in the
 * (uncontrolled) input: it is read once on submit, the input is emptied straight away, and it is
 * never put in React state, a log, an error or a URL.
 */
export function HandoverCard({ item, botName, onSubmitCode, onDone, onCancel, onTakeControl }: HandoverCardProps) {
  const { t } = useTranslation();
  const uid = useId();
  const codeRef = useRef<HTMLInputElement | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [gone, setGone] = useState(false);

  const left = secondsLeft(item.expiresMs, now);
  const finished = item.outcome !== undefined;
  const expired = left === 0;
  const locked = finished || item.closed === true || gone || expired;
  const isCode = item.handover === "code";

  // While the card is open: tick the countdown; a code card also keeps the window out of screen
  // capture and moves focus to the field (the card appears on its own).
  useEffect(() => {
    if (locked) return undefined;
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    const release = isCode ? shieldFromCapture() : () => {};
    codeRef.current?.focus();
    return () => {
      window.clearInterval(timer);
      release();
    };
  }, [locked, isCode]);

  const wipe = () => {
    if (codeRef.current) codeRef.current.value = "";
  };
  useEffect(() => wipe, []);

  // Fixed sentences only: a paired host's refusal text never lands in the card.
  const describe = (err: unknown): string => {
    if (err instanceof NoClientError) return t("handover.notConnected");
    return humanLine(err);
  };

  const run = async (action: () => Promise<void>, clear: boolean) => {
    if (busy || locked) return;
    setBusy(true);
    setError(null);
    try {
      await action();
      if (clear) wipe();
    } catch (err) {
      if (err instanceof ApiError && isHandoverGone(err.code)) {
        wipe();
        setGone(true);
      }
      setError(describe(err));
    } finally {
      setBusy(false);
    }
  };

  const submit = (e: FormEvent) => {
    e.preventDefault();
    const raw = codeRef.current?.value ?? "";
    if (!isCodeValid(raw)) {
      setError(t("handover.wrongFormat"));
      return;
    }
    const code = cleanCode(raw);
    wipe();
    void run(() => onSubmitCode(code), true);
  };

  const onKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      void run(onCancel, true);
    }
  };

  const titleId = `${uid}-title`;
  const bodyId = `${uid}-body`;
  const warnId = `${uid}-warn`;
  const site = escapeInvisible(siteLabel(item.site));
  const Icon = isCode ? ShieldQuestion : Hand;

  return (
    <section
      className={styles.card}
      data-testid="handover-card"
      data-call-id={item.callId}
      data-handover={item.handover}
      aria-labelledby={titleId}
      aria-describedby={`${bodyId}${item.untrustedContent && isCode ? ` ${warnId}` : ""}`}
    >
      <div className={styles.head}>
        <h3 className={styles.title} id={titleId}>
          <Icon size={16} aria-hidden="true" />
          {t(isCode ? "handover.codeTitle" : "handover.captchaTitle", { bot: escapeInvisible(botName) })}
        </h3>
        {!locked ? (
          <span className={styles.countdown} role="timer" aria-label={t("handover.expiresIn")}>
            {formatCountdown(left)}
          </span>
        ) : null}
      </div>

      <p className={styles.dest} data-testid="handover-site">
        {t("handover.siteLabel")} <span className={styles.mono}>{site}</span>
      </p>
      <p className={styles.status} id={bodyId}>
        {t(isCode ? "handover.codeBody" : "handover.captchaBody")}
      </p>
      {item.untrustedContent && isCode ? (
        <p className={styles.warn} id={warnId} role="alert">
          {t("handover.untrusted")}
        </p>
      ) : null}

      {finished ? (
        <p className={styles.status} role="status">
          {item.outcome === "done" ? t("handover.done") : t("handover.cancelled")}
        </p>
      ) : gone ? (
        <p className={styles.status} role="status">{error ?? t("handover.closed")}</p>
      ) : locked ? (
        <p className={styles.status} role="status">
          {expired && item.closed !== true ? t("handover.expired") : t("handover.closed")}
        </p>
      ) : isCode ? (
        <form className={styles.form} onSubmit={submit} onKeyDown={onKeyDown} autoComplete="off">
          <div className={formStyles.field}>
            <label className={formStyles.label} htmlFor={`${uid}-code`}>{t("handover.codeLabel")}</label>
            <input
              id={`${uid}-code`}
              ref={codeRef}
              className={formStyles.input}
              type="text"
              inputMode="text"
              autoComplete="one-time-code"
              autoCapitalize="off"
              autoCorrect="off"
              spellCheck={false}
              maxLength={24}
              disabled={busy}
            />
          </div>
          <div className={styles.actions}>
            <Button size="sm" variant="primary" type="submit" disabled={busy}>
              {t("handover.submit")}
            </Button>
            <Button size="sm" variant="secondary" type="button" disabled={busy} onClick={() => void run(onCancel, true)}>
              {t("handover.cancel")}
            </Button>
          </div>
        </form>
      ) : (
        <div className={styles.form} onKeyDown={onKeyDown}>
          <p className={styles.status}>{t("handover.takeControlHint")}</p>
          <div className={styles.actions}>
            <Button size="sm" variant="primary" type="button" disabled={busy} onClick={onTakeControl}>
              {t("handover.takeControl")}
            </Button>
            <Button size="sm" variant="secondary" type="button" disabled={busy} onClick={() => void run(onDone, false)}>
              {t("handover.doneButton")}
            </Button>
            <Button size="sm" variant="secondary" type="button" disabled={busy} onClick={() => void run(onCancel, false)}>
              {t("handover.cancel")}
            </Button>
          </div>
        </div>
      )}
      {error && !gone && !locked ? (
        <p className={styles.warn} role="alert">{error}</p>
      ) : null}
    </section>
  );
}

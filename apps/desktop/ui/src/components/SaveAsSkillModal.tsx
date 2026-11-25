import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { ApiClient } from "../api/client";
import type { SkillDraft, SkillDraftPreview } from "../api/types";
import { humanLine } from "../lib/humanError";
import { canSave, effectKey, needsAcknowledgement } from "../lib/skillFromChat";
import { riskOf, riskTone } from "../lib/skillMarket";
import { Callout, DialogBody, DialogFooter, DialogHeader, dialogStyles } from "./DialogParts";
import { Modal } from "./Modal";
import styles from "./SaveAsSkillModal.module.css";
import marketStyles from "./SkillMarket.module.css";
import skillStyles from "./Skills.module.css";
import { useToast } from "./Toast";

/**
 * "Save as skill": turn the last finished run of a chat into a reusable skill. The daemon drafts it
 * from the recorded steps (values that look private are left out), this dialog shows the plan, the
 * permissions it relies on and the safety scan, and nothing is saved until the owner presses Save.
 */
export function SaveAsSkillModal({
  chatId,
  client,
  onClose
}: {
  /** The chat to save from; the dialog is closed when null. */
  chatId: string | null;
  client: ApiClient | null;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  return (
    <Modal open={chatId !== null} onClose={onClose} label={t("skillFromChat.title")} size="md">
      {chatId ? <Dialog chatId={chatId} client={client} onClose={onClose} /> : null}
    </Modal>
  );
}

function Dialog({ chatId, client, onClose }: { chatId: string; client: ApiClient | null; onClose: () => void }) {
  const { t } = useTranslation();
  const toast = useToast();
  const [draft, setDraft] = useState<SkillDraft | null>(null);
  const [scan, setScan] = useState<SkillDraftPreview | null>(null);
  const [text, setText] = useState("");
  const [scannedText, setScannedText] = useState("");
  const [ack, setAck] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!client) return;
    let live = true;
    client.skillDraftFromChat(chatId).then(
      (d) => {
        if (!live) return;
        setDraft(d);
        setScan(d.preview);
        setText(d.skill_md);
        setScannedText(d.skill_md);
      },
      (e: unknown) => {
        if (live) setError(humanLine(e));
      }
    );
    return () => {
      live = false;
    };
  }, [client, chatId]);

  const recheck = async () => {
    if (!client || busy) return;
    setBusy(true);
    setError(null);
    try {
      setScan(await client.skillDraftPreview(text));
      setScannedText(text);
      setAck(false);
    } catch (e) {
      setError(humanLine(e));
    } finally {
      setBusy(false);
    }
  };

  const save = async () => {
    if (!client || !scan || busy) return;
    setBusy(true);
    setError(null);
    try {
      const saved = await client.skillDraftInstall(chatId, text, scan.fingerprint, ack);
      toast.show(t("skillFromChat.saved", { name: saved.name }), "success");
      onClose();
    } catch (e) {
      setError(humanLine(e));
      setBusy(false);
    }
  };

  const risk = scan ? riskOf(scan.risk) : "low";
  const flagged = scan ? scan.findings.filter((f) => f.severity !== "info") : [];
  const tainted = draft?.tainted ?? false;
  const dirty = text !== scannedText;
  const ready = scan !== null && canSave({ dirty, risk, tainted, acknowledged: ack, busy });

  return (
    <div className={dialogStyles.root} data-testid="save-as-skill">
      <DialogHeader title={t("skillFromChat.title")} subtitle={t("skillFromChat.subtitle")} closeDisabled={busy} onClose={onClose} />
      <DialogBody inert={busy}>
        {!draft && !error ? <p className={skillStyles.itemDesc} role="status">{t("skillFromChat.loading")}</p> : null}
        {draft ? (
          <>
            <section className={dialogStyles.block}>
              <h3 className={dialogStyles.blockTitle}>{t("skillFromChat.stepsTitle")}</h3>
              <ol className={styles.list}>
                {draft.steps.map((s, i) => (
                  <li key={i}>{s}</li>
                ))}
              </ol>
            </section>
            <section className={dialogStyles.block}>
              <h3 className={dialogStyles.blockTitle}>{t("skillFromChat.permsTitle")}</h3>
              <p className={skillStyles.itemDesc}>{t("skillFromChat.permsLead")}</p>
              <ul className={styles.list}>
                {draft.tools.map((tool) => (
                  <li key={tool.name}>
                    <code>{tool.name}</code> {t(effectKey(tool.effect))}
                  </li>
                ))}
              </ul>
            </section>
            {draft.withheld > 0 ? (
              <Callout tone="info" role="status">{t("skillFromChat.withheld", { count: draft.withheld })}</Callout>
            ) : null}
            {tainted ? <Callout tone="warning" role="status">{t("skillFromChat.tainted")}</Callout> : null}
            <section className={dialogStyles.block}>
              <h3 className={dialogStyles.blockTitle}>{t("skillFromChat.editTitle")}</h3>
              <textarea
                className={styles.editor}
                value={text}
                spellCheck={false}
                aria-label={t("skillFromChat.editTitle")}
                data-testid="save-as-skill-text"
                onChange={(e) => setText(e.target.value)}
              />
              {dirty ? (
                <Button variant="secondary" disabled={busy} onClick={() => void recheck()}>
                  {t("skillFromChat.recheck")}
                </Button>
              ) : null}
            </section>
            <section className={dialogStyles.block}>
              <h3 className={dialogStyles.blockTitle}>{t("skillMarket.scanTitle")}</h3>
              {flagged.length === 0 ? (
                <p className={skillStyles.itemDesc}>{t("skillMarket.scanClean")}</p>
              ) : (
                <ul className={marketStyles.findings}>
                  {flagged.map((f, i) => (
                    <li key={`${f.file}:${f.line ?? 0}:${i}`}>
                      <span className={`${marketStyles.badge} ${f.severity === "high" ? marketStyles.tone_danger : marketStyles.tone_warn}`}>
                        {t(`skillMarket.severity.${f.severity === "high" ? "high" : "warn"}`)}
                      </span>{" "}
                      {f.message}
                    </li>
                  ))}
                </ul>
              )}
              <p className={dialogStyles.hint}>{t("skillMarket.scanAdvisory")}</p>
            </section>
            {needsAcknowledgement(risk, tainted) ? (
              <Callout tone={riskTone(risk) === "danger" ? "danger" : "warning"} title={t("skillMarket.riskTitle", { risk: t(`skillMarket.risk.${risk}`) })}>
                <label className={marketStyles.tick}>
                  <input type="checkbox" checked={ack} onChange={(e) => setAck(e.target.checked)} data-testid="save-as-skill-ack" />
                  <span>{t("skillFromChat.ack")}</span>
                </label>
              </Callout>
            ) : null}
          </>
        ) : null}
        {error ? <Callout tone="danger" role="alert">{error}</Callout> : null}
      </DialogBody>
      <DialogFooter note={draft && !ready && !busy ? t(dirty ? "skillFromChat.recheckFirst" : "skillFromChat.tickFirst") : undefined}>
        <Button variant="ghost" disabled={busy} onClick={onClose}>{t("common.cancel")}</Button>
        <Button variant="primary" disabled={!ready} data-testid="save-as-skill-save" onClick={() => void save()}>
          {t("skillFromChat.save")}
        </Button>
      </DialogFooter>
    </div>
  );
}

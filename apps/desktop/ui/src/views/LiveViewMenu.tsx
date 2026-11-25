import { Check, Crop, Maximize2, Minimize2, PanelRight, RefreshCw, SlidersHorizontal } from "lucide-react";
import { useCallback, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { Popover } from "../components/Popover";
import { StreamQuality } from "../components/StreamQuality";
import type { QualityMode } from "../lib/liveViewAdaptive";
import type { ScreenFit } from "../lib/liveViewInput";
import { type FitMode, VIEWER_SHORTCUTS } from "../lib/liveViewToolbar";
import styles from "./LiveViewMenu.module.css";

/**
 * "View": everything about how the screen is shown, behind one button so the top bar keeps only
 * Back, the screen picker, this button and Stop.  Picture quality, fill the window, refresh the list
 * of screens, the bot info panel and the keyboard shortcuts.
 */
export function LiveViewMenu({
  quality,
  onQuality,
  fit,
  onToggleFit,
  screenFit,
  onToggleScreenFit,
  onRefresh,
  refreshing,
  infoPanel
}: {
  quality: QualityMode;
  onQuality: (mode: QualityMode) => void;
  fit: FitMode;
  onToggleFit: () => void;
  /** `cover`: the screen fills the box and its edges are cropped; `contain`: the whole screen with bars. */
  screenFit: ScreenFit;
  onToggleScreenFit: () => void;
  onRefresh: () => void;
  refreshing: boolean;
  /** The bot panel beside the viewer; absent when the viewer has none. */
  infoPanel?: { open: boolean; onToggle: () => void };
}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const trigger = useRef<HTMLButtonElement | null>(null);
  const close = useCallback(() => setOpen(false), []);
  const fitLabel = t(fit === "fill" ? "liveView.toolbar.fitOn" : "liveView.toolbar.fitOff");
  return (
    <>
      <Button
        ref={trigger}
        variant="secondary"
        size="sm"
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-controls="live-viewer-view"
        data-testid="live-viewer-view"
        onClick={() => setOpen((v) => !v)}
      >
        <SlidersHorizontal aria-hidden="true" />
        <span>{t("liveView.toolbar.view")}</span>
      </Button>
      <Popover open={open} anchor={trigger} onClose={close} placement="bottom-end" role="dialog" label={t("liveView.toolbar.view")} width={304}>
        <div id="live-viewer-view" className={styles.menu} data-testid="live-viewer-tools">
          <section className={styles.section}>
            <p className={styles.label}>{t("liveView.quality.label")}</p>
            <StreamQuality mode={quality} onChange={onQuality} />
          </section>
          <div className={styles.rows}>
            <button
              type="button"
              className={styles.row}
              aria-pressed={fit === "fill"}
              aria-keyshortcuts="F"
              data-testid="live-viewer-fit"
              onClick={onToggleFit}
            >
              {fit === "fill" ? <Minimize2 aria-hidden="true" /> : <Maximize2 aria-hidden="true" />}
              <span className={styles.rowText}>{fitLabel}</span>
              <kbd className={styles.key}>F</kbd>
            </button>
            <button
              type="button"
              className={styles.row}
              aria-pressed={screenFit === "cover"}
              title={t("liveView.toolbar.screenFillHint")}
              data-testid="live-viewer-screen-fit"
              onClick={onToggleScreenFit}
            >
              <Crop aria-hidden="true" />
              <span className={styles.rowText}>{t("liveView.toolbar.screenFill")}</span>
              {screenFit === "cover" ? <Check className={styles.on} aria-hidden="true" /> : null}
            </button>
            <button type="button" className={styles.row} disabled={refreshing} data-testid="live-viewer-refresh" onClick={onRefresh}>
              <RefreshCw aria-hidden="true" />
              <span className={styles.rowText}>{t("liveView.refresh")}</span>
            </button>
            {infoPanel ? (
              <button
                type="button"
                className={styles.row}
                aria-pressed={infoPanel.open}
                data-testid="live-viewer-info"
                onClick={infoPanel.onToggle}
              >
                <PanelRight aria-hidden="true" />
                <span className={styles.rowText}>{t("liveView.infoPanel")}</span>
                {infoPanel.open ? <Check className={styles.on} aria-hidden="true" /> : null}
              </button>
            ) : null}
          </div>
          <section className={styles.section} aria-label={t("liveView.toolbar.shortcuts")} data-testid="live-viewer-shortcuts">
            <p className={styles.label}>{t("liveView.toolbar.shortcuts")}</p>
            <dl className={styles.keys}>
              {VIEWER_SHORTCUTS.map((item) => (
                <div key={item.id} className={styles.keyRow}>
                  <dt>
                    <kbd className={styles.key}>{item.keys}</kbd>
                  </dt>
                  <dd>{t(`liveView.toolbar.keys.${item.id}`)}</dd>
                </div>
              ))}
            </dl>
          </section>
        </div>
      </Popover>
    </>
  );
}

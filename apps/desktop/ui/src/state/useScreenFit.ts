import { useSyncExternalStore } from "react";
import {
  getScreenFit,
  setScreenFit,
  subscribeScreenFit,
  toggleScreenFit
} from "../lib/liveViewFitPref.ts";
import type { ScreenFit } from "../lib/liveViewInput.ts";

/** The screen share's fit (`cover` = fill, `contain` = whole screen) shared by every viewer of it. */
export function useScreenFit(): {
  screenFit: ScreenFit;
  setScreenFit: (fit: ScreenFit) => void;
  toggleScreenFit: () => void;
} {
  const screenFit = useSyncExternalStore(subscribeScreenFit, getScreenFit, getScreenFit);
  return {
    screenFit,
    setScreenFit,
    toggleScreenFit: () => setScreenFit(toggleScreenFit(getScreenFit()))
  };
}

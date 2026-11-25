# Hand-over to the owner: CAPTCHA and one-time (2FA) codes

Tool `browser_handover` (`crates/builtin-tools/src/handover.rs`), event `handover_requested`, routes
`POST /v1/handovers/{call_id}/{code,done,cancel}`, card `apps/desktop/ui/src/components/HandoverCard.tsx`.
Built on the same pattern as `secret_request` (see `secret-request.md`).

## What the user sees

* **CAPTCHA.** The bot first tries a simple one itself, as an ordinary user would (an "I'm not a robot"
  checkbox, an easy picture or text puzzle), at most **2 looks/attempts** per run
  (`CAPTCHA_OWN_ATTEMPTS`). Behavioural or picture-tile puzzles (press and hold, "select all images",
  sliders) are handed over at once. A card says "this site wants proof you are human" with **Take
  control** (opens the existing live view, where the owner takes over as usual) and **Done**. Done
  hands control back, the daemon re-reads the page and only resumes the run when the check is gone;
  otherwise the card stays open and says the page still shows it.
  The attempt budget belongs to the **site** (host; `www.`, scheme and port do not make a new site) and
  to the whole run family: a subagent shares its parent's budget. Leaving the page, opening another
  site, coming back, refreshing or asking a subagent gives no attempt back; only the owner's Done (or
  the end of the root run) clears it. Only a real challenge counts: a passive reCAPTCHA badge or
  "protected by reCAPTCHA" footer is not one. While a CAPTCHA is on the page of the visible managed
  browser (read fresh from the browser when a `desktop_*` input tool is called) those tools refuse, so
  the window cannot be clicked around the cap. If the page still looks unchanged after Done (a solved
  inline widget can stay), the card says so and pressing Done a second time on that same page is the
  owner's own confirmation; the audit row records `verified_by: owner_confirmed`.
* **One-time code.** The bot calls `browser_handover` with `kind: code`. A small card names the site
  (the origin the *browser* reports), shows a countdown and a paste-friendly field (spaces and dashes
  are removed). The daemon types the code into the page's code field; the model only hears `done`.
* No third-party solving service, no bundled solver, no stealth or evasion code.

## How detection works

`handover::detect` reads the rendered snapshot text (a CAPTCHA frame, "I'm not a robot", "verify you are
human", hCaptcha/Turnstile, press-and-hold; a code field named verification/security/one-time code, OTP,
2FA, in English, Turkish and German, or a "we sent a code" sentence next to a text field; coupon,
postal-code and password fields are excluded). `browser_open` / `browser_snapshot` then end with a
`[Praxi notice]` line written by the daemon. Page text cannot trigger a card by itself:
`browser_handover` re-reads the page and refuses unless the prompt is really there, and the card text
is fixed app text.

## Security properties

* The code is never stored, never in the model's context, the journal, the audit log (`handover.code_typed`
  names the kind only), an error message, `Debug` output or the response. It lives in one local variable
  during the typing and goes through `BrowserDriver::fill_secret`, which refuses unless the receiving
  frame is on the origin the card named, re-read from the browser right before the text is inserted.
  The field is found again on a fresh snapshot, so a moved-on page or a stale ref never receives it.
* Owner-only routes (`owner.rs`, refused to every paired peer, local actor re-checked). A failed answer
  (bad code, page moved, check still there) keeps the request waiting.
* Only a run the owner started at the desktop can hand over; other initiators are refused up front.
  Per run: at most `max_handovers_per_run` (4), timeout `handover_timeout` (5 minutes, never longer
  than the approval timeout).
* A page can imitate a 2FA prompt to phish a code. The card therefore names the browser-reported site,
  and shows a stronger warning when the run has read untrusted content.
* After a code is typed, the page is latched until the next main-frame navigation: screenshots, `eval`
  (also inside the page itself, so another tab or a subagent lane asking cannot release it), live-view
  frames and desktop reads are refused; every page read masks the field that got the code and, because a
  page may copy the value, every text-entry value on the page, and drops short code-like text.
* Category `meta`, effect `read`: the card IS the decision, so there is no second approval card.

## Not done

* "Use my e-mail" (reading the code from a connected mailbox) is not offered yet; it needs a daemon-side
  mail read that never passes the mail text to the model.
* Vision: the notice tells the bot to hand over if it cannot see images; the daemon does not check the
  model's vision capability itself.
* Only the managed browser: desktop (non-browser) 2FA prompts are not covered.

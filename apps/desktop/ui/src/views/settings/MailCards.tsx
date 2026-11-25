import type { TFunction } from "i18next";
import { KeyRound } from "lucide-react";
import { Button } from "ui";
import { SettingRow } from "../../components/SettingsUi";
import { openVault } from "../../lib/settingsLink";
import { EmailCard } from "./EmailCard";
import { EmailChannelCard } from "./EmailChannelCard";
import { MailGuardCard } from "./MailGuardCard";
import type { CardDef } from "./types";

/** Cards of the "Apps & connections" mail/vault area. */
export function mailCards(t: TFunction): CardDef[] {
  return [
    {
      id: "vault",
      section: "mail",
      kind: "row",
      title: t("vault.title"),
      description: t("vault.subtitle"),
      keywords: "vault kasa tresor boveda password login account secret sifre hesap",
      node: (
        <SettingRow title={t("vault.title")} description={t("vault.subtitle")}>
          <Button
            size="sm"
            variant="secondary"
            data-testid="settings-open-vault"
            onClick={() => openVault()}
          >
            <KeyRound aria-hidden="true" />
            {t("vault.openButton")}
          </Button>
        </SettingRow>
      )
    },
    {
      id: "email",
      section: "mail",
      kind: "block",
      title: t("settingsModal.email.title"),
      description: t("settingsModal.email.description"),
      keywords: "mail e-posta eposta imap smtp gmail outlook yahoo icloud mailbox posta kutusu agentmail postfach",
      node: <EmailCard />
    },
    {
      id: "email-channel",
      section: "mail",
      kind: "block",
      title: t("settingsModal.emailChannel.title"),
      description: t("settingsModal.emailChannel.description"),
      keywords: "mail e-posta eposta inbox gelen kutusu posteingang channel kanal bot reply yanit antwort",
      node: <EmailChannelCard />
    },
    {
      id: "mail-guard",
      section: "mail",
      kind: "block",
      title: t("settingsModal.mailGuard.title"),
      description: t("settingsModal.mailGuard.description"),
      keywords: "mail e-posta eposta opt-out unsubscribe abonelik çık çıkış engel gönderim günlüğü send log limit sperrliste abmelden versandprotokoll",
      node: <MailGuardCard />
    }
  ];
}

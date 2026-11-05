import { AccountsPanel } from "../components/AccountsPanel";
import { useI18n } from "../lib/i18n";

export default function OverviewPage() {
  const { t } = useI18n();

  return (
    <div className="space-y-6">
      <header>
        <h1 className="text-xl font-semibold text-fg">{t.overview.title}</h1>
        <p className="mt-1 text-sm text-muted">{t.overview.subtitle}</p>
      </header>

      <AccountsPanel />
    </div>
  );
}

import { CheckCheck, ChevronLeft, ChevronRight, ExternalLink } from "lucide-react";
import { useMemo, useState } from "react";

import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../components/ui/card";
import { useNews } from "../hooks/useApi";
import { formatDateTime } from "../lib/format";
import { fmt, useI18n } from "../lib/i18n";
import {
  NEWS_SOURCES,
  countUnread,
  isSubscribed,
  markRead,
  toggleSubscription,
  useNewsPrefs,
} from "../lib/news";

const PAGE_SIZE = 15;

export default function NewsPage() {
  const { t } = useI18n();
  const news = useNews(200);
  const prefs = useNewsPrefs();
  const [page, setPage] = useState(1);

  const items = news.data?.items ?? [];
  const statuses = news.data?.sources ?? [];
  const subscribedSources = NEWS_SOURCES.filter((source) => isSubscribed(prefs, source));
  const unread = countUnread(prefs, items);

  const visible = useMemo(
    () => items.filter((item) => isSubscribed(prefs, item.source)),
    [items, prefs],
  );

  const pages = Math.max(1, Math.ceil(visible.length / PAGE_SIZE));
  const currentPage = Math.min(page, pages);
  const paged = visible.slice((currentPage - 1) * PAGE_SIZE, currentPage * PAGE_SIZE);

  const sourceName = (source: string) =>
    t.news.sources[source as keyof typeof t.news.sources]?.name ?? source;

  return (
    <div className="space-y-6">
      <header>
        <h1 className="text-xl font-semibold text-fg">{t.news.title}</h1>
        <p className="mt-1 text-sm text-muted">{t.news.subtitle}</p>
      </header>

      <Card>
        <CardHeader>
          <div>
            <CardTitle>{t.news.sourcesTitle}</CardTitle>
            <CardDescription>{fmt(t.news.count, { count: subscribedSources.length })}</CardDescription>
          </div>
        </CardHeader>
        <CardContent className="p-0">
          <ul>
            {NEWS_SOURCES.map((source) => {
              const status = statuses.find((entry) => entry.source === source);
              const subscribed = isSubscribed(prefs, source);
              return (
                <li
                  key={source}
                  className="flex flex-wrap items-center gap-3 border-t border-line px-4 py-3 first:border-t-0"
                >
                  <div className="min-w-0 flex-1">
                    <div className="flex flex-wrap items-center gap-2">
                      <span className="text-sm font-medium text-fg">{t.news.sources[source].name}</span>
                      <Badge tone={subscribed ? "success" : "neutral"}>
                        {subscribed ? t.news.subscribed : t.news.unsubscribed}
                      </Badge>
                      {status?.last_error && <Badge tone="danger">{t.news.failed}</Badge>}
                    </div>
                    <p className="mt-0.5 text-xs text-muted">{t.news.sources[source].desc}</p>
                    <p className="mt-1 text-[11px] text-subtle">
                      {status?.last_success_at
                        ? fmt(t.news.updatedAt, { value: formatDateTime(status.last_success_at) })
                        : t.news.never}
                      {" · "}
                      {fmt(t.news.count, { count: status?.count ?? 0 })}
                    </p>
                  </div>
                  <Button
                    size="sm"
                    variant={subscribed ? "ghost" : "secondary"}
                    onClick={() => {
                      toggleSubscription(source);
                      setPage(1);
                    }}
                  >
                    {subscribed ? t.news.toggleOff : t.news.toggleOn}
                  </Button>
                </li>
              );
            })}
          </ul>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <div>
            <CardTitle>{t.news.feedTitle}</CardTitle>
            <CardDescription>{fmt(t.news.unread, { count: unread })}</CardDescription>
          </div>
          <Button
            size="sm"
            variant="ghost"
            disabled={unread === 0}
            onClick={() => markRead(subscribedSources)}
          >
            <CheckCheck size={13} />
            {t.news.allRead}
          </Button>
        </CardHeader>
        <CardContent className="p-0">
          {visible.length === 0 ? (
            <p className="px-4 py-12 text-center text-sm text-muted">
              {subscribedSources.length === 0 ? t.news.empty : t.news.noMatch}
            </p>
          ) : (
            <ul>
              {paged.map((item) => {
                const isNew = Date.parse(item.published_at) > (prefs.readAt[item.source] ?? 0);
                return (
                  <li
                    key={item.id}
                    className="flex flex-wrap items-start gap-3 border-t border-line px-4 py-3 transition-colors first:border-t-0 hover:bg-hover"
                  >
                    <Badge tone="neutral">{sourceName(item.source)}</Badge>
                    <div className="min-w-0 flex-1">
                      <p className="text-sm text-body">{item.title}</p>
                      <p className="mt-1 text-[11px] text-subtle">{formatDateTime(item.published_at)}</p>
                    </div>
                    {isNew && <Badge tone="info">{t.news.newBadge}</Badge>}
                    {item.url && (
                      <a
                        href={item.url}
                        target="_blank"
                        rel="noreferrer"
                        className="inline-flex items-center gap-1 text-[11px] text-accent transition-colors hover:text-accent/80"
                      >
                        {t.news.open}
                        <ExternalLink size={11} />
                      </a>
                    )}
                  </li>
                );
              })}
            </ul>
          )}
          {visible.length > 0 && (
            <div className="flex flex-wrap items-center justify-between gap-2 border-t border-line px-4 py-2 text-[11px] text-subtle">
              <span>{fmt(t.news.pageInfo, { page: currentPage, pages })}</span>
              <div className="flex items-center gap-1">
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={currentPage <= 1}
                  onClick={() => setPage(currentPage - 1)}
                >
                  <ChevronLeft size={13} />
                  {t.news.prevPage}
                </Button>
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={currentPage >= pages}
                  onClick={() => setPage(currentPage + 1)}
                >
                  {t.news.nextPage}
                  <ChevronRight size={13} />
                </Button>
              </div>
            </div>
          )}
        </CardContent>
      </Card>
    </div>
  );
}

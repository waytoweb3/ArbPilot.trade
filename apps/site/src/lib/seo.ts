import { useEffect } from "react";

import { copyFor, type Locale } from "./i18n";
import { legalDocs } from "./legal";

/** 官网正式域名；canonical、og:url 与 sitemap 都以它为准。 */
export const SITE_URL = "https://arbpilot.trade";

export type PageMeta = {
  /** 路由路径，用于 sitemap 与预渲染落盘。 */
  path: string;
  title: string;
  description: string;
  canonical: string;
  noindex: boolean;
};

/** 法律页、价格页与资金费用页的独立描述；首页沿用 i18n 里的站点描述。 */
const DESCRIPTIONS: Record<
  Locale,
  Record<"pricing" | "privacy" | "terms" | "funding", string>
> = {
  zh: {
    pricing:
      "ArbPilot 价格：Google 登录后可免费查看行情并创建、发布策略；自托管部署 $19/月，用于运行策略与套利模块；策略由作者定价，平台按成交额抽成（冷启动期 6 个月 10%，之后 20%）。",
    privacy:
      "ArbPilot 隐私政策：说明订阅与访问官网时我们收集哪些信息、如何使用与共享，以及自托管架构下 API 密钥、订单与交易数据为何始终留在你的设备。",
    terms:
      "ArbPilot 服务条款：订阅与计费、许可范围、退款、风险提示与责任限制，适用于自托管套利终端及其模块。",
    funding:
      "ArbPilot 全市场资金费率看板：实时聚合 8 家交易所的 USDT 本位永续资金费率，币种为行、交易所为列，支持搜索、按年化排序与下次结算倒计时。",
  },
  en: {
    pricing:
      "ArbPilot pricing: free read-only markets plus free strategy creation and publishing with Google sign-in; self-hosted deployment is $19/mo for running strategies and arbitrage modules, and the platform takes 10% of strategy sales for the first 6 months, then 20%.",
    privacy:
      "ArbPilot privacy policy: what we collect when you subscribe or browse the site, how it is used, and why API keys, orders and trading data stay on your device.",
    terms:
      "ArbPilot terms of service: subscriptions and billing, license scope, refunds, risk disclosures and liability limits for the self-hosted arbitrage terminal.",
    funding:
      "ArbPilot market-wide funding dashboard: live USDT-margined perpetual funding rates from 8 venues, coins as rows and venues as columns, with search, annualised sorting and next-settlement countdowns.",
  },
};

/** 按路径解析页面标题与描述；预渲染脚本与客户端路由共用同一份规则。 */
export function metaForPath(pathname: string, locale: Locale): PageMeta {
  const copy = copyFor(locale);
  const path = normalize(pathname);

  if (path === "/privacy" || path === "/terms") {
    const slug = path === "/privacy" ? "privacy" : "terms";
    const doc = legalDocs[locale][slug];
    return build(path, `${doc.title} · ArbPilot`, DESCRIPTIONS[locale][slug], {});
  }

  if (path === "/funding") {
    return build(
      path,
      `${copy.fundingPage.title} · ArbPilot`,
      DESCRIPTIONS[locale].funding,
      {},
    );
  }

  if (path === "/pricing") {
    // /pricing 渲染的是首页内容，统一 canonical 到首页，避免重复内容。
    return build(path, `${copy.nav.pricing} · ArbPilot`, DESCRIPTIONS[locale].pricing, {
      canonicalPath: "/",
    });
  }

  if (path === "/device" || path === "/checkout/success") {
    return build(path, copy.meta.title, copy.meta.description, { noindex: true });
  }

  // 运营后台不收录，也不出现在 sitemap 里。
  if (path === "/admin") {
    return build(path, "运营后台 · ArbPilot", "ArbPilot 运营后台，仅限授权人员访问。", {
      noindex: true,
    });
  }

  if (path === "/") {
    return build("/", copy.meta.title, copy.meta.description, {});
  }

  // 其它路径都是 404：不收录，也不进 sitemap。
  return build("/404", `${copy.notFound.title} · ArbPilot`, copy.notFound.body, {
    noindex: true,
  });
}

/** 客户端路由切换时同步 head，避免每个页面各写一遍 effect。 */
export function usePageMeta(meta: PageMeta) {
  const { title, description, canonical, noindex } = meta;

  useEffect(() => {
    document.title = title;
    setNamedMeta("description", description);
    setNamedMeta("robots", noindex ? "noindex,nofollow" : "index,follow");
    setPropertyMeta("og:title", title);
    setPropertyMeta("og:description", description);
    setPropertyMeta("og:url", canonical);
    setNamedMeta("twitter:title", title);
    setNamedMeta("twitter:description", description);

    const link = document.head.querySelector<HTMLLinkElement>('link[rel="canonical"]');
    if (link) {
      link.href = canonical;
    }
  }, [title, description, canonical, noindex]);
}

function build(
  path: string,
  title: string,
  description: string,
  options: { canonicalPath?: string; noindex?: boolean },
): PageMeta {
  return {
    path,
    title,
    description,
    canonical: `${SITE_URL}${options.canonicalPath ?? path}`,
    noindex: options.noindex ?? false,
  };
}

function normalize(pathname: string): string {
  const trimmed = pathname.replace(/\/+$/, "");
  return trimmed === "" ? "/" : trimmed;
}

function setNamedMeta(name: string, content: string) {
  let element = document.head.querySelector<HTMLMetaElement>(`meta[name="${name}"]`);
  if (!element) {
    element = document.createElement("meta");
    element.name = name;
    document.head.appendChild(element);
  }
  element.content = content;
}

function setPropertyMeta(property: string, content: string) {
  let element = document.head.querySelector<HTMLMetaElement>(`meta[property="${property}"]`);
  if (!element) {
    element = document.createElement("meta");
    element.setAttribute("property", property);
    document.head.appendChild(element);
  }
  element.content = content;
}

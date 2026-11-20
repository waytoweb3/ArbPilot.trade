import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";

export type Locale = "zh" | "en";

const STORAGE_KEY = "arbpilot.locale";

const zh = {
  meta: {
    title: "ArbPilot · 自托管跨所套利终端",
    description:
      "ArbPilot 是自托管的量化套利终端：桌面端负责配置与监控，Rust 执行器在你的电脑或服务器上连接交易所，全市场资金费率、新闻事件与自定义策略统一过风控，密钥不离开你的设备。",
  },
  nav: {
    home: "首页介绍",
    funding: "资金费用",
    news: "新闻采集",
    strategy: "自定义策略",
    pricing: "价格",
    cta: "下载客户端",
    language: "切换语言",
    theme: "切换主题",
    light: "浅色",
    dark: "深色",
  },
  hero: {
    eyebrow: "自托管 · 跨交易所套利终端",
    title1: "跨所套利，",
    title2: "在自己手里执行。",
    body: "ArbPilot 把资金费率、新闻事件与策略执行放进一个自托管终端。桌面端负责配置与监控，Rust 执行器运行在你的电脑或服务器上连接交易所——资金留在交易所，API 密钥从不离开你的设备。",
    cta1: "订阅模块",
    cta2: "查看资金费用",
    guarantees: ["所有订单统一过风控", "密钥只留在你的设备", "桌面端 + Telegram 双入口"],
  },
  venues: {
    eyebrow: "已接入的衍生品市场",
    note: "资金费率采集已接入 Binance、OKX、Bybit、Bitget、Gate、Aster、Hyperliquid 与 Lighter，覆盖 2,500+ USDT 本位永续交易对。",
  },
  live: {
    header: "Order Book → Execution",
    demo: "演示数据",
    pair: "BTC / USDT · 永续",
    gross: "毛价差",
    buyLeg: "买入腿",
    sellLeg: "卖出腿",
    fee: "费用",
    slippage: "滑点",
    net: "净边际",
    steps: ["盘口快照", "风控预检", "双腿提交", "成交回报"],
    note: "执行示意：买入看卖盘、卖出看买盘，条件与风控通过后同时提交双腿。数据为本地模拟，不代表真实成交。",
  },
  funding: {
    eyebrow: "Funding Rates",
    title1: "全市场资金费率，",
    title2: "实时看得见。",
    desc: "执行器持续采集 8 家交易所的 USDT 本位永续资金费率，覆盖 2,500+ 交易对。当期费率、下次结算倒计时与历史结算记录集中在同一张表，币种为行、交易所为列，点击任意列头即可排序。",
    bullets: [
      "全市场采集：一次拉齐 8 家交易所的永续合约清单",
      "费率变动实时高亮，上涨闪绿、下跌闪红",
      "历史结算记录用于区间累计与年化换算",
      "正负费率之差，就是资金费率套利空间",
    ],
    panelTitle: "USDT 永续 · 当期资金费率",
    pairLabel: "交易对",
    demo: "演示数据",
    note: "表格为界面演示，非实时行情。负值表示空头向多头收取。",
    stat1: "覆盖合约",
    stat1Value: "2,500+",
    stat2: "接入交易所",
    stat2Value: "8",
    stat3: "刷新间隔",
    stat3Value: "30s",
  },
  fundingPage: {
    eyebrow: "Live Funding",
    title: "全市场资金费率",
    desc: "官网直接读取官方只读实例的公共资金费率快照：币种为行、交易所为列，正值为多头付空头。金额按各自合约的实际结算间隔折算年化，可搜索交易对或切换排序。",
    statVenues: "接入交易所",
    statPairs: "覆盖交易对",
    statRefresh: "刷新间隔",
    updated: "更新于",
    search: "搜索交易对，例如 BTCUSDT",
    pair: "交易对",
    maxAnnualized: "最高年化",
    nextSettlement: "下次结算",
    empty: "没有匹配的交易对",
    loading: "正在读取官方只读数据…",
    failed: "暂时无法读取资金费率，请稍后重试。",
    retry: "重试",
    noData: "官方实例暂未启用资金费率采集。",
    sortHint: "点击表头可按该列排序",
    pairsUnit: "个交易对",
    page: "第",
    prev: "上一页",
    next: "下一页",
    source: "数据来自 ArbPilot 官方只读实例，每 30 秒自动刷新，仅供参考，不构成投资建议。",
  },
  news: {
    eyebrow: "News & Trading",
    title1: "新闻采集，",
    title2: "先于价格一步。",
    desc: "聚合交易所公告、项目动态与主流媒体快讯，按标的与事件类型归类。命中你关注的事件规则后推送到桌面端与 Telegram，并附上对应的合约与交易介绍。",
    bullets: [
      "多源采集与去重，按币种、板块与事件类型筛选",
      "关键词与事件规则触发告警，不必盯屏",
      "新闻自动映射到可交易合约",
      "事件驱动下单仍须通过统一风控预检",
    ],
    panelTitle: "最新事件",
    demo: "演示数据",
    items: [
      {
        time: "09:41",
        tag: "上线",
        title: "某交易所新增 XYZUSDT 永续，最高 20x 杠杆",
        symbol: "XYZUSDT",
      },
      {
        time: "09:17",
        tag: "解锁",
        title: "ABC 代币 2.4% 供应量将于本周解锁",
        symbol: "ABCUSDT",
      },
      {
        time: "08:52",
        tag: "费率",
        title: "DEF 资金费率转为 -0.031%，空头开始付费",
        symbol: "DEFUSDT",
      },
    ],
    tradeTitle: "交易介绍",
    tradeDesc: "事件命中后不会直接下单：先映射到合约，再过风控预检，确认仓位、滑点与行情时效后才进入执行。",
    flow: ["事件命中", "标的映射", "风控预检", "双腿执行", "成交回报"],
    note: "示例事件与标的仅用于展示界面结构，不构成投资建议。",
  },
  strategy: {
    eyebrow: "Custom Strategy",
    title1: "把自己的判断，",
    title2: "写成可执行的策略。",
    desc: "在桌面端编写策略参数与入场条件，先用历史行情回放与回测验证边际，确认后再交给执行器按统一风控自动执行，每个信号都有完整留痕。",
    bullets: [
      "内置模板：资金费率中性、跨所价差、事件驱动",
      "历史回放与回测，先验证再上线",
      "策略与手动交易共用一套风控与幂等执行链路",
      "多执行器部署，策略与账户按需拆分",
    ],
    panelTitle: "策略回测",
    demo: "演示数据",
    metricAnnual: "年化收益",
    metricDrawdown: "最大回撤",
    metricSharpe: "夏普比率",
    metricWinRate: "胜率",
    templates: [
      { name: "永续费率差", body: "同一标的两家永续对冲，做空高费率、做多低费率" },
      { name: "跨所价差", body: "价差突破阈值后双腿建仓，回归时平仓" },
      { name: "事件驱动", body: "新闻命中后按标的与方向快速建仓" },
    ],
    tradeTitle: "交易介绍",
    flow: ["信号生成", "风控预检", "双腿执行", "持仓对账", "平仓结算"],
    note: "回测指标为演示数据，不代表真实收益，历史表现不代表未来。",
  },
  download: {
    eyebrow: "Download",
    title1: "安装包，",
    title2: "按平台取用。",
    desc: "桌面端提供 macOS 与 Windows 安装包，执行器可随桌面端一起启动，也可以单独部署到你的服务器。安装包签名与版本说明随每次发布更新。",
    version: "当前版本",
    macos: "macOS",
    macosArm: "Apple Silicon",
    windows: "Windows",
    windowsDesc: "x64 · 安装程序",
    download: "下载安装包",
    note: "安装包为预览版本，下载后请核对版本号与文件大小；升级前请先备份本地数据库与配置。",
    sums: "SHA-256 校验文件",
    copy: "复制",
    copied: "已复制",
    winTitle: "Windows 首次安装提示",
    winDesc: "安装包未购买商业代码签名证书，SmartScreen 可能提示「Windows 已保护你的电脑」。这是未签名程序的默认行为，与文件是否安全无关；安装前请先核对下方 SHA-256。",
    winSteps: [
      "点击提示框里的「更多信息」",
      "点击「仍要运行」",
      "仍被拦截时：右键安装包 → 属性 → 勾选「解除锁定」→ 确定",
    ],
    winVerify: "PowerShell 核对：Get-FileHash .\\{file} -Algorithm SHA256",
  },
  pricing: {
    eyebrow: "Pricing",
    title1: "Google 登录，",
    title2: "按能力订阅。",
    desc: "非托管与托管用户统一使用 Google 登录。$19/月 开通自托管部署、套利与新闻，并在本机运行策略；创建与发布策略对所有登录用户免费，平台只按策略成交额抽成（冷启动期 6 个月 10%，之后 20%）。",
    freeName: "Free",
    freeForever: "免费",
    freeDesc: "Google 登录后查看行情、资金费率与资讯，并可创建与发布策略。",
    freeFeatures: [
      "Google 账号登录",
      "行情、资金费率与新闻只读",
      "官方托管数据接口",
      "创建并发布策略，发布免费",
    ],
    freeCta: "免费开始",
    arbitrageName: "自托管部署",
    arbitrageDesc: "开通自托管执行器、套利与新闻，并可在本机运行策略。",
    arbitrageFeatures: [
      "自托管执行器与一键部署",
      "资金费率与跨所价差套利",
      "新闻筛选与 Telegram 告警",
      "运行策略与使用市场策略",
    ],
    arbitrageCta: "下载客户端订阅",
    popular: "推荐",
    perMonth: "/ 月",
    payment: "支付由 Waffo Pancake 作为记录商户处理，卡数据不经过我们的服务器。",
    policies: "订阅即表示同意服务条款与隐私政策；退款政策见服务条款第 7 条。",
    note: "价格以美元计价，不含适用税费。策略由作者定价，可免费或一次性买断（永久授权）；平台按成交额抽成，冷启动期 6 个月 10%，之后 20%，免费策略不抽成。",
  },
  checkout: {
    eyebrow: "Checkout",
    title: "正在确认订阅…",
    pending: "支付结果确认中，通常需要几秒钟。请不要关闭此页面。",
    active: "订阅已开通",
    activeDescGeneric: "授权已生效，回到桌面端即可看到订阅状态。",
    canceled: "订阅已取消",
    canceling: "已安排取消，在当前计费周期结束前仍然有效。",
    canceledDesc: "订阅已结束。如需继续使用，可以重新订阅。",
    pastDue: "最近一次扣款失败，请更新支付方式。",
    expired: "订阅已过期。",
    missing: "没有找到该订单",
    missingDesc: "请从定价页重新发起订阅，或联系支持并提供订单号。",
    plan: "订阅方案",
    status: "状态",
    expires: "当前周期结束",
    back: "返回首页",
    pricing: "查看定价",
    slow: "确认时间较长？可以稍后回到桌面端查看授权状态。",
  },
  legal: {
    updated: "最后更新",
    version: "版本",
    back: "返回首页",
    toc: "本页目录",
    related: "相关页面",
    privacy: "隐私政策",
    terms: "服务条款",
  },
  footer: {
    body: "自托管量化交易终端。桌面端配置与监控，Rust 执行器在本地完成行情、风控与订单执行。",
    product: "产品",
    resources: "资源",
    legal: "法律",
    links: ["首页介绍", "资金费用", "新闻采集", "自定义策略"],
    discord: "Discord 社区",
    privacy: "隐私政策",
    terms: "服务条款",
    disclaimer:
      "数字资产与衍生品交易存在亏损风险。ArbPilot 提供的行情、机会与自动化执行不构成投资建议，也不承诺套利无风险或保证盈利。请根据自身情况评估风险并控制仓位。",
    copyright: "© 2026 ArbPilot · 自托管 · 资金与密钥始终由你掌控",
  },
  notFound: {
    eyebrow: "404",
    title: "这个页面不存在",
    body: "链接可能已经失效，或者地址拼错了。回到首页可以继续查看资金费率、新闻采集与自定义策略。",
    back: "返回首页",
  },
};

const en: typeof zh = {
  meta: {
    title: "ArbPilot · Self-hosted cross-venue arbitrage terminal",
    description:
      "ArbPilot is a self-hosted quantitative arbitrage terminal: the desktop configures and monitors while a Rust executor on your machine or server connects to exchanges. Market-wide funding rates, news events and custom strategies all pass the same risk checks, and keys never leave your device.",
  },
  nav: {
    home: "Overview",
    funding: "Funding rates",
    news: "News feed",
    strategy: "Strategies",
    pricing: "Pricing",
    cta: "Download",
    language: "Switch language",
    theme: "Toggle theme",
    light: "Light",
    dark: "Dark",
  },
  hero: {
    eyebrow: "Self-hosted · Cross-venue arbitrage terminal",
    title1: "Cross-venue arbitrage,",
    title2: "executed on your own terms.",
    body: "ArbPilot puts funding rates, news events and strategy execution into one self-hosted terminal. The desktop handles configuration and monitoring while a Rust executor runs on your machine or server — funds stay on the exchange and API keys never leave your device.",
    cta1: "Subscribe",
    cta2: "See funding rates",
    guarantees: ["Every order passes risk checks", "Keys stay on your device", "Desktop + Telegram access"],
  },
  venues: {
    eyebrow: "Connected derivatives venues",
    note: "Funding-rate collection covers Binance, OKX, Bybit, Bitget, Gate, Aster, Hyperliquid and Lighter — 2,500+ USDT-margined perpetual pairs.",
  },
  live: {
    header: "Order Book → Execution",
    demo: "Simulated",
    pair: "BTC / USDT · Perpetual",
    gross: "Gross edge",
    buyLeg: "Buy leg",
    sellLeg: "Sell leg",
    fee: "Fees",
    slippage: "Slippage",
    net: "Net edge",
    steps: ["Book snapshot", "Risk preflight", "Two-leg submit", "Fill report"],
    note: "Execution sketch: buy against the ask, sell against the bid, submit both legs once conditions and risk checks pass. Data is simulated locally, not real fills.",
  },
  funding: {
    eyebrow: "Funding Rates",
    title1: "Market-wide funding rates,",
    title2: "visible in real time.",
    desc: "The executor continuously collects USDT-margined perpetual funding rates from 8 venues, covering 2,500+ pairs. Current rates, next-settlement countdowns and settled history share one table — coins as rows, venues as columns, sortable from any header.",
    bullets: [
      "Market-wide collection: every perpetual listed on all 8 venues",
      "Rates flash green on a rise and red on a fall as they change",
      "Settled history powers interval totals and annualised rates",
      "The gap between positive and negative rates is the funding arbitrage",
    ],
    panelTitle: "USDT perpetuals · Current funding",
    pairLabel: "Pair",
    demo: "Simulated",
    note: "The table demonstrates the interface and is not live market data. Negative values mean shorts pay longs.",
    stat1: "Contracts",
    stat1Value: "2,500+",
    stat2: "Venues",
    stat2Value: "8",
    stat3: "Refresh",
    stat3Value: "30s",
  },
  fundingPage: {
    eyebrow: "Live Funding",
    title: "Market-wide funding rates",
    desc: "Funding snapshots are read straight from the official read-only instance: coins are rows, venues are columns, and positive rates mean longs pay shorts. Rates are annualised with each market's actual settlement interval, and you can search pairs or switch the sort.",
    statVenues: "Venues",
    statPairs: "Pairs",
    statRefresh: "Refresh",
    updated: "Updated",
    search: "Search a pair, e.g. BTCUSDT",
    pair: "Pair",
    maxAnnualized: "Max APY",
    nextSettlement: "Next settlement",
    empty: "No pairs match this search",
    loading: "Loading the official read-only data…",
    failed: "Funding rates are temporarily unavailable. Please retry shortly.",
    retry: "Retry",
    noData: "Funding collection is not enabled on the official instance.",
    sortHint: "Click a column header to sort by it",
    pairsUnit: "pairs",
    page: "Page",
    prev: "Prev",
    next: "Next",
    source: "Data comes from the ArbPilot official read-only instance, refreshed every 30 seconds. For reference only, not investment advice.",
  },
  news: {
    eyebrow: "News & Trading",
    title1: "News collection,",
    title2: "one step ahead of price.",
    desc: "Exchange announcements, project updates and media flashes are aggregated and grouped by instrument and event type. When a rule you follow matches, the event is pushed to the desktop and Telegram together with the tradable contract and a trade brief.",
    bullets: [
      "Multi-source collection with de-duplication, filtered by coin, sector and event type",
      "Keyword and event rules trigger alerts so you don't have to watch screens",
      "Every news item maps to a tradable contract",
      "Event-driven entries still pass the same risk preflight",
    ],
    panelTitle: "Latest events",
    demo: "Simulated",
    items: [
      {
        time: "09:41",
        tag: "Listing",
        title: "A venue lists XYZUSDT perpetuals with up to 20x leverage",
        symbol: "XYZUSDT",
      },
      {
        time: "09:17",
        tag: "Unlock",
        title: "2.4% of ABC supply unlocks this week",
        symbol: "ABCUSDT",
      },
      {
        time: "08:52",
        tag: "Funding",
        title: "DEF funding flips to -0.031%, shorts start paying",
        symbol: "DEFUSDT",
      },
    ],
    tradeTitle: "How the trade works",
    tradeDesc: "A matched event never trades directly: it maps to a contract, passes risk preflight, then executes only if size, slippage and quote age all check out.",
    flow: ["Event match", "Instrument map", "Risk preflight", "Two-leg execution", "Fill report"],
    note: "Sample events and instruments demonstrate the interface only and are not investment advice.",
  },
  strategy: {
    eyebrow: "Custom Strategy",
    title1: "Turn your own thesis",
    title2: "into an executable strategy.",
    desc: "Write parameters and entry conditions in the desktop app, validate the edge with historical replay and backtests, then let the executor run it through the same risk controls. Every signal is fully logged.",
    bullets: [
      "Ready-made templates: funding-neutral, cross-venue spread, event-driven",
      "Historical replay and backtesting before anything goes live",
      "Strategies share the same risk and idempotent execution path as manual trades",
      "Multiple executors let you split strategies and accounts",
    ],
    panelTitle: "Backtest",
    demo: "Simulated",
    metricAnnual: "Annualised",
    metricDrawdown: "Max drawdown",
    metricSharpe: "Sharpe",
    metricWinRate: "Win rate",
    templates: [
      { name: "Perp funding spread", body: "Hedge the same pair across two perpetuals: short the higher rate, long the lower" },
      { name: "Cross-venue spread", body: "Enter both legs when the spread breaks a threshold, exit on reversion" },
      { name: "Event-driven", body: "Open quickly on the mapped instrument and direction once news hits" },
    ],
    tradeTitle: "How the trade works",
    flow: ["Signal", "Risk preflight", "Two-leg execution", "Position reconciliation", "Close & settle"],
    note: "Backtest metrics are simulated. They are not real returns, and past performance does not guarantee future results.",
  },
  download: {
    eyebrow: "Download",
    title1: "Installers,",
    title2: "built for your platform.",
    desc: "The desktop ships installers for macOS and Windows. The executor can start alongside the desktop app or run on your own server. Build signatures and release notes are updated with every release.",
    version: "Current build",
    macos: "macOS",
    macosArm: "Apple Silicon",
    windows: "Windows",
    windowsDesc: "x64 · installer",
    download: "Download",
    note: "Installers are preview builds. Verify the version and file size after downloading, and back up your local database before upgrading.",
    sums: "SHA-256 checksum files",
    copy: "Copy",
    copied: "Copied",
    winTitle: "First install on Windows",
    winDesc: "The installer is not backed by a commercial code-signing certificate, so SmartScreen may show “Windows protected your PC”. That is normal for unsigned builds and says nothing about the file's safety — verify the SHA-256 below before installing.",
    winSteps: [
      "Click “More info” in the warning",
      "Click “Run anyway”",
      "Still blocked: right-click the installer → Properties → check “Unblock” → OK",
    ],
    winVerify: "Verify in PowerShell: Get-FileHash .\\{file} -Algorithm SHA256",
  },
  pricing: {
    eyebrow: "Pricing",
    title1: "Sign in with Google,",
    title2: "subscribe by capability.",
    desc: "Self-hosted and managed users both sign in with Google. $19/mo unlocks self-hosted deployment, arbitrage and news, and running strategies locally; creating and publishing strategies is free for every signed-in user, and the platform only takes a cut of strategy sales (10% for the first 6 months, then 20%).",
    freeName: "Free",
    freeForever: "Free",
    freeDesc: "Sign in with Google to view markets, funding and news, and to create and publish strategies.",
    freeFeatures: [
      "Google sign-in",
      "Read-only markets, funding and news",
      "Official managed data endpoints",
      "Create and publish strategies for free",
    ],
    freeCta: "Start free",
    arbitrageName: "Self-hosted Deployment",
    arbitrageDesc: "Unlock the self-hosted executor, arbitrage and news, and run strategies locally.",
    arbitrageFeatures: [
      "Self-hosted executor and one-click deploy",
      "Funding-rate and cross-exchange spread arbitrage",
      "News filtering and Telegram alerts",
      "Run strategies and use market strategies",
    ],
    arbitrageCta: "Download app to subscribe",
    popular: "Recommended",
    perMonth: "/ mo",
    payment: "Payments are processed by Waffo Pancake as Merchant of Record. Card data never touches our servers.",
    policies: "Subscribing means you accept the Terms of Service and Privacy Policy; refunds are covered by Section 7 of the Terms.",
    note: "Prices are in USD and exclude applicable taxes. Strategies are priced by their authors, free or one-time purchase with lifetime access; the platform takes 10% of sales for the first 6 months, then 20%, and free strategies take no cut.",
  },
  checkout: {
    eyebrow: "Checkout",
    title: "Confirming your subscription…",
    pending: "Confirming payment, usually a few seconds. Please keep this page open.",
    active: "Subscription active",
    activeDescGeneric: "Your license is active. Head back to the desktop app to see the status.",
    canceled: "Subscription cancelled",
    canceling: "Cancellation scheduled; the subscription stays active until the end of the current billing period.",
    canceledDesc: "The subscription has ended. Subscribe again to keep using ArbPilot Pro.",
    pastDue: "The last charge failed. Please update your payment method.",
    expired: "Subscription expired.",
    missing: "Order not found",
    missingDesc: "Start again from the pricing page, or contact support with your order ID.",
    plan: "Plan",
    status: "Status",
    expires: "Current period ends",
    back: "Back to home",
    pricing: "See pricing",
    slow: "Taking longer than usual? You can check the license status in the desktop app later.",
  },
  legal: {
    updated: "Last updated",
    version: "Version",
    back: "Back to home",
    toc: "On this page",
    related: "Related",
    privacy: "Privacy Policy",
    terms: "Terms of Service",
  },
  footer: {
    body: "A self-hosted quantitative trading terminal. The desktop configures and monitors while the Rust executor handles market data, risk and order execution locally.",
    product: "Product",
    resources: "Resources",
    legal: "Legal",
    links: ["Overview", "Funding rates", "News feed", "Strategies"],
    discord: "Discord community",
    privacy: "Privacy Policy",
    terms: "Terms of Service",
    disclaimer:
      "Digital asset and derivatives trading carries the risk of loss. Market data, opportunities and automated execution from ArbPilot are not investment advice and do not promise risk-free arbitrage or profits. Assess your own situation and control position size.",
    copyright: "© 2026 ArbPilot · Self-hosted · Funds and keys always stay in your hands",
  },
  notFound: {
    eyebrow: "404",
    title: "This page does not exist",
    body: "The link may be out of date, or the address was mistyped. Head back to the homepage to see funding rates, news coverage and custom strategies.",
    back: "Back to home",
  },
};

export type Copy = typeof zh;

const dictionaries: Record<Locale, Copy> = { zh, en };

/** 取整套文案；seo.ts 在构建期据此生成页面标题与描述。 */
export function copyFor(locale: Locale): Copy {
  return dictionaries[locale];
}

function detectLocale(): Locale {
  // 构建期预渲染在 Node 中执行，没有 localStorage / navigator。
  if (typeof window === "undefined") {
    return "en";
  }
  const stored = localStorage.getItem(STORAGE_KEY);
  if (stored === "zh" || stored === "en") {
    return stored;
  }
  return "en";
}

export function getLocale(): Locale {
  return detectLocale();
}

/** 将 {name} 占位符替换为变量值。 */
export function fmt(template: string, vars?: Record<string, string | number>): string {
  if (!vars) {
    return template;
  }
  return Object.entries(vars).reduce(
    (text, [key, value]) => text.replaceAll(`{${key}}`, String(value)),
    template,
  );
}

type I18nValue = {
  locale: Locale;
  setLocale: (locale: Locale) => void;
  toggleLocale: () => void;
  t: Copy;
};

const I18nContext = createContext<I18nValue | null>(null);

export function I18nProvider({
  children,
  initialLocale,
}: {
  children: ReactNode;
  /** 预渲染时固定语言，浏览器端缺省按本地存储与浏览器语言判断。 */
  initialLocale?: Locale;
}) {
  const [locale, setLocaleState] = useState<Locale>(() => initialLocale ?? detectLocale());

  useEffect(() => {
    document.documentElement.lang = locale === "zh" ? "zh-CN" : "en";
  }, [locale]);

  const setLocale = useCallback((next: Locale) => {
    localStorage.setItem(STORAGE_KEY, next);
    setLocaleState(next);
  }, []);

  const toggleLocale = useCallback(() => {
    setLocaleState((current) => {
      const next = current === "zh" ? "en" : "zh";
      localStorage.setItem(STORAGE_KEY, next);
      return next;
    });
  }, []);

  const value = useMemo<I18nValue>(
    () => ({ locale, setLocale, toggleLocale, t: dictionaries[locale] }),
    [locale, setLocale, toggleLocale],
  );

  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>;
}

export function useI18n(): I18nValue {
  const value = useContext(I18nContext);
  if (!value) {
    throw new Error("useI18n must be used inside I18nProvider");
  }
  return value;
}

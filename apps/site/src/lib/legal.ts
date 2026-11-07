import type { Locale } from "./i18n";

export interface LegalTable {
  columns: string[];
  rows: string[][];
}

export interface LegalSection {
  heading: string;
  paragraphs?: string[];
  bullets?: string[];
  table?: LegalTable;
}

export interface LegalDoc {
  slug: "privacy" | "terms";
  title: string;
  updated: string;
  version: string;
  intro: string[];
  sections: LegalSection[];
  footnote: string;
}

const UPDATED = "2026-11-07";

const privacyZh: LegalDoc = {
  slug: "privacy",
  title: "隐私政策",
  updated: UPDATED,
  version: "1.0",
  intro: [
    "arbpilot.trade（由个人经营者运营，以下简称“我们”）尊重并保护您的隐私权。本隐私政策（“本政策”）说明我们在您购买订阅、访问介绍站或使用 ArbPilot 桌面端与执行器（合称“本服务”）时，如何收集、使用、存储和共享您的个人信息，以及您依法享有的权利。",
    "请在使用本服务前仔细阅读本政策。完成购买或继续使用本服务，即表示您已阅读并同意本政策。我们可能不时更新本政策，如发生重大变更将通过邮件或站内公告提前告知。",
  ],
  sections: [
    {
      heading: "1. 数据控制者",
      paragraphs: ["本服务的数据控制者为："],
      bullets: [
        "运营主体：arbpilot.trade（个人经营者）",
        "注册地址：中华人民共和国浙江省杭州市",
        "隐私联系邮箱：support@arbpilot.trade",
        "数据保护负责人（DPO）：暂未设立",
      ],
    },
    {
      heading: "2. 我们收集的个人信息",
      paragraphs: [
        "2.1 您主动提供的信息",
        "购买与订阅：邮箱地址、订单号、订阅方案、支付金额与币种、税费、支付状态、账单国家/地区。我们不接收也不存储完整卡号——卡数据由支付处理方处理（见第 5 条）。",
        "沟通记录：您发给客服的邮件、工单与反馈内容。",
        "2.2 我们自动收集的信息",
        "设备与网络：IP 地址、设备型号、操作系统、桌面端版本、浏览器类型与时区。",
        "使用与诊断：页面访问、下载、授权校验请求、错误日志与性能数据。",
        "2.3 我们不会收集的信息",
        "ArbPilot 采用自托管架构：交易所 API Key、API Secret、钱包私钥、资金、持仓与策略参数始终保存在您的设备或您自己的服务器上，不会上传到我们的服务器。",
      ],
    },
    {
      heading: "3. 我们如何使用您的个人信息",
      table: {
        columns: ["使用目的", "法律依据"],
        rows: [
          ["提供软件许可、授权校验与更新", "履行合同"],
          ["计费、开票与支付处理", "履行合同"],
          ["客户支持与售后沟通", "履行合同 / 合法利益"],
          ["服务通知（账单、安全、政策变更）", "合法利益"],
          ["安全防护、欺诈与滥用防控", "合法利益"],
          ["产品优化与匿名统计", "合法利益"],
          ["法律与税务合规", "法律义务"],
          ["产品更新与优惠信息（可选）", "您的同意"],
        ],
      },
      paragraphs: ["我们可能对数据进行汇总或去标识化处理用于统计分析，此类数据无法识别到具体个人。"],
    },
    {
      heading: "4. Cookie 与本地存储",
      paragraphs: [
        "介绍站使用浏览器本地存储保存语言与主题偏好；托管收银台（由 Waffo Pancake 提供）会使用维持支付流程所必需的 Cookie。",
      ],
      table: {
        columns: ["类型", "用途", "可否关闭"],
        rows: [
          ["严格必要型", "维持收银台会话、完成支付与防欺诈", "否"],
          ["功能型", "记住语言与浅色/深色偏好", "是"],
        ],
      },
    },
    {
      heading: "5. 个人信息的共享与披露",
      paragraphs: [
        "我们不出售您的个人信息，包括适用法律（如 CCPA）所定义的“出售”行为。我们仅在以下情形共享信息：",
      ],
      bullets: [
        "支付与商户记录：本服务的支付由符合 PCI-DSS 标准的支付处理方 Waffo Pancake 作为记录商户（Merchant of Record）独家处理，卡数据不存储于我们的服务器上。",
        "服务提供商：云计算、邮件与客服工具等供应商，受保密协议约束，仅按我们的指示处理数据。",
        "法律与监管要求：依法律、法院命令或主管机关的合法请求。",
        "商业交易：在合并、收购或资产转让情形下，提前通知并确保保护义务延续。",
        "经您同意：在获得您明确事先同意的前提下用于其他目的。",
      ],
    },
    {
      heading: "6. 数据安全措施",
      bullets: [
        "传输加密：介绍站、收银台与授权接口全部使用 TLS / HTTPS。",
        "访问控制：最小权限原则，生产环境凭据与个人账号分离。",
        "日志最小化：日志仅保留排障所需字段，不记录支付凭据。",
        "定期安全审计与依赖漏洞扫描。",
      ],
      paragraphs: [
        "如发生影响您权益的安全事件，我们将在发现后 72 小时内依法通知您及相关监管机构。请妥善保管您的授权邮箱与设备凭据，不要与他人共享。",
      ],
    },
    {
      heading: "7. 个人信息保留期限",
      table: {
        columns: ["数据类型", "保留期限", "到期处理"],
        rows: [
          ["订阅与订单信息", "订阅有效期内，结束后 3 年", "删除或匿名化"],
          ["交易与税务记录", "依适用法规，通常 5 年", "归档后删除"],
          ["客服记录", "2 年", "安全删除"],
          ["安全与访问日志", "12 个月", "安全删除"],
        ],
      },
    },
    {
      heading: "8. 您的数据权利",
      paragraphs: [
        "如需行使以下权利，请发送邮件至 support@arbpilot.trade，我们将在 30 个日历日内处理。",
      ],
      table: {
        columns: ["权利", "说明"],
        rows: [
          ["知情权", "了解我们收集及使用哪些数据"],
          ["访问权", "获取您的个人信息副本"],
          ["更正权", "纠正不准确或不完整的信息"],
          ["删除权", "在特定条件下请求删除数据"],
          ["限制处理权", "在特定情形下暂停数据处理"],
          ["数据可携带权", "以机器可读格式获取您的数据"],
          ["反对权", "反对基于合法利益或营销的处理"],
          ["撤回同意权", "撤回基于同意的处理授权"],
        ],
      },
    },
    {
      heading: "9. 营销通信与退订",
      paragraphs: [
        "经您同意，我们可能发送产品更新与优惠信息。您可随时点击邮件中的“取消订阅”链接或联系我们退订。退订不影响账单、安全提醒与政策变更等服务必要通知。",
      ],
    },
    {
      heading: "10. 跨境数据传输",
      paragraphs: [
        "我们的服务器及合作方可能位于新加坡、美国或其他地区。跨境传输时，我们通过纳入欧盟标准合同条款（SCC）的数据处理协议、并仅向具备同等保护水平的接收方传输来保障数据安全。",
      ],
    },
    {
      heading: "11. 未成年人保护",
      paragraphs: [
        "本服务面向年满 18 周岁的用户，不向未成年人提供服务。我们不会故意收集未成年人的个人信息。如您认为未成年人向我们提供了信息，请立即联系我们删除。",
      ],
    },
    {
      heading: "12. 第三方链接与服务",
      paragraphs: [
        "介绍站可能包含指向交易所或其他第三方站点的链接，本服务也依赖第三方交易所的公开接口。本政策仅适用于我们直接收集的信息，第三方的数据实践请查阅其各自的隐私政策。",
      ],
    },
    {
      heading: "13. 本政策的变更",
      paragraphs: [
        "如发生重大变更，我们将至少提前 15 天通过邮件或站内公告通知您，并更新本页顶部的“最后更新”日期。生效日后继续使用即视为接受修订后的政策。",
      ],
    },
    {
      heading: "14. 联系我们",
      bullets: [
        "客户支持、隐私与法务、退款申请：support@arbpilot.trade",
        "运营主体：arbpilot.trade（个人经营者）",
        "通讯地址：中华人民共和国浙江省杭州市",
      ],
    },
  ],
  footnote:
    "本政策最后更新于 2026-11-07。如对本政策有任何疑问，请联系 support@arbpilot.trade。",
};

const privacyEn: LegalDoc = {
  slug: "privacy",
  title: "Privacy Policy",
  updated: UPDATED,
  version: "1.0",
  intro: [
    'arbpilot.trade, operated by an individual sole proprietor ("we", "us", "our"), respects your privacy. This Privacy Policy explains how we collect, use, store and share personal information when you purchase a subscription, visit this website, or use the ArbPilot desktop app and executor (together, the "Service"), and the rights you have under applicable law.',
    "Please read this Policy before using the Service. Completing a purchase or continuing to use the Service means you have read and agree to this Policy. We may update it from time to time and will notify you in advance of material changes by email or in-product notice.",
  ],
  sections: [
    {
      heading: "1. Data controller",
      paragraphs: ["The data controller for the Service is:"],
      bullets: [
        "Legal name: arbpilot.trade (sole proprietor)",
        "Registered address: Hangzhou, Zhejiang, People's Republic of China",
        "Privacy contact: support@arbpilot.trade",
        "Data Protection Officer: not appointed",
      ],
    },
    {
      heading: "2. Information we collect",
      paragraphs: [
        "2.1 Information you provide",
        "Purchases and subscriptions: email address, order ID, plan, amount and currency, taxes, payment status and billing country. We never receive or store full card numbers — card data is handled by our payment processor (see Section 5).",
        "Communications: emails, tickets and feedback you send to support.",
        "2.2 Information collected automatically",
        "Device and network: IP address, device model, operating system, desktop app version, browser type and time zone.",
        "Usage and diagnostics: page visits, downloads, license validation requests, error logs and performance data.",
        "2.3 Information we do not collect",
        "ArbPilot is self-hosted: exchange API keys, API secrets, wallet private keys, funds, positions and strategy parameters stay on your device or your own server and are never uploaded to us.",
      ],
    },
    {
      heading: "3. How we use your information",
      table: {
        columns: ["Purpose", "Legal basis"],
        rows: [
          ["Delivering the software license, validation and updates", "Performance of a contract"],
          ["Billing, invoicing and payment processing", "Performance of a contract"],
          ["Customer support", "Contract / legitimate interests"],
          ["Service notices (billing, security, policy changes)", "Legitimate interests"],
          ["Security, fraud and abuse prevention", "Legitimate interests"],
          ["Product improvement and aggregate analytics", "Legitimate interests"],
          ["Legal and tax compliance", "Legal obligation"],
          ["Product updates and offers (optional)", "Your consent"],
        ],
      },
      paragraphs: [
        "We may aggregate or de-identify data for statistical analysis; such data cannot be linked back to an individual.",
      ],
    },
    {
      heading: "4. Cookies and local storage",
      paragraphs: [
        "This website stores your language and theme preferences in browser local storage. The hosted checkout, operated by Waffo Pancake, uses cookies that are strictly necessary to complete payment.",
      ],
      table: {
        columns: ["Type", "Purpose", "Can be disabled"],
        rows: [
          ["Strictly necessary", "Checkout session, payment completion, fraud prevention", "No"],
          ["Functional", "Remember language and light/dark preference", "Yes"],
        ],
      },
    },
    {
      heading: "5. Sharing and disclosure",
      paragraphs: [
        'We do not sell your personal information, including as "sale" is defined under applicable law (e.g. CCPA). We share information only in these cases:',
      ],
      bullets: [
        "Payments and merchant records: purchases are processed exclusively by Waffo Pancake, a PCI-DSS compliant Merchant of Record and payment processor. Card data never touches our servers.",
        "Service providers: cloud hosting, email and support tooling bound by confidentiality obligations and acting on our instructions.",
        "Legal and regulatory requirements: where required by law, court order or a lawful request from a competent authority.",
        "Business transactions: in a merger, acquisition or asset transfer, with prior notice and continued protection obligations.",
        "With your consent: for other purposes after obtaining your explicit prior consent.",
      ],
    },
    {
      heading: "6. Security measures",
      bullets: [
        "Encryption in transit: the website, checkout and licensing endpoints all use TLS / HTTPS.",
        "Access control: least privilege, with production credentials separated from personal accounts.",
        "Log minimisation: logs keep only the fields needed to debug and never record payment credentials.",
        "Regular security reviews and dependency vulnerability scanning.",
      ],
      paragraphs: [
        "If a security incident affects your rights, we will notify you and the relevant regulators without undue delay and within 72 hours of discovery where required by law. Keep your license email and device credentials private.",
      ],
    },
    {
      heading: "7. Retention",
      table: {
        columns: ["Data type", "Retention period", "Disposal"],
        rows: [
          ["Subscription and order records", "Subscription term, then 3 years", "Deleted or anonymised"],
          ["Transaction and tax records", "As required by law, usually 5 years", "Archived, then deleted"],
          ["Support records", "2 years", "Securely deleted"],
          ["Security and access logs", "12 months", "Securely deleted"],
        ],
      },
    },
    {
      heading: "8. Your rights",
      paragraphs: [
        "To exercise any of the rights below, email support@arbpilot.trade. We respond within 30 calendar days.",
      ],
      table: {
        columns: ["Right", "What it means"],
        rows: [
          ["To be informed", "Know which data we collect and how we use it"],
          ["Access", "Obtain a copy of your personal information"],
          ["Rectification", "Correct inaccurate or incomplete data"],
          ["Erasure", "Request deletion under certain conditions"],
          ["Restriction", "Pause processing in specific circumstances"],
          ["Portability", "Receive your data in a machine-readable format"],
          ["Objection", "Object to processing based on legitimate interests or marketing"],
          ["Withdraw consent", "Withdraw consent-based processing at any time"],
        ],
      },
    },
    {
      heading: "9. Marketing and opt-out",
      paragraphs: [
        'With your consent we may send product updates and offers. You can opt out at any time via the "unsubscribe" link in any email or by contacting us. Opting out does not affect service notices such as billing, security alerts or policy changes.',
      ],
    },
    {
      heading: "10. International transfers",
      paragraphs: [
        "Our servers and providers may be located in Singapore, the United States or other regions. For cross-border transfers we rely on data processing agreements incorporating the EU Standard Contractual Clauses and only transfer to recipients with comparable protection.",
      ],
    },
    {
      heading: "11. Children",
      paragraphs: [
        "The Service is intended for users aged 18 and over and is not offered to minors. We do not knowingly collect information from minors. If you believe a minor has provided us with information, contact us and we will delete it.",
      ],
    },
    {
      heading: "12. Third-party links and services",
      paragraphs: [
        "This website links to exchanges and other third-party sites, and the Service relies on public exchange APIs. This Policy covers only information we collect directly; please review the privacy policies of those third parties.",
      ],
    },
    {
      heading: "13. Changes to this Policy",
      paragraphs: [
        'For material changes we will give at least 15 days\' notice by email or in-product announcement and update the "Last updated" date above. Continued use after the effective date constitutes acceptance.',
      ],
    },
    {
      heading: "14. Contact us",
      bullets: [
        "Customer support, privacy, legal and refunds: support@arbpilot.trade",
        "Legal name: arbpilot.trade (sole proprietor)",
        "Postal address: Hangzhou, Zhejiang, People's Republic of China",
      ],
    },
  ],
  footnote:
    "This Policy was last updated on 2026-11-07. If you have any questions, contact support@arbpilot.trade.",
};

const termsZh: LegalDoc = {
  slug: "terms",
  title: "服务条款",
  updated: UPDATED,
  version: "1.0",
  intro: [
    "欢迎使用 ArbPilot（以下简称“我们”）。本服务由个人经营者以 arbpilot.trade 名义运营，经营地为中华人民共和国浙江省杭州市，依据中华人民共和国法律经营。本服务条款（“条款”）规范您对 ArbPilot 桌面端、执行器、介绍站及相关服务（合称“本服务”）的访问与使用。",
    "订阅方案、建立授权或以其他方式使用本服务，即表示您：（a）已年满 18 周岁；（b）已阅读、理解并同意受本条款约束；（c）同意我们的隐私政策；以及（d）有权代表自身或所属机构签署本协议。如不同意本条款，请停止使用本服务。",
  ],
  sections: [
    {
      heading: "第 1 条 服务说明",
      paragraphs: [
        "ArbPilot 是自托管的跨交易所套利终端：桌面端负责配置与监控，Rust 执行器在您自己的设备或服务器上连接交易所、执行风控与订单。本服务为通过互联网交付的数字软件即服务（SaaS）产品，付款确认后即时开通授权。",
        "我们不托管资金、不保存您的交易所 API 密钥、不代客交易，也不对您的交易结果作出任何承诺。ArbPilot 提供的行情、机会与自动化执行不构成投资建议。由于数字服务的即时交付性质，退款适用第 7 条的特定限制。",
      ],
    },
    {
      heading: "第 2 条 授权与账号",
      paragraphs: [
        "2.1 授权创建。购买订阅时需提供准确、完整的邮箱地址。授权与订阅绑定至该邮箱，您有责任保证邮箱持续有效。",
        "2.2 账号安全。请勿与他人共享授权或安装包；如发现未经授权的使用，请立即联系 support@arbpilot.trade。",
        "2.3 授权范围。每份订阅授权供单一自然人或其所属机构在合理数量的自有设备上使用。不得转售、出租或分包本服务。",
      ],
    },
    {
      heading: "第 3 条 订阅方案",
      paragraphs: [
        "我们提供按月与按年自动续费的订阅方案，当前方案功能与价格以定价页（/pricing）展示为准。我们保留调整方案功能或推出新方案的权利；价格变更按第 5.2 条处理。",
      ],
    },
    {
      heading: "第 4 条 计费与付款",
      paragraphs: [
        "4.1 自动续费授权。您在提供支付方式并订阅付费方案时，即明确授权我们通过 Waffo Pancake 按周期向您的支付方式扣收对应订阅费用。该授权持续有效，直至您依第 6 条取消订阅。除您取消外，订阅在每个计费周期结束时自动续费；年付订阅续费前我们会提前发送提醒。",
        "4.2 价格与税费。除非另有说明，所列价格不含适用税费（增值税、GST、销售税等）。如适用法律要求，我们将代为收取并代缴。",
        "4.3 支付处理方。所有支付由 Waffo Pancake 作为记录商户（Merchant of Record）与支付处理方完成，您的卡数据由其独家处理，不存储于我们的服务器上。",
      ],
    },
    {
      heading: "第 5 条 取消政策",
      paragraphs: [
        "5.1 如何取消。您可随时通过以下任一方式取消订阅，取消后不再自动续费：",
      ],
      bullets: [
        "在线（推荐）：通过您购买时收到的订阅管理链接或收银台中的订阅入口取消",
        "邮件：从授权邮箱发送取消申请至 support@arbpilot.trade",
      ],
    },
    {
      heading: "6. 取消后的效果",
      bullets: [
        "订阅在当前已付费计费周期到期前保持有效。",
        "后续计费周期不再扣款。",
        "订阅到期后，桌面端查看仓位、撤单与风险处置等本地能力不受影响，仅在线授权校验与更新服务停止。",
        "账户相关数据按隐私政策第 7 条保留与删除。",
      ],
    },
    {
      heading: "第 7 条 退款政策",
      paragraphs: [
        "7.1 总体政策。由于数字服务具有即时交付的无形性质，订阅费用原则上不予退款，以下情形除外。",
        "7.2 可退款情形",
      ],
      bullets: [
        "新订户 7 日保障：首次订阅用户可在首次扣款后 7 天内申请全额退款，前提是该订阅周期内未发生实质性使用。",
        "重复扣款：因计费错误导致重复扣款的，全额退款。",
        "已验证的服务中断：因我方原因导致授权服务连续中断超过 72 小时的，按比例退款或展期。",
        "法定权利：如适用法律赋予撤销权或退款权（如欧盟/英国十四天冷静期），这些权利予以保留。",
      ],
    },
    {
      heading: "7.3 如何申请退款与不予退款项",
      paragraphs: [
        "退款申请请发送至 support@arbpilot.trade，并提供授权邮箱、订单号与退款原因。我们将在 2 个工作日内确认，并在 5–10 个工作日内原路退款。",
        "以下情形不予退款：已使用的订阅周期费用（第 7.2 条除外）；年付订阅首次购买 30 天后的退款申请；因违反本条款或可接受使用政策而被关闭的授权；因交易亏损、行情波动或第三方交易所故障造成的损失。",
      ],
    },
    {
      heading: "第 8 条 账单争议",
      paragraphs: [
        "如您认为扣款存在错误，请先发送邮件至 support@arbpilot.trade，在向银行或卡组织提起争议前与我们联系。我们承诺在 2 个工作日内回复，并在 5 个工作日内解决确认的计费错误。",
      ],
    },
    {
      heading: "第 9 条 知识产权与许可",
      paragraphs: [
        "9.1 许可。在订阅有效期内，我们授予您有限、非独家、不可转让、可撤销的许可，允许您在自有设备上安装并使用 ArbPilot 桌面端与执行器。",
        "9.2 限制。您不得逆向工程、反编译、拆解本软件的核心组件，不得移除版权标识，不得再分发、转售或提供托管版本给第三方。开源组件依其各自许可证使用。",
        "9.3 反馈。您提供的建议与反馈可用于改进产品，我们不会因此向您支付额外对价。",
      ],
    },
    {
      heading: "第 10 条 可接受使用",
      paragraphs: ["您同意不将本服务用于："],
      bullets: [
        "违反任何适用法律、法规、交易所规则或制裁要求的行为。",
        "市场操纵、虚假申报、幌骗或其他被禁止的交易行为。",
        "绕过、干扰或试图削弱本服务的安全机制与风控。",
        "未经事先书面批准转售、出租或分包访问权。",
        "侵犯任何第三方的知识产权或隐私权。",
        "在未获得许可的司法管辖区向受限主体提供服务。",
      ],
    },
    {
      heading: "第 11 条 数据、隐私与安全",
      paragraphs: [
        "您对本服务的使用受我们隐私政策（/privacy）约束。支付卡数据由符合 PCI-DSS 标准的支付处理方（Waffo Pancake）独家处理，不存储于我们的服务器上。取消订阅后，账户相关数据保留 90 天，随后依据隐私政策删除。",
      ],
    },
    {
      heading: "第 12 条 免责声明与责任限制",
      paragraphs: [
        "12.1 本服务以“现状”和“如可用”的方式提供，不作任何明示或隐含的保证，包括适销性、特定用途适用性或不间断运行。",
        "12.2 交易风险。数字资产与衍生品交易存在重大亏损风险。我们不保证任何策略、价差或自动化执行能够盈利，也不对因滑点、双腿失衡、交易所故障、网络中断或参数配置导致的损失承担责任。",
        "12.3 责任限制。在适用法律允许的最大范围内，我们不对任何间接、附带、后果性或惩罚性损害承担责任；我们的总责任以索赔事件发生前 12 个月内您向我们支付的订阅费用为限。",
      ],
    },
    {
      heading: "第 13 条 期限与终止",
      paragraphs: [
        "本条款在您使用本服务期间持续有效。如您严重违反本条款、涉嫌欺诈或法律要求，我们可暂停或终止您的授权。如我方因非您违约的原因终止服务，将按比例退还未使用部分。您可随时通过 support@arbpilot.trade 申请注销授权。",
      ],
    },
    {
      heading: "第 14 条 适用法律与争议解决",
      paragraphs: [
        "本条款受中华人民共和国法律管辖，不适用其冲突法规则。提起正式程序前，请先发送邮件至 support@arbpilot.trade 尝试友好解决；协商不成的，提交杭州市有管辖权的人民法院处理。",
      ],
    },
    {
      heading: "第 15 条 通用条款",
      paragraphs: [
        "我们可随时更新本条款，重大变更将在生效日前至少 14 天通过邮件通知。继续使用即表示接受修订后的条款。如任何条款被认定无效，其余条款继续完全有效。本条款与隐私政策共同构成您与我们之间的完整协议。",
      ],
    },
    {
      heading: "第 16 条 联系方式",
      bullets: [
        "客户支持、账单查询、退款申请、取消订阅、法务与隐私、安全问题：support@arbpilot.trade",
        "运营主体：arbpilot.trade（个人经营者）",
        "通讯地址：中华人民共和国浙江省杭州市",
      ],
    },
  ],
  footnote:
    "本条款最后更新于 2026-11-07。如对本条款有任何疑问，请联系 support@arbpilot.trade。",
};

const termsEn: LegalDoc = {
  slug: "terms",
  title: "Terms of Service",
  updated: UPDATED,
  version: "1.0",
  intro: [
    'Welcome to ArbPilot ("ArbPilot", "we", "us", "our"). These Terms of Service ("Terms") govern your access to and use of the ArbPilot desktop app, executor, website and related services (together, the "Service"). The Service is operated by an individual sole proprietor under the name arbpilot.trade, based in Hangzhou, Zhejiang, People\'s Republic of China.',
    "By subscribing to a plan, activating a license or otherwise using the Service, you confirm that you: (a) are at least 18 years of age; (b) have read, understood and agree to be bound by these Terms; (c) agree to our Privacy Policy; and (d) are authorised to enter into this agreement on behalf of yourself or any organisation you represent. If you do not agree, do not use the Service.",
  ],
  sections: [
    {
      heading: "1. The Service",
      paragraphs: [
        "ArbPilot is a self-hosted cross-venue arbitrage terminal: the desktop app handles configuration and monitoring, while a Rust executor running on your own machine or server connects to exchanges, applies risk checks and places orders. The Service is a digital software-as-a-service product delivered over the internet; access is granted immediately after payment is confirmed.",
        "We do not custody funds, store your exchange API keys, trade on your behalf, or promise any trading outcome. Market data, opportunities and automated execution from ArbPilot are not investment advice. Because the Service is delivered instantly in digital form, refunds are subject to the limits in Section 7.",
      ],
    },
    {
      heading: "2. License and account",
      paragraphs: [
        "2.1 Creating a license. A valid and accurate email address is required at purchase. The license is bound to that address and you are responsible for keeping it active.",
        "2.2 Account security. Do not share your license or installers with third parties. Report unauthorised use immediately to support@arbpilot.trade.",
        "2.3 Scope. Each subscription licenses a single individual, or the organisation they represent, across a reasonable number of devices they own. Resale, rental or sublicensing is not permitted.",
      ],
    },
    {
      heading: "3. Subscription plans",
      paragraphs: [
        "We offer monthly and yearly auto-renewing subscription plans. Current features and prices are shown on the pricing page (/pricing). We may change plan features or introduce new plans; price changes follow Section 4.2.",
      ],
    },
    {
      heading: "4. Billing and payment",
      paragraphs: [
        "4.1 Auto-renewal authorisation. When you provide a payment method and subscribe to a paid plan, you expressly authorise us, through Waffo Pancake, to charge that payment method on a recurring basis for the applicable subscription fee. This authorisation remains in effect until you cancel under Section 5. Unless you cancel, your subscription renews automatically at the end of each billing period; we send a reminder before yearly renewals.",
        "4.2 Prices and taxes. Unless stated otherwise, prices exclude applicable taxes (VAT, GST, sales tax and similar). Where required by law, we collect and remit such taxes.",
        "4.3 Payment processor. All payments are handled by Waffo Pancake as Merchant of Record and payment processor. Card data is processed exclusively by them and is never stored on our servers.",
      ],
    },
    {
      heading: "5. Cancellation",
      paragraphs: ["5.1 How to cancel. You may cancel at any time; auto-renewal then stops:"],
      bullets: [
        "Online (recommended): use the subscription management link in your purchase receipt or the subscription entry in the checkout portal",
        "Email: send a cancellation request from your license email to support@arbpilot.trade",
      ],
    },
    {
      heading: "6. Effect of cancellation",
      bullets: [
        "Your subscription stays active until the end of the current paid billing period.",
        "No further charges are made in subsequent billing periods.",
        "After expiry, local capabilities such as viewing positions, cancelling orders and risk handling in the desktop app keep working; only online license validation and updates stop.",
        "Account data is retained and deleted according to Section 7 of the Privacy Policy.",
      ],
    },
    {
      heading: "7. Refund policy",
      paragraphs: [
        "7.1 General. Because the Service is an instantly delivered digital product, subscription fees are generally non-refundable, except in the following cases.",
        "7.2 Refundable cases",
      ],
      bullets: [
        "New subscriber 7-day guarantee: first-time subscribers may request a full refund within 7 days of the first charge, provided the plan was not materially used during that period.",
        "Duplicate charges: full refund where a billing error caused a duplicate charge.",
        "Verified outage: if license services are unavailable for more than 72 consecutive hours due to our fault, we issue a pro-rata refund or extension.",
        "Statutory rights: any withdrawal or refund rights granted by applicable law (e.g. the EU/UK 14-day cooling-off period) are preserved.",
      ],
    },
    {
      heading: "7.3 How to request a refund, and exclusions",
      paragraphs: [
        "Email support@arbpilot.trade with your license email, order ID and reason. We confirm within 2 business days and refund to the original payment method within 5–10 business days.",
        "We do not refund: fees for subscription periods already used (other than under Section 7.2); yearly plans after 30 days from the initial purchase; licenses terminated for breach of these Terms or the acceptable use policy; losses caused by trading, market movement or third-party exchange failures.",
      ],
    },
    {
      heading: "8. Billing disputes",
      paragraphs: [
        "If you believe a charge is incorrect, email support@arbpilot.trade before raising a dispute with your bank or card network. We respond within 2 business days and resolve confirmed billing errors within 5 business days.",
      ],
    },
    {
      heading: "9. Intellectual property and license",
      paragraphs: [
        "9.1 License grant. During your subscription term we grant you a limited, non-exclusive, non-transferable, revocable license to install and use the ArbPilot desktop app and executor on devices you own.",
        "9.2 Restrictions. You may not reverse engineer, decompile or disassemble core components, remove copyright notices, or redistribute, resell or host the Service for third parties. Open-source components remain governed by their own licenses.",
        "9.3 Feedback. Suggestions and feedback you provide may be used to improve the product without additional compensation.",
      ],
    },
    {
      heading: "10. Acceptable use",
      paragraphs: ["You agree not to use the Service to:"],
      bullets: [
        "Violate any applicable law, regulation, exchange rule or sanctions requirement.",
        "Engage in market manipulation, spoofing, layering or other prohibited trading conduct.",
        "Bypass, interfere with or attempt to weaken security controls or risk checks.",
        "Resell, rent or sublicense access without prior written approval.",
        "Infringe any third party's intellectual property or privacy rights.",
        "Provide the Service to restricted persons or in restricted jurisdictions.",
      ],
    },
    {
      heading: "11. Data, privacy and security",
      paragraphs: [
        "Your use of the Service is governed by our Privacy Policy (/privacy). Card data is processed exclusively by our PCI-DSS compliant payment processor (Waffo Pancake) and is not stored on our servers. After cancellation, account data is retained for 90 days and then deleted in line with the Privacy Policy.",
      ],
    },
    {
      heading: "12. Disclaimers and limitation of liability",
      paragraphs: [
        '12.1 The Service is provided "as is" and "as available", without warranties of any kind, express or implied, including merchantability, fitness for a particular purpose or uninterrupted operation.',
        "12.2 Trading risk. Digital asset and derivatives trading carries a substantial risk of loss. We do not guarantee that any strategy, spread or automated execution will be profitable, and we are not liable for losses caused by slippage, unbalanced legs, exchange outages, network interruptions or misconfiguration.",
        "12.3 Limitation of liability. To the maximum extent permitted by law, we are not liable for indirect, incidental, consequential or punitive damages. Our aggregate liability is limited to the subscription fees you paid us in the 12 months preceding the claim.",
      ],
    },
    {
      heading: "13. Term and termination",
      paragraphs: [
        "These Terms remain in effect while you use the Service. We may suspend or terminate your license for material breach, suspected fraud or where required by law. If we terminate for reasons other than your breach, we refund the unused pro-rata portion. You may close your license at any time by contacting support@arbpilot.trade.",
      ],
    },
    {
      heading: "14. Governing law and disputes",
      paragraphs: [
        "These Terms are governed by the laws of the People's Republic of China, excluding conflict-of-law rules. Before starting formal proceedings, email support@arbpilot.trade to seek an amicable resolution; unresolved disputes are submitted to the competent People's Court in Hangzhou, Zhejiang.",
      ],
    },
    {
      heading: "15. General",
      paragraphs: [
        "We may update these Terms at any time; material changes are notified by email at least 14 days before they take effect. Continued use constitutes acceptance. If any provision is held invalid, the remaining provisions stay in full force. These Terms together with the Privacy Policy form the entire agreement between you and us.",
      ],
    },
    {
      heading: "16. Contact",
      bullets: [
        "Customer support, billing, refunds, cancellations, legal, privacy and security: support@arbpilot.trade",
        "Legal name: arbpilot.trade (sole proprietor)",
        "Postal address: Hangzhou, Zhejiang, People's Republic of China",
      ],
    },
  ],
  footnote:
    "These Terms were last updated on 2026-11-07. If you have any questions, contact support@arbpilot.trade.",
};

export const legalDocs: Record<Locale, Record<LegalDoc["slug"], LegalDoc>> = {
  zh: { privacy: privacyZh, terms: termsZh },
  en: { privacy: privacyEn, terms: termsEn },
};

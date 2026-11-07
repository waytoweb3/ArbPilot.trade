# @arbpilot/payments

ArbPilot 的独立收款服务，封装 Waffo Pancake（Merchant of Record）。私钥只存在于本服务，
网站（`apps/site`）与桌面端（`apps/desktop`）只通过 REST 调用，不接触任何密钥。

## 启动

```bash
cp apps/payments/.env.example apps/payments/.env   # 填入真实值
pnpm --filter @arbpilot/payments dev               # http://127.0.0.1:8788
```

默认 `WAFFO_ENVIRONMENT=prod`（未显式设为 `test` 时即正式收款）。联调阶段改成 `test`
即可显示「测试模式 - 不会产生真实扣款」，用的也是测试环境商品。

若本机走企业代理或自签根证书，Node 的 `fetch` 可能报 `fetch failed`（`UNABLE_TO_GET_ISSUER_CERT_LOCALLY`），
启动时补一份根证书即可：`NODE_EXTRA_CA_CERTS=/etc/ssl/cert.pem pnpm --filter @arbpilot/payments dev`。

## 配置私钥（三选一）

| 变量 | 说明 |
| --- | --- |
| `WAFFO_PRIVATE_KEY` | 整段 PEM，换行可写成 `\n` |
| `WAFFO_PRIVATE_KEY_BASE64` | PEM 的 base64，便于放进 CI Secret |
| `WAFFO_PRIVATE_KEY_FILE` | 指向 `.pem` 文件的路径 |

未配置私钥时进程仍可启动：`/healthz` 返回 `configured: false`，`/api/plans` 正常，
`/api/checkout` 返回 `503 payments_not_configured`（前端会展示对应提示）。

## 接口

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/healthz` | 健康检查，含 `configured` / `environment` / `storeId` |
| GET | `/api/plans` | 套餐与货币（价格来自 `WAFFO_PRICE_*`） |
| POST | `/api/checkout` | `{ plan, email?, locale, darkMode }` → `{ ref, checkoutUrl, expiresAt }` |
| GET | `/api/licenses/:ref` | 授权状态；未知 ref 返回 404 |
| POST | `/api/webhooks/pancake` | Pancake webhook（校验 `X-Waffo-Signature`，按 `event.id` 幂等） |

数据落在 `apps/payments/.data/`：`licenses.json` 存授权与已处理事件，
`products.json` 缓存自动创建的 Pancake 商品 ID。

## Webhook

1. `PUBLIC_BASE_URL` 必须是公网可达的 https 地址，Pancake 才会推送事件。
2. 在 Pancake Dashboard 把回调地址设为 `${PUBLIC_BASE_URL}/api/webhooks/pancake`。
3. 订阅 `subscription.*` 事件（创建、续费、取消、支付失败等），否则授权状态不会更新。
4. 本地调试可先 `ngrok http 8788`，再把 ngrok 的 https 地址填进 `PUBLIC_BASE_URL`。

## 商品

首次创建收银台时会按 `WAFFO_STORE_ID` 自动创建 Arbitrage & News / Strategy 商品并 `publish()`，ID 写入
`products.json` 复用；也可用 `WAFFO_PRODUCT_ARBITRAGE` / `WAFFO_PRODUCT_STRATEGY` 指定已有商品。

## 前端接入

网站与桌面端通过 `VITE_PAYMENTS_URL` 指向本服务（默认 `http://127.0.0.1:8788`）。
`ALLOWED_ORIGINS` 为额外放行的来源白名单，逗号分隔。

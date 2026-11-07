import { Agent, request } from "node:http";
import type { ClientRequestArgs } from "node:http";
import { connect as netConnect } from "node:net";
import type { Duplex } from "node:stream";
import { connect as tlsConnect, rootCertificates } from "node:tls";

/** 与 fetch 对齐的最小响应接口，避免为代理再引入依赖。 */
export interface OutboundResponse {
  ok: boolean;
  status: number;
  text(): Promise<string>;
  json<T>(): Promise<T | null>;
}

const TIMEOUT_MS = 15_000;

/** 代理只用于 OAuth 服务（Google），本机服务仍走直连。 */
const proxyUrl = [
  process.env.ARBPILOT_OAUTH_PROXY,
  process.env.HTTPS_PROXY,
  process.env.https_proxy,
  process.env.ALL_PROXY,
  process.env.all_proxy,
].find((value) => value && /^http:\/\//i.test(value.trim()));

/**
 * 通过 HTTP CONNECT 隧道访问 HTTPS。
 *
 * 返回给 http.Agent 的必须是已建立 TLS 的 socket，否则 Node 会绕过隧道直连。
 */
class ProxyAgent extends Agent {
  constructor(private readonly proxy: URL) {
    super({ keepAlive: false });
  }

  override createConnection(
    options: ClientRequestArgs,
    callback?: (error: Error | null, stream: Duplex) => void,
  ): undefined {
    let settled = false;
    const done = (error: Error | null, stream?: Duplex) => {
      if (settled) {
        return;
      }
      settled = true;
      callback?.(error, stream as Duplex);
    };

    const host = String(options.host ?? options.hostname ?? "");
    const port = Number(options.port ?? 443);
    const authority = `${host}:${port}`;
    const socket = netConnect({
      host: this.proxy.hostname,
      port: Number(this.proxy.port || 80),
    });

    const onData = (chunk: Buffer) => {
      handshake += chunk.toString("latin1");
      if (!handshake.includes("\r\n\r\n")) {
        return;
      }
      socket.off("data", onData);
      socket.off("error", onError);

      const status = handshake.slice(0, handshake.indexOf("\r\n"));
      if (!/^HTTP\/1\.[01] 200/.test(status)) {
        socket.destroy();
        done(new Error(`代理拒绝建立隧道：${status}`));
        return;
      }

      // 显式传入 Mozilla 根证书，避免个别环境下默认信任库为空。
      const tlsSocket = tlsConnect({ socket, servername: host, ca: [...rootCertificates] });
      tlsSocket.once("secureConnect", () => done(null, tlsSocket));
      tlsSocket.once("error", (error) => done(error));
    };

    let handshake = "";
    const onError = (error: Error) => done(error);
    socket.on("data", onData);
    socket.once("error", onError);
    socket.once("connect", () => {
      const credentials = this.proxy.username
        ? `Proxy-Authorization: Basic ${Buffer.from(
            `${decodeURIComponent(this.proxy.username)}:${decodeURIComponent(this.proxy.password)}`,
          ).toString("base64")}\r\n`
        : "";
      socket.write(
        `CONNECT ${authority} HTTP/1.1\r\nHost: ${authority}\r\n${credentials}Proxy-Connection: keep-alive\r\n\r\n`,
      );
    });
    return undefined;
  }
}

const agent = proxyUrl ? new ProxyAgent(new URL(proxyUrl.trim())) : undefined;

/**
 * 访问第三方 OAuth 接口的统一出口。
 *
 * 直连时沿用全局 fetch；配置了 http 代理时改走 CONNECT 隧道，
 * 保证 Google 在需要代理的网络下也能完成登录。
 */
export function outboundFetch(
  url: string,
  init?: { method?: string; headers?: Record<string, string>; body?: string },
): Promise<OutboundResponse> {
  if (!agent) {
    return fetch(url, init);
  }

  return new Promise((resolve, reject) => {
    const target = new URL(url);
    // 隧道里已经完成 TLS，因此这里按明文 http 构造请求行，实际字节由 tls socket 加密。
    const req = request(
      {
        host: target.hostname,
        port: Number(target.port || 443),
        path: `${target.pathname}${target.search}`,
        method: init?.method ?? "GET",
        headers: { Host: target.host, ...init?.headers },
        agent,
        timeout: TIMEOUT_MS,
      },
      (res) => {
        const chunks: Buffer[] = [];
        res.on("data", (chunk: Buffer) => chunks.push(chunk));
        res.on("end", () => {
          const text = Buffer.concat(chunks).toString("utf8");
          resolve({
            ok: Boolean(res.statusCode && res.statusCode >= 200 && res.statusCode < 300),
            status: res.statusCode ?? 0,
            text: async () => text,
            json: async <T>() => {
              try {
                return JSON.parse(text) as T;
              } catch {
                return null;
              }
            },
          });
        });
        res.on("error", reject);
      },
    );
    req.on("timeout", () => req.destroy(new Error("访问登录服务超时")));
    req.on("error", reject);
    req.end(init?.body);
  });
}

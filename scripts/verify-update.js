#!/usr/bin/env node
// 校验 Tauri 更新清单，避免把缺少签名或地址写错的 latest.json 发出去。
//
// 用法：node scripts/verify-update.js dist/desktop/latest.json
// 退出码非 0 表示清单不可用；CI 里直接让构建失败。
const fs = require("node:fs");

const file = process.argv[2];
if (!file) {
  console.error("用法：node scripts/verify-update.js <latest.json>");
  process.exit(2);
}

let manifest;
try {
  manifest = JSON.parse(fs.readFileSync(file, "utf8"));
} catch (error) {
  console.error(`无法读取更新清单 ${file}：${error.message}`);
  process.exit(1);
}

const errors = [];

if (typeof manifest.version !== "string" || !/^\d+\.\d+\.\d+/.test(manifest.version)) {
  errors.push(`version 不是合法语义化版本：${manifest.version}`);
}
if (manifest.pub_date && Number.isNaN(Date.parse(manifest.pub_date))) {
  errors.push(`pub_date 不是合法时间：${manifest.pub_date}`);
}

const platforms = manifest.platforms;
if (!platforms || typeof platforms !== "object" || Array.isArray(platforms)) {
  errors.push("缺少 platforms 对象");
} else {
  const entries = Object.entries(platforms);
  if (entries.length === 0) {
    errors.push("platforms 为空，客户端不会收到任何更新");
  }
  for (const [name, entry] of entries) {
    if (!entry || typeof entry !== "object") {
      errors.push(`${name}: 条目格式错误`);
      continue;
    }
    if (typeof entry.url !== "string" || !entry.url.startsWith("https://")) {
      errors.push(`${name}: 下载地址必须是 https://，当前为 ${entry.url}`);
    } else if (
      typeof manifest.version === "string" &&
      !entry.url.includes(manifest.version)
    ) {
      errors.push(`${name}: 下载地址与版本号 ${manifest.version} 不一致：${entry.url}`);
    }
    // Tauri v2 的签名文件是 base64 文本（minisign 新格式不再带 untrusted comment 行）。
    if (typeof entry.signature !== "string" || entry.signature.trim().length < 100) {
      errors.push(`${name}: 缺少有效的更新签名`);
    }
  }
}

if (errors.length > 0) {
  console.error(`更新清单校验失败（${file}）：`);
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.error(`更新清单校验通过：版本 ${manifest.version}`);

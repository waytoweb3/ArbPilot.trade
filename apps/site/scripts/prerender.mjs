// 构建后处理：把 dist-ssr/entry-server.js 渲染出的 HTML 写进 dist，
// 并按同一份路由表生成 sitemap.xml。运行前需要先执行客户端与 SSR 两次 vite build。
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const siteDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const distDir = join(siteDir, "dist");
const serverEntry = join(siteDir, "dist-ssr", "entry-server.js");
const rootPlaceholder = '<div id="root"></div>';

const template = await readFile(join(distDir, "index.html"), "utf8");
if (!template.includes(rootPlaceholder)) {
  throw new Error(`dist/index.html 中找不到 ${rootPlaceholder}`);
}

const { renderPages, SITE_URL } = await import(pathToFileURL(serverEntry).href);
const pages = renderPages("zh");

for (const page of pages) {
  const targetDir = page.meta.path === "/" ? distDir : join(distDir, page.meta.path.slice(1));
  await mkdir(targetDir, { recursive: true });

  const html = applyMeta(template, page.meta).replace(
    rootPlaceholder,
    `<div id="root">${page.html}</div>`,
  );
  await writeFile(join(targetDir, "index.html"), html);
  console.log(`[prerender] ${page.meta.path} → ${join(targetDir, "index.html")}`);
}

const today = new Date().toISOString().slice(0, 10);
const entries = pages
  .filter((page) => !page.meta.noindex)
  .map(
    (page) =>
      `  <url>\n    <loc>${SITE_URL}${page.meta.path}</loc>\n    <lastmod>${today}</lastmod>\n  </url>`,
  )
  .join("\n");

await writeFile(
  join(distDir, "sitemap.xml"),
  `<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n${entries}\n</urlset>\n`,
);
console.log(`[prerender] sitemap.xml（${pages.length} 个页面）`);

function applyMeta(html, meta) {
  const replacements = [
    [/<title>[\s\S]*?<\/title>/, `<title>${escapeHtml(meta.title)}</title>`],
    [
      /<meta name="description"[^>]*>/,
      `<meta name="description" content="${escapeHtml(meta.description)}" />`,
    ],
    [
      /<meta name="robots"[^>]*>/,
      `<meta name="robots" content="${meta.noindex ? "noindex,nofollow" : "index,follow"}" />`,
    ],
    [/<link rel="canonical"[^>]*>/, `<link rel="canonical" href="${meta.canonical}" />`],
    [/<meta property="og:title"[^>]*>/, `<meta property="og:title" content="${escapeHtml(meta.title)}" />`],
    [
      /<meta property="og:description"[^>]*>/,
      `<meta property="og:description" content="${escapeHtml(meta.description)}" />`,
    ],
    [/<meta property="og:url"[^>]*>/, `<meta property="og:url" content="${meta.canonical}" />`],
    [
      /<meta name="twitter:title"[^>]*>/,
      `<meta name="twitter:title" content="${escapeHtml(meta.title)}" />`,
    ],
    [
      /<meta name="twitter:description"[^>]*>/,
      `<meta name="twitter:description" content="${escapeHtml(meta.description)}" />`,
    ],
  ];

  return replacements.reduce((result, [pattern, replacement]) => {
    if (!pattern.test(result)) {
      throw new Error(`预渲染失败：index.html 缺少 ${pattern}`);
    }
    return result.replace(pattern, () => replacement);
  }, html);
}

function escapeHtml(value) {
  return value
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

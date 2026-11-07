# 账号服务与支付服务共用的运行时镜像（Node 22 + tsx 直接执行 TS 源码）。
# 构建上下文为仓库根目录，由 GitHub Actions 构建并推送到 GHCR；
# SERVICE 取 apps/ 下的目录名（accounts 或 payments）。
FROM node:22-alpine

ARG SERVICE=accounts

WORKDIR /app
ENV NODE_ENV=production

# 依赖清单单独一层，源码变化时可复用 npm 安装缓存。
COPY apps/${SERVICE}/package.json ./
# 运行时用 tsx 执行 TS 源码，因此 devDependencies 也必须装。
RUN npm install --include=dev --no-audit --no-fund --loglevel=error
COPY apps/${SERVICE}/ ./

# 两个服务都通过 tsx 启动，入口路径一致：src/server.ts
CMD ["node", "node_modules/tsx/dist/cli.mjs", "src/server.ts"]

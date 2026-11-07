# 支付服务与账号服务共用的运行时镜像（Node 22 + tsx 直接执行 TS 源码）。
# 构建上下文是该服务自己的目录（package.json、src、可选 migrations）。
FROM node:22-alpine

WORKDIR /app
ENV NODE_ENV=production

COPY . .
# 运行时用 tsx 执行 TS 源码，因此 devDependencies 也必须装。
RUN npm install --include=dev --no-audit --no-fund --loglevel=error

# 两个服务都通过 tsx 启动，入口路径一致：src/server.ts
CMD ["node", "node_modules/tsx/dist/cli.mjs", "src/server.ts"]

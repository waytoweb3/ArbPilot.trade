/** 站点外部入口与安装包地址，集中维护，替换时无需改动页面组件。 */
export const DISCORD_URL = "https://discord.gg/yHg4xRQnMX";
/** 安装包发布目录，上传制品后即可直接下载。 */
export const DOWNLOAD_BASE = "https://download.arbpilot.trade";
/** 与桌面端 package.json 的版本保持一致。 */
export const APP_VERSION = "0.2.0";
/** 每个版本用独立子目录发布安装包：同名文件重新打包也不会命中 CDN 旧缓存。 */
export const DOWNLOAD_VERSION_PATH = `${DOWNLOAD_BASE}/desktop/v${APP_VERSION}`;

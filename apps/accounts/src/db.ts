import { readdirSync, readFileSync } from "node:fs";
import { resolve } from "node:path";

import { Pool } from "pg";

import { appDir, config } from "./config";

export const pool = new Pool({
  connectionString: config.databaseUrl,
  max: 10,
});

const migrationsDir = resolve(appDir, "migrations");

/** 启动时按文件名顺序执行未应用的迁移，失败立即退出。 */
export async function migrate(): Promise<void> {
  await pool.query(
    `CREATE TABLE IF NOT EXISTS schema_migrations (
       name text PRIMARY KEY,
       applied_at timestamptz NOT NULL DEFAULT now()
     )`,
  );

  const files = readdirSync(migrationsDir)
    .filter((name) => name.endsWith(".sql"))
    .sort();

  for (const file of files) {
    const applied = await pool.query("SELECT 1 FROM schema_migrations WHERE name = $1", [file]);
    if (applied.rowCount) {
      continue;
    }
    const sql = readFileSync(resolve(migrationsDir, file), "utf8");
    const client = await pool.connect();
    try {
      await client.query("BEGIN");
      await client.query(sql);
      await client.query("INSERT INTO schema_migrations (name) VALUES ($1)", [file]);
      await client.query("COMMIT");
      console.log(`[accounts] 已应用迁移 ${file}`);
    } catch (error) {
      await client.query("ROLLBACK");
      throw error;
    } finally {
      client.release();
    }
  }
}

export async function closePool(): Promise<void> {
  await pool.end();
}

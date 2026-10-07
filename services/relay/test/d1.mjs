// A fake D1 on node:sqlite (DatabaseSync, in memory) for driving the worker without the Workers
// runtime: it applies migrations/*.sql in name order and implements prepare(sql).bind(...).first()/
// .all()/.run() plus batch(). Every bound value is recorded in `bound` — a later integration test can
// assert that no request bytes ever reach storage, because this is where they would land.

import { readdirSync, readFileSync } from 'node:fs';
import { DatabaseSync } from 'node:sqlite';
import { fileURLToPath } from 'node:url';

export function fakeD1(migrationsDir = fileURLToPath(new URL('../migrations/', import.meta.url))) {
  const db = new DatabaseSync(':memory:');
  for (const file of readdirSync(migrationsDir).filter((f) => f.endsWith('.sql')).sort()) {
    db.exec(readFileSync(`${migrationsDir}${file}`, 'utf8'));
  }
  const bound = [];
  return {
    bound,
    prepare(sql) {
      const statement = db.prepare(sql);
      const execute = (values) => ({
        first: async () => statement.get(...values) ?? null,
        all: async () => ({ results: statement.all(...values) }),
        run: async () => ({ success: true, meta: { changes: statement.run(...values).changes } }),
      });
      return {
        // like D1, a statement can run without bind() — that is bind() with no values
        ...execute([]),
        bind(...values) {
          bound.push({ sql, values });
          return execute(values);
        },
      };
    },
    async batch(statements) {
      const results = [];
      for (const statement of statements) results.push(await statement.run());
      return results;
    },
  };
}

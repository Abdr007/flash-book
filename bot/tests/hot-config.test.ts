import { describe, expect, test } from 'bun:test';
import { writeFileSync, unlinkSync, mkdtempSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { HotConfigReloader } from '../src/hot-config.ts';

interface Cfg { spreadBps: number; }

describe('HotConfigReloader', () => {
  test('start() returns the initial parsed config', async () => {
    const dir = mkdtempSync(join(tmpdir(), 'fb-hc-'));
    const path = join(dir, 'cfg.json');
    writeFileSync(path, JSON.stringify({ spreadBps: 10 }));
    let updates = 0;
    const r = new HotConfigReloader<Cfg>(path, {
      parse: (raw) => raw as Cfg,
      onUpdate: () => { updates += 1; },
    });
    const initial = await r.start();
    expect(initial.spreadBps).toBe(10);
    expect(updates).toBe(0); // initial doesn't fire onUpdate
    r.stop();
    unlinkSync(path);
  });

  test('reloadNow re-reads the file and fires onUpdate', async () => {
    const dir = mkdtempSync(join(tmpdir(), 'fb-hc-'));
    const path = join(dir, 'cfg.json');
    writeFileSync(path, JSON.stringify({ spreadBps: 10 }));
    let last: Cfg | null = null;
    const r = new HotConfigReloader<Cfg>(path, {
      parse: (raw) => raw as Cfg,
      onUpdate: (c) => { last = c; },
    });
    await r.start();
    writeFileSync(path, JSON.stringify({ spreadBps: 25 }));
    await r.reloadNow();
    expect(last).not.toBeNull();
    expect(last!.spreadBps).toBe(25);
    r.stop();
    unlinkSync(path);
  });

  test('parse error keeps old config + invokes onError', async () => {
    const dir = mkdtempSync(join(tmpdir(), 'fb-hc-'));
    const path = join(dir, 'cfg.json');
    writeFileSync(path, JSON.stringify({ spreadBps: 10 }));
    let lastErr: Error | null = null;
    let updates = 0;
    const r = new HotConfigReloader<Cfg>(path, {
      parse: (raw) => {
        const c = raw as Cfg;
        if (c.spreadBps < 0) throw new Error('spread must be >= 0');
        return c;
      },
      onUpdate: () => { updates += 1; },
      onError: (e) => { lastErr = e; },
    });
    await r.start();
    writeFileSync(path, JSON.stringify({ spreadBps: -5 }));
    await r.reloadNow();
    expect(lastErr).not.toBeNull();
    expect(updates).toBe(0);
    expect(r.getCurrent()!.spreadBps).toBe(10); // old config still live
    r.stop();
    unlinkSync(path);
  });
});

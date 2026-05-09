// Hot config reload — watch a JSON config file and hot-swap bot params
// without restarting the process. Standard ops pattern at every prod
// MM (operators tune spread / inventory cap / kill switch live).
//
// Usage:
//   const reloader = new HotConfigReloader<MyConfig>('./mm.json',
//     (next) => bot.applyConfig(next));
//   reloader.start();
//
// The reloader uses fs.watch (POSIX inotify under bun/node) so it has
// near-zero overhead in steady state. On change, it re-reads the file,
// validates via the supplied parser, and calls onUpdate. Parse errors
// are logged but the previous config remains live — never crashes the
// bot on a bad save.

import { readFile } from 'node:fs/promises';
import { watch, type FSWatcher } from 'node:fs';

export interface HotConfigReloaderOptions<T> {
  /// Parses + validates the raw JSON. Throw to reject invalid configs;
  /// the previous config remains active.
  parse: (raw: unknown) => T;
  /// Called whenever a successful reload happens.
  onUpdate: (next: T) => void;
  /// Called on parse / read error so operators can wire it to logs.
  onError?: (err: Error) => void;
  /// Debounce window — fs.watch fires multiple events for one save on
  /// some platforms. Default: 100ms.
  debounceMs?: number;
}

export class HotConfigReloader<T> {
  private watcher: FSWatcher | null = null;
  private debounceTimer: ReturnType<typeof setTimeout> | null = null;
  private current: T | null = null;

  constructor(
    private readonly path: string,
    private readonly opts: HotConfigReloaderOptions<T>,
  ) {}

  /// Read the initial config + start watching. Returns the parsed
  /// initial value so callers can apply it before the first onUpdate.
  async start(): Promise<T> {
    const initial = await this.loadOnce();
    this.current = initial;

    this.watcher = watch(this.path, () => {
      // Debounce — fs.watch fires multiple events per save on macOS.
      if (this.debounceTimer) clearTimeout(this.debounceTimer);
      this.debounceTimer = setTimeout(() => {
        void this.handleReload();
      }, this.opts.debounceMs ?? 100);
    });
    return initial;
  }

  stop(): void {
    if (this.watcher) {
      this.watcher.close();
      this.watcher = null;
    }
    if (this.debounceTimer) {
      clearTimeout(this.debounceTimer);
      this.debounceTimer = null;
    }
  }

  /// Currently-live config. Useful for "what's loaded right now" probes.
  getCurrent(): T | null {
    return this.current;
  }

  /// Public for explicit-call testing — bypass fs.watch.
  async reloadNow(): Promise<void> {
    await this.handleReload();
  }

  private async loadOnce(): Promise<T> {
    const raw = await readFile(this.path, 'utf8');
    const parsed = JSON.parse(raw) as unknown;
    return this.opts.parse(parsed);
  }

  private async handleReload(): Promise<void> {
    try {
      const next = await this.loadOnce();
      this.current = next;
      this.opts.onUpdate(next);
    } catch (e) {
      const err = e instanceof Error ? e : new Error(String(e));
      this.opts.onError?.(err);
    }
  }
}

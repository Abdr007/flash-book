// Telemetry — Prometheus-compatible metrics emitter for the bot and
// keeper suite.
//
// Two surfaces:
//   • In-process registry: counters / gauges / histograms, snapshot to
//     Prometheus text format.
//   • Pluggable sink: operators wire this to push-gateway (production),
//     stdout (dev), or no-op (testing).
//
// Stays minimal — no `prom-client` dep. The Prometheus text format is
// trivially small. Operators that want richer cardinality / quantiles
// can swap to prom-client by implementing the same `MetricsSink`.

export interface MetricLabels {
  readonly [key: string]: string;
}

export interface MetricsSink {
  /// Emit the current registry snapshot as Prometheus text. Caller is
  /// responsible for transport (HTTP push, file write, stdout).
  flush(text: string): void | Promise<void>;
}

export class StdoutSink implements MetricsSink {
  flush(text: string): void {
    // eslint-disable-next-line no-console
    console.log(text);
  }
}

export class NoopSink implements MetricsSink {
  flush(_text: string): void {}
}

/// HTTP push-gateway sink — operators wire this to a Prometheus
/// push-gateway endpoint or any HTTP receiver.
export class HttpPushSink implements MetricsSink {
  constructor(
    private readonly url: string,
    private readonly fetcher: typeof fetch = fetch,
  ) {}
  async flush(text: string): Promise<void> {
    await this.fetcher(this.url, {
      method: 'POST',
      headers: { 'content-type': 'text/plain' },
      body: text,
    });
  }
}

interface CounterEntry {
  name: string;
  help: string;
  labels: MetricLabels;
  value: number;
}

interface GaugeEntry {
  name: string;
  help: string;
  labels: MetricLabels;
  value: number;
}

/// In-process metric registry. Values are kept in JS Maps keyed by
/// `name|sorted-labels`. Snapshotting builds Prometheus text once per
/// flush.
export class MetricsRegistry {
  private counters: Map<string, CounterEntry> = new Map();
  private gauges: Map<string, GaugeEntry> = new Map();

  /// Bump a counter. Counters are monotonic — never decrement.
  inc(name: string, help: string, labels: MetricLabels = {}, by = 1): void {
    const key = this.metricKey(name, labels);
    const cur = this.counters.get(key);
    if (cur) {
      cur.value += by;
    } else {
      this.counters.set(key, { name, help, labels, value: by });
    }
  }

  /// Set a gauge — for state that goes up AND down (inventory, mark price).
  set(name: string, help: string, labels: MetricLabels, value: number): void {
    const key = this.metricKey(name, labels);
    this.gauges.set(key, { name, help, labels, value });
  }

  /// Render the current state as Prometheus text.
  render(): string {
    const lines: string[] = [];
    // Group by metric name so we emit one HELP / TYPE per name.
    const counterByName = new Map<string, CounterEntry[]>();
    for (const c of this.counters.values()) {
      if (!counterByName.has(c.name)) counterByName.set(c.name, []);
      counterByName.get(c.name)!.push(c);
    }
    for (const [name, entries] of counterByName) {
      lines.push(`# HELP ${name} ${entries[0]!.help}`);
      lines.push(`# TYPE ${name} counter`);
      for (const e of entries) {
        lines.push(`${name}${formatLabels(e.labels)} ${e.value}`);
      }
    }
    const gaugeByName = new Map<string, GaugeEntry[]>();
    for (const g of this.gauges.values()) {
      if (!gaugeByName.has(g.name)) gaugeByName.set(g.name, []);
      gaugeByName.get(g.name)!.push(g);
    }
    for (const [name, entries] of gaugeByName) {
      lines.push(`# HELP ${name} ${entries[0]!.help}`);
      lines.push(`# TYPE ${name} gauge`);
      for (const e of entries) {
        lines.push(`${name}${formatLabels(e.labels)} ${e.value}`);
      }
    }
    return lines.join('\n') + '\n';
  }

  /// Reset all metrics. Useful for tests.
  reset(): void {
    this.counters.clear();
    this.gauges.clear();
  }

  private metricKey(name: string, labels: MetricLabels): string {
    const sorted = Object.keys(labels).sort();
    const parts = sorted.map((k) => `${k}=${labels[k]}`);
    return `${name}|${parts.join(',')}`;
  }
}

function formatLabels(labels: MetricLabels): string {
  const keys = Object.keys(labels);
  if (keys.length === 0) return '';
  const parts = keys
    .sort()
    .map((k) => `${k}="${labels[k]!.replace(/\\/g, '\\\\').replace(/"/g, '\\"')}"`);
  return `{${parts.join(',')}}`;
}

/// Periodic flusher — calls registry.render() and pushes to sink at
/// interval. Owners (bot, keeper) typically start one of these per
/// process.
export class TelemetryFlusher {
  private timer: ReturnType<typeof setInterval> | null = null;

  constructor(
    private readonly registry: MetricsRegistry,
    private readonly sink: MetricsSink,
    private readonly intervalMs: number,
  ) {}

  start(): void {
    if (this.timer) return;
    this.timer = setInterval(() => {
      void this.flushOnce();
    }, this.intervalMs);
  }

  stop(): void {
    if (this.timer) {
      clearInterval(this.timer);
      this.timer = null;
    }
  }

  /// Manual flush — public for tests + final flush before shutdown.
  async flushOnce(): Promise<void> {
    const text = this.registry.render();
    await this.sink.flush(text);
  }
}

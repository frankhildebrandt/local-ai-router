import { useState, type ReactNode } from "react";
import type { UsageCandle } from "./types";

export function CandleLineChart({ candles, unit, empty, formatValue }: {
  candles: UsageCandle[];
  unit: string;
  empty?: ReactNode;
  formatValue?: (value: number) => string;
}) {
  const [hover, setHover] = useState<number | null>(null);
  if (!candles.length) return <>{empty}</>;
  const format = formatValue ?? ((value: number) => value.toFixed(value >= 10 ? 1 : 3));
  const pad = { l: 48, r: 16, t: 16, b: 32 };
  const width = 720;
  const height = 220;
  const innerW = width - pad.l - pad.r;
  const innerH = height - pad.t - pad.b;
  const min = Math.min(...candles.map(candle => candle.low));
  const max = Math.max(...candles.map(candle => candle.high));
  const span = max - min || 1;
  const y = (value: number) => pad.t + (1 - (value - min) / span) * innerH;
  const slot = innerW / candles.length;
  const x = (index: number) => pad.l + (index + 0.5) * slot;
  const bodyW = Math.max(3, Math.min(12, slot * 0.48));
  const line = candles.map((candle, index) => `${index === 0 ? "M" : "L"}${x(index).toFixed(1)} ${y(candle.avg).toFixed(1)}`).join(" ");
  const ticks = 4;
  const labelEvery = Math.max(1, Math.ceil(candles.length / 8));
  const active = hover != null ? candles[hover] : null;
  return (
    <div className="candle-chart-wrap">
      <svg className="candle-chart" viewBox={`0 0 ${width} ${height}`} role="img" aria-label={`${unit} over time`}>
        {Array.from({ length: ticks + 1 }, (_, index) => {
          const value = min + (span * index) / ticks;
          const py = y(value);
          return <g key={index}>
            <line className="candle-grid" x1={pad.l} x2={width - pad.r} y1={py} y2={py} />
            <text className="candle-axis" x={pad.l - 8} y={py + 3} textAnchor="end">{format(value)}</text>
          </g>;
        })}
        {candles.map((candle, index) => {
          const cx = x(index);
          const up = candle.close >= candle.open;
          const top = y(Math.max(candle.open, candle.close));
          const bottom = y(Math.min(candle.open, candle.close));
          const bodyH = Math.max(1.5, bottom - top);
          return <g key={candle.start} className={up ? "candle up" : "candle down"} onMouseEnter={() => setHover(index)} onMouseLeave={() => setHover(null)}>
            <rect className="candle-hit" x={cx - slot / 2} y={pad.t} width={slot} height={innerH} />
            <line x1={cx} x2={cx} y1={y(candle.high)} y2={y(candle.low)} />
            <rect x={cx - bodyW / 2} y={top} width={bodyW} height={bodyH} />
          </g>;
        })}
        <path className="candle-line" d={line} />
        {candles.map((candle, index) => index % labelEvery === 0 ? <text key={candle.start} className="candle-axis" x={x(index)} y={height - 10} textAnchor="middle">{formatCandleLabel(candle.start, candles.length)}</text> : null)}
      </svg>
      {active && <div className="candle-tooltip"><strong>{new Date(active.start).toLocaleString()}</strong><span>O {format(active.open)} · H {format(active.high)} · L {format(active.low)} · C {format(active.close)}</span><span>Avg {format(active.avg)} {unit}</span></div>}
    </div>
  );
}

function formatCandleLabel(start: string, count: number) {
  const date = new Date(start);
  if (count > 48) return date.toLocaleDateString(undefined, { month: "short", day: "numeric" });
  return date.toLocaleString(undefined, { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
}

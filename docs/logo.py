"""Generate docs/logo.svg: a CAN_H / CAN_L differential pair on a scope screen.

The wires share the recessive level and split symmetrically on dominant bits,
with a realistic slew and a little ringing on each edge. Edit the constants
and run:

    python3 docs/logo.py docs/logo.svg
"""
import math
import sys

BG, GRID, CAN_H, CAN_L = "#0d1412", "#7fd6b0", "#ffd166", "#4ee6ff"
STROKE = 12
X0, X1 = 64, 448                  # trace extent
MID, SWING = 256, 84              # recessive level, dominant excursion
BITS = [1, 0, 1, 0, 0, 1]         # 1 = recessive, 0 = dominant
IDLE = 40                         # recessive lead-in / tail (px)


def level(x):
    """0 = recessive, 1 = dominant, with slew and damped ringing after each edge."""
    bw = (X1 - X0 - 2 * IDLE) / len(BITS)
    i = int((x - X0 - IDLE) // bw)
    seq = [1] + BITS + [1]                      # idle before and after
    k = min(max(i + 1, 0), len(seq) - 1)
    t = x - (X0 + IDLE + (k - 1) * bw)          # time since this segment's edge
    cur, prev = 1 - seq[k], 1 - seq[k - 1] if k > 0 else 0
    if cur == prev or t < 0:
        return float(cur)
    return prev + (cur - prev) * (1 - math.exp(-t / 2.5) + math.exp(-t / 7) * math.sin(t / 2.6) * 0.11)


def trace(sign):
    xs = [X0 + k for k in range(X1 - X0 + 1)]
    return " ".join(f"{x},{MID + sign * SWING * level(x):.1f}" for x in xs)


grid = "".join(
    f'<line x1="{p}" y1="0" x2="{p}" y2="512"/><line x1="0" y1="{p}" x2="512" y2="{p}"/>' for p in (128, 384)
)
svg = f"""<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512" width="512" height="512">
  <title>canstudio</title>
  <defs>
    <filter id="glow" x="-10%" y="-20%" width="120%" height="140%">
      <feGaussianBlur stdDeviation="9" result="b"/>
      <feMerge><feMergeNode in="b"/><feMergeNode in="SourceGraphic"/></feMerge>
    </filter>
    <clipPath id="screen"><rect width="512" height="512" rx="112"/></clipPath>
  </defs>
  <rect width="512" height="512" rx="112" fill="{BG}"/>
  <g clip-path="url(#screen)" stroke="{GRID}" stroke-width="2">
    <g stroke-opacity="0.12">{grid}</g>
    <line x1="0" y1="256" x2="512" y2="256" stroke-opacity="0.22"/>
    <line x1="256" y1="0" x2="256" y2="512" stroke-opacity="0.22"/>
  </g>
  <g fill="none" stroke-width="{STROKE}" stroke-linejoin="round" stroke-linecap="round" filter="url(#glow)">
    <polyline points="{trace(1)}" stroke="{CAN_L}"/>
    <polyline points="{trace(-1)}" stroke="{CAN_H}"/>
  </g>
</svg>
"""
open(sys.argv[1], "w").write(svg)

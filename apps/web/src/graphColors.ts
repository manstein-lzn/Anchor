type Lab = readonly [number, number, number];

type Candidate = { css: string; lab: Lab };

const hueRgb = (hue: number, saturation: number, lightness: number): [number, number, number] => {
  const h = ((hue % 360) + 360) % 360 / 60;
  const s = saturation / 100;
  const l = lightness / 100;
  const chroma = (1 - Math.abs(2 * l - 1)) * s;
  const x = chroma * (1 - Math.abs(h % 2 - 1));
  const m = l - chroma / 2;
  const sector = Math.floor(h);
  const rgb = sector === 0 ? [chroma, x, 0]
    : sector === 1 ? [x, chroma, 0]
      : sector === 2 ? [0, chroma, x]
        : sector === 3 ? [0, x, chroma]
          : sector === 4 ? [x, 0, chroma] : [chroma, 0, x];
  return [(rgb[0] + m) * 255, (rgb[1] + m) * 255, (rgb[2] + m) * 255];
};

const linear = (value: number) => {
  const channel = value / 255;
  return channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4;
};

const oklab = (rgb: [number, number, number]): Lab => {
  const [red, green, blue] = rgb.map(linear);
  const l = Math.cbrt(0.4122214708 * red + 0.5363325363 * green + 0.0514459929 * blue);
  const m = Math.cbrt(0.2119034982 * red + 0.6806995451 * green + 0.1073969566 * blue);
  const s = Math.cbrt(0.0883024619 * red + 0.2817188376 * green + 0.6299787005 * blue);
  return [
    0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
    1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s,
    0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s,
  ];
};

const distance = (left: Lab, right: Lab) =>
  Math.hypot(left[0] - right[0], left[1] - right[1], left[2] - right[2]);

const candidates = (count: number): Candidate[] => {
  const hueCount = Math.max(120, Math.ceil(count / 6));
  const result: Candidate[] = [];
  for (let hueIndex = 0; hueIndex < hueCount; hueIndex += 1) {
    const hue = hueIndex * 360 / hueCount;
    for (const saturation of [58, 72]) {
      for (const lightness of [40, 52, 64]) {
        const rgb = hueRgb(hue, saturation, lightness);
        result.push({
          css: `hsl(${hue.toFixed(2)}, ${saturation}%, ${lightness}%)`,
          lab: oklab(rgb),
        });
      }
    }
  }
  return result;
};

/**
 * Assign one shared, deterministic palette to the current Graph set.
 * Greedy farthest-point selection in OKLab spreads colors perceptually instead of hashing each
 * name independently. Names are sorted so API ordering cannot change the assignment.
 */
export const assignGraphColors = (names: readonly string[]): Record<string, string> => {
  const ordered = [...new Set(names.filter(Boolean))].sort();
  if (!ordered.length) return {};

  const palette = candidates(ordered.length);
  const selected = [0];
  const nearest = palette.map(candidate => distance(candidate.lab, palette[0].lab));
  nearest[0] = -1;
  while (selected.length < ordered.length) {
    let best = -1;
    for (let index = 0; index < nearest.length; index += 1) {
      if (nearest[index] > (best < 0 ? -1 : nearest[best])) best = index;
    }
    selected.push(best);
    nearest[best] = -1;
    for (let index = 0; index < nearest.length; index += 1) {
      if (nearest[index] >= 0) nearest[index] = Math.min(nearest[index], distance(palette[index].lab, palette[best].lab));
    }
  }

  return Object.fromEntries(ordered.map((name, index) => [name, palette[selected[index]].css]));
};

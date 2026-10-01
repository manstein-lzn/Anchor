import { describe, expect, it } from 'vitest';
import { assignGraphColors } from './graphColors';

describe('timeline Graph colors', () => {
  it('assigns a deterministic, perceptually spread palette to the current Graph set', () => {
    const names = ['deep-academic-research', 'rsi', 'wecom-assistant', 'weekly-work-report'];
    const colors = assignGraphColors(names);
    expect(colors).toEqual(assignGraphColors([...names].reverse()));
    expect(new Set(Object.values(colors)).size).toBe(names.length);
    expect(colors.rsi).not.toBe(colors['deep-academic-research']);
    const hue = (color: string) => Number(color.match(/^hsl\((\d+\.\d+)/)?.[1]);
    expect(Math.abs(hue(colors.rsi) - hue(colors['deep-academic-research']))).toBeGreaterThan(30);
    expect(assignGraphColors([])).toEqual({});
    expect(assignGraphColors(['same', 'same'])).toEqual(assignGraphColors(['same']));
  });
});

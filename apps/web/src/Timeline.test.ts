import { describe, expect, it } from 'vitest';
import { graphColor } from './Timeline';

describe('timeline Graph colors', () => {
  it('generates stable distinct colors from Graph names', () => {
    expect(graphColor('assistant')).toBe(graphColor('assistant'));
    expect(graphColor('wecom-assistant')).not.toBe(graphColor('weekly-work-report'));
    const hue = (color: string) => Number(color.match(/^hsl\((\d+\.\d+)/)?.[1]);
    expect(Math.abs(hue(graphColor('wecom-assistant')) - hue(graphColor('weekly-work-report')))).toBeGreaterThan(30);
    expect(graphColor('assistant')).toMatch(/^hsl\(\d+\.\d{2}, 52%, 42%\)$/);
  });
});

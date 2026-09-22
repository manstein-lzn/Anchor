/** Grouping a workspace listing by directory.
 *
 * Small, and worth a test because of one case that is easy to get wrong: a file at the top of the
 * workspace has no directory, and giving it one named after itself would print the file's name as a
 * heading above the file.
 */

import { describe, expect, it } from 'vitest';
import { byDirectory } from './Files';

const file = (path: string, size = 1) => ({ path, size });

describe('grouping by directory', () => {
  it('keeps the top-level files in a group with no directory name', () => {
    expect(byDirectory([file('notes.md'), file('sources.md')])).toEqual([
      { directory: '', items: [file('notes.md'), file('sources.md')] },
    ]);
  });

  it('groups by the directory a file is actually in, not by the top of its path', () => {
    // Each group is a heading over the files under it, so it has to be the directory those files are
    // in. Folding `raw/ft` into `raw` would put a heading over files that are not directly under it.
    const groups = byDirectory([file('raw/a.json'), file('raw/ft/x.txt'), file('notes.md')]);

    expect(groups.map(group => [group.directory, group.items.map(item => item.path)])).toEqual([
      ['raw', ['raw/a.json']],
      ['raw/ft', ['raw/ft/x.txt']],
      ['', ['notes.md']],
    ]);
  });

  it('does not merge two directories that share a prefix', () => {
    const groups = byDirectory([file('raw/a.json'), file('raw2/b.json')]);

    expect(groups.map(group => group.directory)).toEqual(['raw', 'raw2']);
  });
});

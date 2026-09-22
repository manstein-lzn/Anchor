import { expect, it } from 'vitest';
import { fileTree } from './Files';

it('builds nested folders without losing root files or merging similar directory names', () => {
  const files = ['paper.md', 'raw/a.json', 'raw/full/text.md', 'raw2/b.json', 'empty-parent/deep/c.txt']
    .map(path => ({ path, size: 1 }));
  const root = fileTree(files);
  expect(root.files.map(file => file.path)).toEqual(['paper.md']);
  expect([...root.directories.keys()]).toEqual(['raw', 'raw2', 'empty-parent']);
  expect(root.directories.get('raw')?.files).toEqual([files[1]]);
  expect(root.directories.get('raw')?.directories.get('full')?.files).toEqual([files[2]]);
  expect(root.directories.get('raw2')?.files).toEqual([files[3]]);
  expect(root.directories.get('empty-parent')?.directories.get('deep')?.files).toEqual([files[4]]);
  expect(fileTree([]).files).toEqual([]);
});

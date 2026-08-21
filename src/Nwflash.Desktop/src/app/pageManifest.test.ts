import { expect, test } from 'vitest';
import { NWFLASH_APP_PAGES, PAGE_TITLES } from './pageManifest';

test('导航布局包含 10 个页面', () => {
  const pageCount = NWFLASH_APP_PAGES.reduce((sum, group) => sum + group.pages.length, 0);
  expect(pageCount).toBe(10);
});

test('每个页面都有中文标题', () => {
  for (const key of Object.keys(PAGE_TITLES)) {
    expect(PAGE_TITLES[key as keyof typeof PAGE_TITLES]).toBeTruthy();
  }
});

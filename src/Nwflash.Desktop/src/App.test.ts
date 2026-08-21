import { expect, test } from 'vitest';
import { APP_BRAND } from './app/App';

test('应用品牌文案固定为奶蛙Flash', () => {
  expect(APP_BRAND).toBe('奶蛙Flash');
});

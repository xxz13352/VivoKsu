import { describe, expect, it } from 'vitest';

import { errorMessage } from './error';

describe('errorMessage', () => {
  it('replaces backend diagnostics containing paths, URLs, or credentials', () => {
    const fallback = '分区操作失败，请重试';
    const message = errorMessage(
      '内部错误: 外部工具执行失败: C:\\Users\\private\\image.img https://api.github.com/x?token=secret token=secret',
      fallback,
    );

    expect(message).toBe(fallback);
    expect(message).not.toContain('private');
    expect(message).not.toContain('github');
    expect(message).not.toContain('secret');
  });

  it('keeps a fixed user-facing backend message', () => {
    expect(errorMessage('设备已变化，请重新读取分区表后再执行。', '分区操作失败')).toBe(
      '设备已变化，请重新读取分区表后再执行。',
    );
  });
});

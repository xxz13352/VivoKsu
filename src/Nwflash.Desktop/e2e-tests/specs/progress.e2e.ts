import assert from 'node:assert/strict';
import { authenticateE2eUser } from './authenticated-session';

describe('奶蛙Flash operation progress baseline', () => {
  beforeEach(async () => {
    await authenticateE2eUser();
  });

  it('shows the idle operation state before a command starts', async () => {
    const text = await $('body').getText();
    assert.match(text, /无进行中的操作/);
  });
});

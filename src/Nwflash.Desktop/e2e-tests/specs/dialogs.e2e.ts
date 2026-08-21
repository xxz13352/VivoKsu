import assert from 'node:assert/strict';
import { authenticateE2eUser } from './authenticated-session';

describe('奶蛙Flash dialog baseline', () => {
  beforeEach(async () => {
    await authenticateE2eUser();
  });

  it('keeps confirmation dialogs closed until an explicit prepare action', async () => {
    await $('aside[aria-label="主导航"] [data-page-id="QuickFlash"]').click();
    assert.equal(await $('[role="dialog"]').isExisting(), false);
  });
});

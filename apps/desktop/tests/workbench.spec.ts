import { expect, test } from '@playwright/test';
test('browser preview shows a real connection boundary and accessible workbench', async ({
  page,
}) => {
  await page.goto('/');
  await expect(page.getByRole('heading', { name: '从一个想法，到下一次提交。' })).toBeVisible();
  await expect(page.getByText('浏览器预览 · 在桌面应用中连接本地 Agent 和工作区')).toBeVisible();
  await expect(page.getByRole('button', { name: '发送任务' })).toBeDisabled();
  await page.getByRole('button', { name: '切换主题' }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await page.screenshot({ path: 'test-results/workbench-preview.png', fullPage: true });
});
test('opens workspace, creates native session, submits and approves exactly once', async ({
  page,
}) => {
  await page.goto('/tests/fixture.html');
  await page.getByRole('button', { name: '打开工作区', exact: true }).first().click();
  await page.getByRole('textbox', { name: '文件夹路径' }).fill('/fixture/test-project');
  await page.getByRole('dialog').getByRole('button', { name: '打开工作区', exact: true }).click();
  await page.getByRole('button', { name: '新建会话' }).click();
  await page.getByRole('button', { name: '连接 Agent' }).click();
  await page.getByRole('textbox', { name: '任务说明' }).fill('请测试这个项目');
  await page.getByRole('button', { name: '发送任务' }).click();
  await expect(page.getByText('Agent 请求你的批准')).toBeVisible();
  await expect(page.getByText('请测试这个项目', { exact: true }).first()).toBeVisible();
  await page.getByRole('button', { name: '批准一次' }).click();
  await expect(page.getByText('Agent 请求你的批准')).toHaveCount(0);
  await expect(page.getByText('完成了', { exact: false })).toBeVisible();
  const approvals = await page.evaluate(() =>
    (window as any).__requests.filter((request: any) => request.method === 'approve'),
  );
  expect(approvals).toHaveLength(1);
  expect(approvals[0].params.approval_id).toBe('host-token');
  await page.screenshot({ path: 'test-results/workbench-session.png', fullPage: true });
});

test('third-party UI uses a bound message port and cannot read host IPC authority', async ({
  page,
}) => {
  await page.goto('/tests/fixture.html?extensions=1');
  await page.getByRole('button', { name: 'Hello plugin' }).click();
  const frame = page.frameLocator('iframe[title="Hello plugin"]');
  await expect(frame.locator('#result')).toHaveText('Connected as hello');
  await frame.getByRole('textbox', { name: 'Message' }).fill('real isolated frame');
  await frame.getByRole('button', { name: 'Call WASM service' }).click();
  await expect(frame.locator('#result')).toContainText('real isolated frame');
  const child = page.frames().find((frame) => frame.parentFrame() !== null)!;
  const authority = await child.evaluate(() => {
    try {
      return String((window.parent as any).__VIRTADE_HOST_TOKEN__);
    } catch (error) {
      return (error as Error).name;
    }
  });
  expect(authority).toBe('SecurityError');
  await child.evaluate(() =>
    window.parent.postMessage(
      {
        type: 'virt:request',
        id: 999,
        request: { method: 'approve', params: { approval_id: 'stolen' } },
      },
      '*',
    ),
  );
  expect(
    await page.evaluate(() =>
      (window as any).__requests.some((request: any) => request.method === 'approve'),
    ),
  ).toBe(false);
  const messages = await page.evaluate(() =>
    (window as any).__requests.filter(
      (request: any) =>
        request.method === 'plugin_request' && request.params.request.method === 'service_call',
    ),
  );
  expect(messages[0].params.id).toBe('hello');
  expect(messages[0].params.generation).toBe('generation-1');
});

test('Monaco loads local workers and saves the selected file with its expected version', async ({
  page,
}) => {
  await page.goto('/tests/fixture.html');
  await page.getByRole('button', { name: '打开工作区', exact: true }).first().click();
  await page.getByRole('textbox', { name: '文件夹路径' }).fill('/fixture/project');
  await page.getByRole('dialog').getByRole('button', { name: '打开工作区', exact: true }).click();
  await page.getByRole('button', { name: '文件浏览', exact: true }).click();
  await page.getByRole('button', { name: 'main.rs', exact: true }).click();
  await expect(page.locator('.monaco-editor')).toBeVisible();
  await expect(page.locator('.view-lines')).toContainText('fn main()');
  await page.locator('.monaco-editor').click();
  await page.keyboard.press('ControlOrMeta+End');
  await page.keyboard.type('// edited');
  await page.keyboard.press('ControlOrMeta+s');
  await expect
    .poll(async () =>
      page.evaluate(
        () =>
          (window as any).__requests.filter(
            (request: any) =>
              request.method === 'workbench' && request.params.request.operation === 'write_file',
          ).length,
      ),
    )
    .toBe(1);
  const write = await page.evaluate(() =>
    (window as any).__requests.find(
      (request: any) =>
        request.method === 'workbench' && request.params.request.operation === 'write_file',
    ),
  );
  expect(write.params.request.params.expected).toBe('fixture-version');
  expect(write.params.request.params.path).toBe('main.rs');
  expect(write.params.request.params.content).toContain('// edited');
});

test('late Git response from a previous workspace cannot replace the selected workspace', async ({
  page,
}) => {
  await page.goto('/tests/fixture.html?git-race=1');
  await page.getByRole('button', { name: 'old-project' }).click();
  await page.getByRole('button', { name: 'Git 变更', exact: true }).click();
  await expect
    .poll(() =>
      page.evaluate(() =>
        (window as any).__requests.some(
          (r: any) =>
            r.method === 'workbench' &&
            r.params.workspace_id === 'old' &&
            r.params.request.operation === 'git_status',
        ),
      ),
    )
    .toBe(true);
  await page.getByRole('button', { name: 'new-project' }).click();
  await expect(page.getByRole('button', { name: 'M new.txt', exact: true })).toBeVisible();
  await expect.poll(() => page.evaluate(() => (window as any).__oldGitComplete)).toBe(true);
  await expect(page.getByRole('button', { name: 'M old.txt', exact: true })).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'M new.txt', exact: true })).toBeVisible();
});

test('native task failure is visible with its preserved provider error', async ({ page }) => {
  await page.goto('/tests/fixture.html?failure=1');
  await page.getByRole('button', { name: '打开工作区', exact: true }).first().click();
  await page.getByRole('textbox', { name: '文件夹路径' }).fill('/fixture/test-project');
  await page.getByRole('dialog').getByRole('button', { name: '打开工作区', exact: true }).click();
  await page.getByRole('button', { name: '新建会话' }).click();
  await page.getByRole('button', { name: '连接 Agent' }).click();
  await page.getByRole('textbox', { name: '任务说明' }).fill('error fixture');
  await page.getByRole('button', { name: '发送任务' }).click();
  await page.getByRole('button', { name: '批准一次' }).click();
  await expect(page.getByText('Agent 报告任务失败')).toBeVisible();
  await expect(page.getByText('fixture expired authentication', { exact: false })).toBeVisible();
});

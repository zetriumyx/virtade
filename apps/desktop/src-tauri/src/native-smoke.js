// Runs inside the actual Tauri WebKit/WebView document, never a browser fixture.
(async () => {
  const invoke = window.__TAURI_INTERNALS__.invoke;
  const headers = { 'x-virtade-host': window.__VIRTADE_HOST_TOKEN__ };
  const request = request => invoke('desktop_request', { version: 1, request }, { headers });
  const checks = [];
  const assert = (condition, name) => { if (!condition) throw new Error(name); checks.push(name); };
  try {
    const deadline = Date.now() + 15000;
    while (!document.querySelector('button[aria-label="切换主题"]')) {
      if (Date.now() > deadline) throw new Error('React workbench did not mount');
      await new Promise(resolve => setTimeout(resolve, 100));
    }
    assert(document.querySelector('#root').textContent.includes('virtade'), 'React workbench rendered');
    const snapshot = await request({ method: 'snapshot' });
    assert(Array.isArray(snapshot.workspaces) && Array.isArray(snapshot.contributions), 'native SQLite host snapshot');
    assert(snapshot.contributions.some(([, view]) => view.spec.type === 'builtin'), 'virt built-in panels registered');
    let denied = false;
    try { await invoke('desktop_request', { version: 1, request: { method: 'snapshot' } }); }
    catch (_) { denied = true; }
    assert(denied, 'IPC without host nonce rejected');
    await request({ method: 'save_settings', params: { settings: { ...snapshot.settings, dsh_executable: 'native-smoke-marker' } } });
    assert((await request({ method: 'snapshot' })).settings.dsh_executable === 'native-smoke-marker', 'typed IPC persistence round trip');
    const diagnostics = await request({ method: 'diagnostics' });
    assert(diagnostics.schema_version === 1 && diagnostics.resources.services > 0 && !('settings' in diagnostics), 'structured diagnostics exclude private settings');
    const beforeTheme = document.documentElement.dataset.theme;
    document.querySelector('button[aria-label="切换主题"]').click();
    await new Promise(resolve => setTimeout(resolve, 100));
    assert(['dark', 'light'].includes(document.documentElement.dataset.theme) && document.documentElement.dataset.theme !== beforeTheme, 'native WebView interactive theme');
    const beforeNavigation = location.href;
    location.assign('https://blocked-navigation.invalid/');
    await new Promise(resolve => setTimeout(resolve, 300));
    assert(location.href === beforeNavigation, 'external top-level navigation rejected');
    await invoke('native_smoke_report', { result: { ok: true, checks, userAgent: navigator.userAgent } }, { headers });
  } catch (error) {
    await invoke('native_smoke_report', { result: { ok: false, checks, error: String(error) } }, { headers });
  }
})();
